//! Running `evm-call` requests inside a run (tasks 4.1, 4.2): the runner of
//! `rustman-http` resolves the file and hands it here through
//! [`EvmExecutor`]; this module identifies the endpoint (chain pin, client),
//! loads the ABI, resolves ENS names, runs the `eth_call` (with an estimate
//! when asked or when a `gas_lt` assertion needs one, and a trace when
//! asked), evaluates the `evm` assertions and builds the document captures
//! read. Read-only: nothing here signs or sends a transaction.

use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy_json_abi::{Function, JsonAbi};
use rustman_core::environment::Environment;
use rustman_core::model::{AbiSpec, Assertion, EvmAssertion, unlocated};
use rustman_core::vars::ResolvedRequest;
use rustman_http::SendOptions;
use rustman_http::runner::{EvmCall, EvmExecutor, EvmFuture, EvmOutcome, evm_error_record};
use rustman_jsonrpc::HttpSend;
use serde_json::{Value, json};

use crate::abi::{AbiCache, LoadedAbi, resolve_function};
use crate::assert::{EvmFacts, evaluate_all};
use crate::call::{
    CallInputs, CallReport, CallRequest, output_key, parse_address, prepare, run_call,
};
use crate::chain::ChainRegistry;
use crate::codec::{DecodedLog, value_to_json};
use crate::ens::{ENS_NEEDS_MAINNET, Ens, ens_for, is_ens_name};
use crate::error::EvmError;
use crate::events::{NO_EVENTS, events_json, simulated_events, wants_events};
use crate::explorer::Explorer;
use crate::identity::{EndpointIdentity, Identities, Pin};
use crate::quantity::Denomination;
use crate::rpc::{JsonrpcTransport, Rpc};
use crate::settings::EvmSettings;
use crate::trace::{TraceResult, TraceSupport, TraceView};
use crate::tx::{NO_DECIMALS, TokenCache};

/// Runs `evm-call` requests for a run, keeping per-session caches (node
/// identities, token metadata, parsed ABIs, trace support).
pub struct EvmRunner {
    sender: Arc<dyn HttpSend + Send + Sync>,
    /// `--estimate`: ask `eth_estimateGas` for every call.
    pub estimate: bool,
    /// `--trace`: trace every call.
    pub trace: bool,
    identities: Identities,
    tokens: TokenCache,
    abis: AbiCache,
    traces: TraceSupport,
}

impl EvmRunner {
    /// A runner sending over `sender`: the engine for the CLI and the TUI,
    /// the governed sender for agents.
    pub fn new(sender: Arc<dyn HttpSend + Send + Sync>) -> Self {
        Self {
            sender,
            estimate: false,
            trace: false,
            identities: Identities::new(),
            tokens: TokenCache::new(),
            abis: AbiCache::new(),
            traces: TraceSupport::new(),
        }
    }

    /// Calls to `endpoint` with `options`.
    pub fn transport(&self, endpoint: ResolvedRequest, options: &SendOptions) -> JsonrpcTransport {
        JsonrpcTransport::new(endpoint, self.sender.clone(), options.clone())
    }

    /// The node identities seen so far this session.
    pub fn identities(&self) -> &Identities {
        &self.identities
    }

    /// Identify `rpc` under the workspace's `[evm]` table `evm`, pinned
    /// to `environment`'s `chain_id` when it sets one.
    pub async fn connect(
        &self,
        rpc: &dyn Rpc,
        evm: &toml::Table,
        environment: Option<&Environment>,
    ) -> Result<Connected, EvmError> {
        let settings = EvmSettings::from_table(evm)?;
        let registry = ChainRegistry::with_overrides(&settings)?;
        let pin = environment.and_then(|env| {
            env.chain_id.map(|chain_id| Pin {
                environment: &env.name,
                chain_id,
            })
        });
        let identity = self.identities.identify(rpc, &registry, pin).await?;
        Ok(Connected {
            settings,
            registry,
            identity,
        })
    }

