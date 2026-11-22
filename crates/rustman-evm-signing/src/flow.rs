//! One `evm-tx` request through the whole gate, for `rustman evm send`
//! and `rustman run --sign` alike. The preconditions are checked in order
//! and each refusal exits with code 4:
//!
//! 1. `[evm] allow_signing = true`;
//! 2. no key material as a signer value; the preview is shown (naming the
//!    signer only when it is a configured id);
//! 3. `--sign` (without it the preview is all that happens);
//! 4. a signer id from `--signer`, `RUSTMAN_EVM_SIGNER` or the request's
//!    `signer`, a plain identifier configured in `[evm.signers]`;
//! 5. the environment pins `chain_id` and the node's `eth_chainId` matches;
//! 6. a confirmation is possible ([`confirm::mode`]: a terminal, or
//!    `--yes-local` on a local chain), before any key is unlocked;
//! 7. the signer unlocks (keystore password, keychain entry);
//! 8. the simulation (`eth_call` from the signer at `pending` and
//!    `eth_estimateGas`) succeeds, unless `--force-revert`;
//! 9. the user confirms the decoded summary.
//!
//! Then the chain id is checked once more, the transaction is signed and
//! sent, and the receipt is awaited. Its assertions and captures run on the
//! receipt, and history keeps an audit entry without the signed bytes or
//! any key material, also when the receipt never came or could not be
//! read, and when the send itself failed (`broadcast-unknown`, with the
//! hash computed from the signed bytes). Ctrl+C before the transaction is sent stops with exit code 130
//! and nothing signed or sent: the console's prompt is interrupted, and
//! the flow checks again after the confirmation and right before sending.
//!
//! A [`Signing`] keeps the nonces of one run, so consecutive transactions
//! of a signer use increasing nonces, and signs one transaction at a time
//! (prompts never interleave).

use std::sync::Arc;
use std::time::Instant;

use alloy_json_abi::{Function, JsonAbi};
use alloy_primitives::B256;
use rustman_core::Secret;
use rustman_core::model::{EvmRequest, EvmTxRequest, Request, unlocated};
use rustman_core::run::{RequestRecord, RequestStatus, SkipReason};
use rustman_core::secrets::Keychain;
use rustman_core::text::printable_line;
use rustman_core::vars::ResolvedEvmTx;
use rustman_evm::abi::AbiCache;
use rustman_evm::call::{CallInputs, CallRequest, prepare};
use rustman_evm::codec::decode_input;
use rustman_evm::ens::{ens_for, is_ens_name};
use rustman_evm::exec::{EvmRunner, error_kind};
use rustman_evm::identity::Pin;
use rustman_evm::quantity::Denomination;
use rustman_evm::tx::TokenCache;
use rustman_evm::{ChainInfo, ChainRegistry, EvmError, EvmSettings, JsonrpcTransport};
use rustman_http::HttpEngine;
use rustman_http::runner::{EvmOutcome, EvmTxCall, EvmTxExecutor, EvmTxFuture, evm_error_record};
use serde_json::Value;
use zeroize::Zeroizing;

use crate::config::{SignerConfig, SignerSource, Signers};
use crate::confirm::{self, Confirmation, Mode, Summary};
use crate::display::{Preview, render_call, signer_label, token_amount_input};
use crate::fees::{FeeInputs, fees, gas_limit};
use crate::gate::{
    Simulation, check_chain, check_enabled, check_sign_flag, check_simulation, require_pin,
    select_signer, simulate,
};
use crate::nonce::Nonces;
use crate::rawkey::refuse_raw_key;
use crate::receipt::{self, Mined};
use crate::send::{Broadcasted, Wait, Waited, explorer_url, sign_and_send, wait_for_receipt};

/// The error when Ctrl+C stops the gate before anything is sent.
pub const INTERRUPTED: &str = "interrupted; nothing was signed or sent";
use crate::signer::{UnlockContext, UnlockedSigner, unlock};
use crate::transaction::UnsignedTx;

/// What the invocation asked for.
#[derive(Clone, Debug, Default)]
pub struct SignOptions {
    /// `--sign`: without it only the preview is shown.
    pub sign: bool,
    /// `--signer`, else `RUSTMAN_EVM_SIGNER`; over the request's `signer`.
    pub signer: Option<String>,
    pub force_revert: bool,
    pub yes_local: bool,
    pub wait: Wait,
}

/// Where the gate talks to the user.
pub trait Console: Sync {
    /// Whether there is a terminal to confirm on.
    fn interactive(&self) -> bool;
    /// The preview, the summary, the hash and the receipt. Already
    /// redacted and escaped.
    fn show(&self, text: &str);
    /// A warning or a note, apart from the output (stderr). Already
    /// redacted and escaped.
    fn note(&self, text: &str);
    /// Ask `question` and read one line (empty when there is no answer);
    /// `None` when Ctrl+C interrupted the prompt.
    fn ask(&self, question: &str) -> Option<String>;
    /// A no-echo password prompt; `Ok(None)` without a terminal, the
    /// reason when the terminal could not be read.
    fn password(&self, question: &str) -> Result<Option<Zeroizing<String>>, String>;
    /// Whether Ctrl+C was pressed.
    fn cancelled(&self) -> bool;
}

/// How a sent transaction ended.
#[derive(Debug)]
pub enum Ending {
    /// Mined: the checked record, and whether the transaction succeeded
    /// on chain.
    Mined {
        record: RequestRecord,
        succeeded: bool,
    },
    /// Sent, but not mined within the receipt timeout, or its receipt
    /// could not be read: the record carries the error, `explorer` the
    /// transaction's page. The audit entry is kept either way.
    Unconfirmed {
        record: RequestRecord,
        explorer: Option<String>,
    },
}

/// The signing gate of one invocation or one run.
pub struct Signing<'c> {
    options: SignOptions,
    console: &'c dyn Console,
    keychain: Option<Box<dyn Keychain>>,
    engine: EvmRunner,
    nonces: Nonces,
    one_at_a_time: tokio::sync::Mutex<()>,
}

/// The node, its chain and what the request resolved to.
struct Context<'a> {
    settings: EvmSettings,
    tx: ResolvedEvmTx,
    rpc: JsonrpcTransport,
    registry: ChainRegistry,
    chain: ChainInfo,
    pin: Pin<'a>,
    abi: JsonAbi,
    function: Function,
}

impl<'c> Signing<'c> {
    pub fn new(
        options: SignOptions,
        console: &'c dyn Console,
        keychain: Option<Box<dyn Keychain>>,
    ) -> Self {
        Self {
            options,
            console,
            keychain,
            engine: EvmRunner::new(Arc::new(HttpEngine::new())),
            nonces: Nonces::new(),
            one_at_a_time: tokio::sync::Mutex::new(()),
        }
    }