    async fn run(&self, call: &EvmCall<'_>) -> Result<Done, EvmError> {
        let rpc = self.transport(call.resolved.endpoint.clone(), call.options);
        let Connected {
            settings, identity, ..
        } = self
            .connect(&rpc, &call.settings.evm, call.environment)
            .await?;
        let native = Denomination::native(&identity.chain);
        let (ens_rpc, unresolved) = match call.resolved.ens_rpc.clone() {
            Some(Ok(endpoint)) => (Some(self.transport(endpoint, call.options)), None),
            Some(Err(reason)) => (None, Some(reason)),
            None => (None, None),
        };
        let configured = ens_rpc.as_ref().map(|r| r as &dyn Rpc);
        let ens = match unresolved {
            Some(_) => None,
            None => ens_for(&rpc, identity.chain.id, configured).ok(),
        };
        let unresolved = |error: EvmError| ens_unavailable(error, unresolved.as_deref());
        let inputs = inputs(call);
        let context = AbiContext {
            call,
            settings: &settings,
            rpc: &rpc,
            chain_id: identity.chain.id,
            ens: ens.as_ref(),
        };
        let (abi, abi_note) = self.abi(&context, &inputs.to).await.map_err(unresolved)?;
        let spec = call.evm.function.as_ref().map(|f| f.get().as_str());
        let function = resolve_function(&abi.abi, spec.unwrap_or_default())?.clone();
        let request = prepare(&inputs, &function, &native, ens.as_ref())
            .await
            .map_err(unresolved)?;
        let assertions = unlocated(call.evm.assertions.as_deref());
        let estimate = self.estimate || wants_gas(&assertions);
        let report = run_call(&rpc, &request, &function, Some(&abi.abi), estimate).await?;
        let report = report
            .with_token(&rpc, identity.chain.id, request.to, &self.tokens)
            .await;
        let trace = match self.trace {
            true => Some(self.trace_text(&rpc, &request, &identity, &abi.abi).await),
            false => None,
        };
        let captures = call.evm.captures.as_deref().unwrap_or_default();
        let events = match wants_events(&assertions, captures) && report.revert().is_none() {
            true => simulated_events(&rpc, &request, &abi.abi).await,
            false => Ok(Vec::new()),
        };
        Ok(Done {
            identity,
            native,
            request,
            report,
            trace,
            abi_note,
            assertions,
            events,
        })
    }

    async fn abi(
        &self,
        ctx: &AbiContext<'_, '_>,
        to: &str,
    ) -> Result<(LoadedAbi, Option<String>), EvmError> {
        let spec = ctx.call.evm.abi.as_ref().map(|a| a.get());
        let file = &ctx.call.request.entry.path;
        match spec {
            Some(AbiSpec::Source(source)) => {
                let loaded = self.abis.load(ctx.call.root, file, source)?;
                Ok((loaded, None))
            }
            Some(AbiSpec::Explorer) => self.fetch_abi(ctx, to).await,
            None => Err(EvmError::config("evm-call: 'abi' is required")),
        }
    }

    async fn fetch_abi(
        &self,
        ctx: &AbiContext<'_, '_>,
        to: &str,
    ) -> Result<(LoadedAbi, Option<String>), EvmError> {
        let explorer = Explorer {
            settings: ctx.settings,
            root: ctx.call.root,
            http: self.sender.as_ref(),
            options: ctx.call.options,
            api_key: ctx
                .call
                .resolved
                .explorer_keys
                .get(&ctx.chain_id.to_string())
                .cloned(),
            cache: &self.abis,
        };
        if !ctx.settings.allow_explorer_fetch {
            return Err(EvmError::config(crate::explorer::FETCH_DISABLED));
        }
        let address = target_address(to, ctx.ens).await?;
        let fetched = explorer.fetch(ctx.rpc, ctx.chain_id, address).await?;
        let note = fetched.note();
        Ok((fetched.loaded, note))
    }