    /// Take `call` through the gate. A refusal is an error; once the
    /// transaction is sent, how it ended.
    pub async fn send(&self, call: &EvmTxCall<'_>) -> Result<Ending, EvmError> {
        let _one = self.one_at_a_time.lock().await;
        let (ctx, config, id) = self.preconditions(call).await?;
        let signer = self.unlock(call, config).await?;
        self.check_interrupted()?;
        let request = call_request(&ctx, &signer).await?;
        let simulation = simulate(&ctx.rpc, &request, &ctx.function, Some(&ctx.abi)).await?;
        check_simulation(&simulation, self.options.force_revert)?;
        let summary = self
            .summarize(call, &ctx, &id, &signer, &request, &simulation)
            .await?;
        self.console.show(&shown(call, &summary.render()));
        let confirmation = self.confirm(&summary, &ctx.settings)?;
        self.check_interrupted()?;
        // The chain is asked once more, right before signing.
        check_chain(&ctx.rpc, &ctx.registry, ctx.pin).await?;
        self.check_interrupted()?;
        let broadcasted = sign_and_send(&summary.tx, &signer, confirmation, &ctx.rpc).await?;
        // The nonce counts as used even when the send failed: the
        // transaction may be out, and reusing its nonce could replace it.
        self.nonces
            .sent(summary.tx.chain_id, signer.address(), summary.tx.nonce);
        drop(signer);
        let hash = broadcasted.hash();
        let failure = match broadcasted {
            Broadcasted::Accepted(_) => {
                self.console.show(&format!("sent {hash}"));
                None
            }
            Broadcasted::Unknown { reason, .. } => {
                self.console.note(&shown(
                    call,
                    &format!(
                        "warning: {reason}; transaction {hash} may have been sent; waiting for its receipt"
                    ),
                ));
                Some(reason)
            }
        };
        Ok(self
            .receipt(call, &ctx, &summary, hash, failure.as_deref())
            .await)
    }

    /// Ctrl+C was pressed: stop before anything is sent.
    fn check_interrupted(&self) -> Result<(), EvmError> {
        match self.console.cancelled() {
            true => Err(EvmError::Interrupted(INTERRUPTED.to_owned())),
            false => Ok(()),
        }
    }

    /// Steps 1-6: the workspace opt-in, the preview, `--sign`, the signer,
    /// the chain pin, and whether the user can confirm at all.
    async fn preconditions<'a>(
        &self,
        call: &'a EvmTxCall<'_>,
    ) -> Result<(Context<'a>, SignerConfig, String), EvmError> {
        let settings = EvmSettings::from_table(&call.settings.evm)?;
        check_enabled(&settings)?;
        let (tx, options) = call
            .runner
            .resolve_evm_tx(call.request)
            .await
            .map_err(EvmError::Config)?;
        let flag = self.options.signer.as_deref();
        for value in [flag, tx.signer.as_deref()].into_iter().flatten() {
            refuse_raw_key(value)?;
        }
        let (abi, function) = abi_and_function(call)?;
        self.show_preview(call, &settings, &tx, &function);
        check_sign_flag(self.options.sign)?;
        let id = select_signer(flag, tx.signer.as_deref())?.to_owned();
        let config = Signers::from_settings(&settings)?.get(&id)?.clone();
        let environment = call.environment;
        let pin = require_pin(
            environment.map(|e| e.name.as_str()),
            environment.and_then(|e| e.chain_id),
        )?;
        let rpc = self.engine.transport(tx.endpoint.clone(), &options);
        let registry = ChainRegistry::with_overrides(&settings)?;
        let chain = check_chain(&rpc, &registry, pin).await?;
        // A send that cannot be confirmed (no terminal on a non-local
        // chain) is refused before the key is unlocked.
        let interactive = self.console.interactive();
        confirm::mode(&chain, &settings, interactive, self.options.yes_local)?;
        let ctx = Context {
            settings,
            tx,
            rpc,
            registry,
            chain,
            pin,
            abi,
            function,
        };
        Ok((ctx, config, id))
    }

    /// The preview, before `--sign` is checked. The signer is named only
    /// when it is a configured id.
    fn show_preview(
        &self,
        call: &EvmTxCall<'_>,
        settings: &EvmSettings,
        tx: &ResolvedEvmTx,
        function: &Function,
    ) {
        let signers = Signers::from_settings(settings).unwrap_or_default();
        let named = self.options.signer.as_deref().or(tx.signer.as_deref());
        let id = call.request.entry.id();
        let preview = Preview {
            id: &id,
            to: &tx.to,
            function: &function.name,
            args: &tx.args,
            value: tx.value.as_deref(),
            signer: signer_label(named, |id| signers.contains(id)),
        };
        let redactor = call.runner.known_secrets();
        let text = preview.render(|text| redactor.text(text).into_owned());
        self.console.show(&text);
    }

    /// Unlock `config`: its keystore password is resolved (a secret) or
    /// prompted on a terminal.
    async fn unlock(
        &self,
        call: &EvmTxCall<'_>,
        config: SignerConfig,
    ) -> Result<UnlockedSigner, EvmError> {
        let password = match &config.source {
            SignerSource::Keystore {
                password: Some(text),
                ..
            } => {
                let name = format!("{}_password", config.id);
                let secret = call
                    .runner
                    .resolve_evm_secret(call.request, &name, text)
                    .await
                    .map_err(EvmError::Config)?;
                // Moved, not copied, into a zeroizing holder.
                Some(Secret::new(name, Zeroizing::new(secret.into_inner())))
            }
            _ => None,
        };
        let prompt = |question: &str| self.console.password(question);
        let ctx = UnlockContext {
            root: call.root,
            password,
            prompt: &prompt,
            keychain: self.keychain.as_deref(),
        };
        let unlocked = unlock(&config, &ctx);
        self.check_interrupted()?;
        unlocked
    }

    /// The transaction to sign (gas, fees, nonce) and its summary.
    async fn summarize(
        &self,
        call: &EvmTxCall<'_>,
        ctx: &Context<'_>,
        id: &str,
        signer: &UnlockedSigner,
        request: &CallRequest,
        simulation: &Simulation,
    ) -> Result<Summary, EvmError> {
        let reverted = simulation.revert();
        if let Some(revert) = &reverted {
            self.console.note(&shown(
                call,
                &format!("warning: SIMULATION REVERTED: {revert}"),
            ));
        }
        let native = Denomination::native(&ctx.chain);
        let gas = gas_limit(ctx.tx.gas.as_deref(), simulation.gas_estimate(), &native)?;
        let inputs = FeeInputs {
            tx_type: tx_request(call)?.tx_type.unwrap_or_default(),
            max_fee: ctx.tx.max_fee.as_deref(),
            max_priority_fee: ctx.tx.max_priority_fee.as_deref(),
            gas_price: ctx.tx.gas_price.as_deref(),
        };
        let fees = fees(&ctx.rpc, &inputs, &native).await?;
        let explicit = ctx.tx.nonce.as_deref();
        let nonce = self
            .nonces
            .next(&ctx.rpc, ctx.chain.id, signer.address(), explicit)
            .await?;
        let tx = UnsignedTx {
            chain_id: ctx.chain.id,
            nonce,
            to: request.to,
            value: request.value.unwrap_or_default(),
            input: request.data.clone(),
            gas_limit: gas,
            fees,
        };
        let decoded = decoded_call(ctx, &tx).await;
        Ok(Summary {
            chain: ctx.chain.clone(),
            signer_id: id.to_owned(),
            signer: signer.address(),
            to_name: is_ens_name(&ctx.tx.to).then(|| ctx.tx.to.clone()),
            call: decoded,
            tx,
            reverted,
        })
    }

    /// Step 8: the user's confirmation of `summary`.
    fn confirm(&self, summary: &Summary, settings: &EvmSettings) -> Result<Confirmation, EvmError> {
        let interactive = self.console.interactive();
        let mode = confirm::mode(
            &summary.chain,
            settings,
            interactive,
            self.options.yes_local,
        )?;
        if mode == Mode::LocalHeadless {
            let label = summary.chain.label();
            self.console.note(&format!(
                "confirmed by --yes-local on {}",
                printable_line(&label)
            ));
            return Confirmation::local_headless(summary, settings);
        }
        if self.options.yes_local {
            self.console.note(
                "note: --yes-local applies only to chain 31337 and [evm] headless_signing_chains",
            );
        }
        let answer = self.ask("sign and send this transaction? [y/N] ")?;
        let typed = match (&mode, confirm::is_yes(&answer)) {
            (Mode::Typed(name), true) => Some(self.ask(&chain_name_question(name))?),
            _ => None,
        };
        // Ctrl+C while the answer was typed still wins.
        self.check_interrupted()?;
        Confirmation::interactive(summary, settings, &answer, typed.as_deref())
    }

    /// Ask the console; Ctrl+C at the prompt is an interruption.
    fn ask(&self, question: &str) -> Result<String, EvmError> {
        self.check_interrupted()?;
        self.console
            .ask(question)
            .ok_or_else(|| EvmError::Interrupted(INTERRUPTED.to_owned()))
    }

    /// Wait for the receipt, check it and keep the audit entry. The
    /// transaction is signed and its send attempted (`failure` when the
    /// send failed): however the wait ends, the audit entry is written.
    async fn receipt(
        &self,
        call: &EvmTxCall<'_>,
        ctx: &Context<'_>,
        summary: &Summary,
        hash: B256,
        failure: Option<&str>,
    ) -> Ending {
        let sent = Sent {
            call,
            ctx,
            summary,
            hash,
            failure,
            started: Instant::now(),
        };
        let timeout = seconds(self.options.wait.timeout);
        let receipt = match wait_for_receipt(&ctx.rpc, hash, &self.options.wait).await {
            Waited::Mined(receipt) => receipt,
            waited => return sent.not_mined(waited, &timeout),
        };
        let (mined, outcome) = match sent.checked(&receipt).await {
            Ok(checked) => checked,
            Err(error) => {
                let message = format!(
                    "transaction {hash} was mined but its receipt could not be read: {error}"
                );
                return sent.unconfirmed(message, receipt::audit_receipt_unknown(summary, hash));
            }
        };
        self.console
            .show(&shown(call, mined.report.render().trim_end()));
        if let Some(url) = explorer_url(&summary.chain, hash) {
            self.console.show(&format!("explorer: {url}"));
        }
        let record = call
            .runner
            .finish_evm_tx(call.request, &ctx.tx.endpoint, outcome);
        Ending::Mined {
            record,
            succeeded: mined.succeeded(),
        }
    }
}