    async fn trace_text(
        &self,
        rpc: &dyn Rpc,
        request: &CallRequest,
        identity: &EndpointIdentity,
        abi: &Arc<JsonAbi>,
    ) -> Value {
        match crate::trace::trace_call(rpc, request, &identity.client, &self.traces).await {
            TraceResult::Tree(tree) => {
                let target = request.to;
                let abis = |address: Option<alloy_primitives::Address>| match address {
                    Some(a) if a == target => vec![abi.clone()],
                    _ => crate::abi::BUILT_IN
                        .iter()
                        .filter_map(|n| crate::abi::built_in(n))
                        .collect(),
                };
                json!({"tree": TraceView { abis: &abis }.render(&tree)})
            }
            TraceResult::Unsupported(note) => json!({"note": note}),
            TraceResult::Failed(reason) => json!({"note": format!("trace failed: {reason}")}),
        }
    }
}

impl EvmExecutor for EvmRunner {
    fn execute<'a>(&'a self, call: EvmCall<'a>) -> EvmFuture<'a> {
        Box::pin(async move {
            let started = Instant::now();
            let result = self.run(&call).await;
            let elapsed = started.elapsed();
            match result {
                Ok(done) => done.outcome(elapsed),
                Err(error) => error_outcome(&error, elapsed),
            }
        })
    }
}

/// An identified endpoint and the settings it was checked with.
pub struct Connected {
    pub settings: EvmSettings,
    pub registry: ChainRegistry,
    pub identity: EndpointIdentity,
}

/// What loading the ABI needs.
struct AbiContext<'c, 'a> {
    call: &'c EvmCall<'a>,
    settings: &'c EvmSettings,
    rpc: &'c dyn Rpc,
    chain_id: u64,
    ens: Option<&'c Ens<'c>>,
}

/// `error`, or, when it is the generic "no ENS endpoint" hint and
/// `[evm] ens_rpc` is set but did not resolve, why it did not.
fn ens_unavailable(error: EvmError, unresolved: Option<&str>) -> EvmError {
    match (unresolved, &error) {
        (Some(reason), EvmError::Config(message)) if message == ENS_NEEDS_MAINNET => {
            EvmError::config(reason)
        }
        _ => error,
    }
}

async fn target_address(
    to: &str,
    ens: Option<&Ens<'_>>,
) -> Result<alloy_primitives::Address, EvmError> {
    if is_ens_name(to) {
        let ens = ens.ok_or_else(|| EvmError::config(ENS_NEEDS_MAINNET))?;
        return ens.resolve(to).await;
    }
    parse_address(to).map_err(|reason| EvmError::config(format!("to: {reason}")))
}

fn inputs(call: &EvmCall<'_>) -> CallInputs {
    let resolved = call.resolved;
    CallInputs {
        to: resolved.to.clone(),
        from: resolved.from.clone(),
        value: resolved.value.clone(),
        args: resolved.args.clone(),
        block: resolved.block.clone(),
        state_overrides: resolved.state_overrides.clone(),
    }
}

/// Whether an assertion needs the gas estimate.
fn wants_gas(assertions: &[Assertion]) -> bool {
    assertions
        .iter()
        .any(|a| matches!(a, Assertion::Evm(e) if e.gas_lt.is_some()))
}

/// The `evm` assertions of a list, for callers outside the runner.
pub fn evm_assertions(assertions: &[Assertion]) -> Vec<&EvmAssertion> {
    assertions
        .iter()
        .filter_map(|a| match a {
            Assertion::Evm(evm) => Some(&***evm),
            _ => None,
        })
        .collect()
}

/// A call that ran.
struct Done {
    identity: EndpointIdentity,
    native: Denomination,
    request: CallRequest,
    report: CallReport,
    trace: Option<Value>,
    abi_note: Option<String>,
    assertions: Vec<Assertion>,
    /// The events a simulation of the call emits, when the request reads
    /// them, or why the node could not simulate them.
    events: Result<Vec<DecodedLog>, String>,
}