/// A sent transaction whose receipt is awaited.
struct Sent<'s, 'c, 'a> {
    call: &'s EvmTxCall<'c>,
    ctx: &'s Context<'a>,
    summary: &'s Summary,
    hash: B256,
    /// Why the send failed, when it did (the broadcast is unknown).
    failure: Option<&'s str>,
    started: Instant,
}

impl Sent<'_, '_, '_> {
    /// The mined transaction decoded, and the outcome its assertions and
    /// captures are checked on.
    async fn checked(&self, receipt: &Value) -> Result<(Mined, EvmOutcome), EvmError> {
        let ctx = self.ctx;
        let mined = receipt::decode(&ctx.rpc, self.summary, self.hash, &ctx.abi, receipt).await?;
        let mut outcome = outcome(self.call, self.summary, self.hash, &mined, self.started)?;
        if let Some(failure) = self.failure {
            outcome
                .warnings
                .push(format!("{failure}; the transaction was mined all the same"));
        }
        Ok((mined, outcome))
    }

    /// No receipt within `timeout`: the error and the audit entry,
    /// `broadcast-unknown` when the send itself failed.
    fn not_mined(&self, waited: Waited, timeout: &str) -> Ending {
        let (hash, summary) = (self.hash, self.summary);
        let polled = match waited {
            Waited::Unknown(error) => Some(error),
            _ => None,
        };
        if let Some(failure) = self.failure {
            let last = polled.map_or_else(String::new, |e| format!("; last receipt poll: {e}"));
            let message = format!(
                "transaction {hash} may have been sent ({failure}) but no receipt came within {timeout}{last}; check the explorer for {hash} before sending it again"
            );
            return self.unconfirmed(message, receipt::audit_broadcast_unknown(summary, hash));
        }
        match polled {
            Some(error) => {
                let message = format!(
                    "transaction {hash} was sent but its receipt could not be read within {timeout}: {error}"
                );
                self.unconfirmed(message, receipt::audit_receipt_unknown(summary, hash))
            }
            None => {
                let message = format!("transaction {hash} was sent but not mined within {timeout}");
                self.unconfirmed(message, receipt::audit(summary, hash, None))
            }
        }
    }

    /// No receipt to check (timed out, or unreadable): the error
    /// `message` and the explorer link, and the audit entry `audit`, kept
    /// since the transaction was sent.
    fn unconfirmed(&self, message: String, audit: Value) -> Ending {
        let outcome = EvmOutcome {
            document: Value::Null,
            evm_assertions: Vec::new(),
            error: Some(message),
            warnings: Vec::new(),
            record: audit,
            elapsed: self.started.elapsed(),
        };
        let record =
            self.call
                .runner
                .finish_evm_tx(self.call.request, &self.ctx.tx.endpoint, outcome);
        Ending::Unconfirmed {
            record,
            explorer: explorer_url(&self.summary.chain, self.hash),
        }
    }
}

impl EvmTxExecutor for Signing<'_> {
    /// `rustman run --sign`: a refusal or an error is the request's
    /// record (a refusal makes the run exit with code 4); a transaction
    /// that reverted on chain fails.
    fn execute<'a>(&'a self, call: EvmTxCall<'a>) -> EvmTxFuture<'a> {
        Box::pin(async move {
            match self.send(&call).await {
                Ok(Ending::Mined { record, succeeded }) => reverted_fails(record, succeeded),
                Ok(Ending::Unconfirmed { record, explorer }) => with_explorer(record, explorer),
                Err(EvmError::Interrupted(_)) => call.record.skipped(SkipReason::Interrupted),
                Err(error) => refusal_record(&call, &error),
            }
        })
    }
}

/// A mined transaction that reverted on chain fails its request.
fn reverted_fails(mut record: RequestRecord, succeeded: bool) -> RequestRecord {
    if !succeeded && record.status == RequestStatus::Passed {
        record.status = RequestStatus::Failed;
        record
            .warnings
            .push("the transaction reverted on chain".to_owned());
    }
    record
}

/// An unconfirmed transaction's record names its explorer page.
fn with_explorer(mut record: RequestRecord, explorer: Option<String>) -> RequestRecord {
    if let Some(url) = explorer {
        record.warnings.push(format!("explorer: {url}"));
    }
    record
}

/// The record of a request the gate refused or could not prepare.
fn refusal_record(call: &EvmTxCall<'_>, error: &EvmError) -> RequestRecord {
    let message = call
        .runner
        .known_secrets()
        .text(&error.to_string())
        .into_owned();
    let mut record = call.record.clone().errored(message.clone());
    record.evm = Some(evm_error_record(error_kind(error), &message));
    record
}

/// `text` redacted with every secret of the run, and escaped (newlines
/// kept: multi-line output such as the summary, each of whose values is
/// already on its line).
fn shown(call: &EvmTxCall<'_>, text: &str) -> String {
    let redactor = call.runner.known_secrets();
    rustman_core::text::printable(&redactor.text(text)).into_owned()
}

/// The typed-name question of a protected chain, on one line.
fn chain_name_question(name: &str) -> String {
    format!(
        "type the chain name ({}) to confirm: ",
        printable_line(name)
    )
}

/// The call the summary shows: decoded with the request's ABI, ERC-20
/// amounts in token units when the token's decimals are known.
async fn decoded_call(ctx: &Context<'_>, tx: &UnsignedTx) -> String {
    let Some(decoded) = decode_input(&ctx.abi, &tx.input) else {
        return ctx.function.signature();
    };
    let Some(index) = token_amount_input(&decoded.function) else {
        return decoded.to_string();
    };
    let token = TokenCache::new().get(&ctx.rpc, ctx.chain.id, tx.to).await;
    render_call(&decoded, Some((index, &token)))
}