impl Done {
    fn outcome(self, elapsed: Duration) -> EvmOutcome {
        let gas = self
            .report
            .gas_estimate
            .as_ref()
            .and_then(|g| g.as_ref().ok().copied());
        let facts = EvmFacts {
            outputs: self.report.returned_values(),
            revert: self.report.revert(),
            events: self.events.as_deref().unwrap_or_default(),
            gas,
            native: &self.native,
            token: self.report.token.as_ref(),
        };
        let evm_assertions = evaluate_all(&self.assertions, &facts);
        let mut warnings: Vec<String> = Vec::new();
        if self
            .report
            .token
            .as_ref()
            .is_some_and(|t| t.decimals.is_none())
        {
            warnings.push(NO_DECIMALS.to_owned());
        }
        if let Err(reason) = &self.events {
            warnings.push(format!("{NO_EVENTS}: {reason}"));
        }
        let events = self.events.as_deref().unwrap_or_default();
        EvmOutcome {
            document: document(&self.report, events),
            evm_assertions,
            error: None,
            warnings,
            record: self.record(),
            elapsed,
        }
    }

    fn record(&self) -> Value {
        json!({
            "endpoint": self.identity.to_json(),
            "status": self.identity.status(),
            "to": self.request.to.to_checksum(None),
            "block": self.request.block.to_param(),
            "result": self.report.to_json(),
            "rendered": self.report.render(),
            "trace": self.trace,
            "abi": self.abi_note,
            "events": self.event_lines(),
        })
    }

    /// The simulated events, one decoded line each.
    fn event_lines(&self) -> Vec<String> {
        let events = self.events.as_deref().unwrap_or_default();
        events.iter().map(ToString::to_string).collect()
    }
}

/// What captures read: outputs by position with their names, the events
/// a simulation of the call emits (when the request reads them) and the
/// receipt (none for a read-only call).
pub fn document(report: &CallReport, events: &[DecodedLog]) -> Value {
    let outputs: Vec<Value> = report
        .returned_values()
        .unwrap_or_default()
        .iter()
        .enumerate()
        .map(|(i, v)| {
            json!({
                "name": v.name,
                "key": output_key(v, i),
                "type": v.ty,
                "value": value_to_json(&v.value),
            })
        })
        .collect();
    json!({"outputs": outputs, "events": events_json(events), "receipt": null})
}

/// The kind of an error, as `rustman evm` maps it to an exit code.
pub fn error_kind(error: &EvmError) -> &'static str {
    match error {
        EvmError::Config(_) => "config",
        EvmError::Rpc(_) => "rpc",
        EvmError::Refused(_) => "refused",
        EvmError::NotFound(_) => "not_found",
        EvmError::Interrupted(_) => "interrupted",
    }
}

/// The exit code of an error kind recorded by [`error_kind`].
pub fn exit_code_of_kind(kind: &str) -> u8 {
    match kind {
        "not_found" => 1,
        "config" => 2,
        "refused" => 4,
        "interrupted" => 130,
        _ => 3,
    }
}

/// Whether the `evm` record of a request holds a safety refusal (exit 4).
pub fn is_refusal(record: &Value) -> bool {
    record["error"]["kind"] == "refused"
}

fn error_outcome(error: &EvmError, elapsed: Duration) -> EvmOutcome {
    EvmOutcome {
        document: json!({"outputs": [], "events": [], "receipt": null}),
        evm_assertions: Vec::new(),
        error: Some(error.to_string()),
        warnings: Vec::new(),
        record: evm_error_record(error_kind(error), &error.to_string()),
        elapsed,
    }
}

/// The function an `evm-call` file names, from its ABI, for front ends
/// that show it before sending.
pub fn function_of(abi: &JsonAbi, spec: &str) -> Result<Function, EvmError> {
    resolve_function(abi, spec).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unresolved_ens_rpc_replaces_the_generic_ens_hint() {
        // Regression: an `[evm] ens_rpc` that did not resolve was dropped,
        // and a call needing ENS said "set [evm] ens_rpc".
        let reason = "[evm] ens_rpc: unresolved variable 'mainnet_rpc'";
        let hint = || EvmError::config(ENS_NEEDS_MAINNET);
        assert_eq!(
            ens_unavailable(hint(), Some(reason)),
            EvmError::config(reason)
        );
        assert_eq!(ens_unavailable(hint(), None), hint());
        let other = EvmError::Rpc("down".into());
        assert_eq!(ens_unavailable(other.clone(), Some(reason)), other);
    }
}