/// The call the transaction makes, from the signer.
async fn call_request(ctx: &Context<'_>, signer: &UnlockedSigner) -> Result<CallRequest, EvmError> {
    let inputs = CallInputs {
        to: ctx.tx.to.clone(),
        from: Some(signer.address().to_checksum(None)),
        value: ctx.tx.value.clone(),
        args: ctx.tx.args.clone(),
        block: Some("pending".to_owned()),
        state_overrides: None,
    };
    let ens = ens_for(&ctx.rpc, ctx.chain.id, None).ok();
    let native = Denomination::native(&ctx.chain);
    prepare(&inputs, &ctx.function, &native, ens.as_ref()).await
}

fn outcome(
    call: &EvmTxCall<'_>,
    summary: &Summary,
    hash: B256,
    mined: &Mined,
    started: Instant,
) -> Result<EvmOutcome, EvmError> {
    let assertions = unlocated(tx_request(call)?.assertions.as_deref());
    let native = Denomination::native(&summary.chain);
    Ok(EvmOutcome {
        document: receipt::document(mined),
        evm_assertions: receipt::evm_assertions(&assertions, mined, &native),
        error: None,
        warnings: mined.report.warnings.clone(),
        record: receipt::audit(summary, hash, Some(mined)),
        elapsed: started.elapsed(),
    })
}

/// The `evm-tx` request as written.
fn tx_request<'a>(call: &'a EvmTxCall<'_>) -> Result<&'a EvmTxRequest, EvmError> {
    match call.request.request() {
        Request::Evm(evm) => match evm.as_ref() {
            EvmRequest::Tx(tx) => Ok(tx),
            EvmRequest::Call(_) => Err(EvmError::config("not an evm-tx request")),
        },
        _ => Err(EvmError::config("not an evm-tx request")),
    }
}

/// The request's ABI (from the workspace) and function.
fn abi_and_function(call: &EvmTxCall<'_>) -> Result<(JsonAbi, Function), EvmError> {
    let Request::Evm(evm) = call.request.request() else {
        return Err(EvmError::config("not an evm-tx request"));
    };
    let path = &call.request.entry.path;
    let checked = rustman_evm::load::check(call.root, path, evm, &AbiCache::new())
        .map_err(|e| EvmError::config(e.to_string()))?;
    match (checked.abi, checked.function) {
        (Some(abi), Some(function)) => Ok((abi.abi.as_ref().clone(), function)),
        (None, _) => Err(EvmError::config(
            "evm-tx: an explorer ABI is not supported for signing; use a built-in or workspace ABI",
        )),
        (_, None) => Err(EvmError::config("evm-tx: 'function' is required")),
    }
}

/// `120s`, or `250ms` for a time that is not whole seconds.
pub fn seconds(duration: std::time::Duration) -> String {
    match duration.as_millis() {
        ms if ms % 1000 == 0 => format!("{}s", ms / 1000),
        ms => format!("{ms}ms"),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use alloy_primitives::{Address, U256};
    use rustman_evm::ChainRegistry;

    use super::*;
    use crate::fees::{Fee, Fees};

    /// A terminal that answers `answer` to every question; with
    /// `ctrl_c`, Ctrl+C is pressed while the user types it.
    struct Terminal {
        answer: Option<&'static str>,
        ctrl_c: bool,
        cancelled: AtomicBool,
        asked: Mutex<Vec<String>>,
    }

    impl Terminal {
        fn new(answer: Option<&'static str>, ctrl_c: bool) -> Self {
            Self {
                answer,
                ctrl_c,
                cancelled: AtomicBool::new(false),
                asked: Mutex::new(Vec::new()),
            }
        }
    }

    impl Console for Terminal {
        fn interactive(&self) -> bool {
            true
        }
        fn show(&self, _: &str) {}
        fn note(&self, _: &str) {}
        fn ask(&self, question: &str) -> Option<String> {
            self.asked.lock().unwrap().push(question.to_owned());
            if self.ctrl_c {
                self.cancelled.store(true, Ordering::SeqCst);
            }
            self.answer.map(str::to_owned)
        }
        fn password(&self, _: &str) -> Result<Option<Zeroizing<String>>, String> {
            Ok(None)
        }
        fn cancelled(&self) -> bool {
            self.cancelled.load(Ordering::SeqCst)
        }
    }

    fn summary(chain_id: u64) -> Summary {
        let fee = Fee {
            wei: U256::from(1_000_000_000_u64),
            suggested: true,
        };
        Summary {
            chain: ChainRegistry::built_in().get(chain_id),
            signer_id: "deployer".to_owned(),
            signer: Address::repeat_byte(1),
            to_name: None,
            call: "transfer(to=0x…, amount=1)".to_owned(),
            tx: UnsignedTx {
                chain_id,
                nonce: 0,
                to: Address::repeat_byte(2),
                value: U256::ZERO,
                input: vec![0xa9, 0x05, 0x9c, 0xbb],
                gas_limit: U256::from(21_000),
                fees: Fees::Eip1559 {
                    max_fee: fee,
                    max_priority_fee: fee,
                },
            },
            reverted: None,
        }
    }

    fn confirm_on(terminal: &Terminal, chain_id: u64) -> Result<Confirmation, EvmError> {
        let signing = Signing::new(SignOptions::default(), terminal, None);
        signing.confirm(&summary(chain_id), &EvmSettings::default())
    }

    #[test]
    fn ctrl_c_at_the_prompt_aborts_with_nothing_signed() {
        // Regression: the prompt was a blocking read and the flow never
        // looked at the cancellation again, so Ctrl+C then `y` signed.
        let interrupted = confirm_on(&Terminal::new(None, true), 11_155_111).unwrap_err();
        assert_eq!(interrupted.to_string(), INTERRUPTED);
        assert_eq!(interrupted.exit_code(), 130);
        // Ctrl+C while `y` was typed still aborts.
        let typed_yes = confirm_on(&Terminal::new(Some("y"), true), 11_155_111).unwrap_err();
        assert_eq!(typed_yes.exit_code(), 130);
        // On mainnet, Ctrl+C at the first question stops before the second.
        let mainnet = Terminal::new(Some("y"), true);
        assert_eq!(confirm_on(&mainnet, 1).unwrap_err().exit_code(), 130);
        assert_eq!(mainnet.asked.lock().unwrap().len(), 1);
        assert!(confirm_on(&Terminal::new(Some("y"), false), 11_155_111).is_ok());
    }

    #[test]
    fn durations_show_in_seconds_or_milliseconds() {
        assert_eq!(seconds(std::time::Duration::from_secs(120)), "120s");
        assert_eq!(seconds(std::time::Duration::from_millis(250)), "250ms");
    }

    #[test]
    fn a_transaction_that_reverted_on_chain_fails_its_request() {
        let mut record = RequestRecord::new("t".into(), "t".into(), "t.rman.yaml".into());
        record.status = RequestStatus::Passed;
        let failed = reverted_fails(record.clone(), false);
        assert_eq!(failed.status, RequestStatus::Failed);
        assert_eq!(failed.warnings, ["the transaction reverted on chain"]);
        assert_eq!(reverted_fails(record, true).status, RequestStatus::Passed);
    }

    #[test]
    fn the_chain_name_question_stays_on_one_line() {
        // Regression: the protected chain's name was escaped with
        // `printable`, which keeps newlines.
        let question = chain_name_question("Main\nnet\u{202e}");
        assert_eq!(
            question,
            "type the chain name (Main\\u{a}net\\u{202e}) to confirm: "
        );
    }
}
