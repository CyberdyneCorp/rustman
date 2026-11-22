//! The confirmation before signing (evm-signing "Mandatory decoded
//! confirmation", "Mainnet typed confirmation", "Headless signing
//! restricted to local chains").
//!
//! A [`Summary`] lists what will be signed. A [`Confirmation`] is the
//! user's approval of exactly that summary: it can only come from
//! [`Confirmation::interactive`] (the CLI prompt, with the user's
//! answers) or [`Confirmation::local_headless`] (`--yes-local`
//! on a local chain). There is no other way to make one, and the send
//! step refuses a confirmation of another transaction.
//!
//! Chain 1 and `[evm] protected_chains` are never local: a protected
//! chain listed in `headless_signing_chains` still needs its name typed,
//! and both constructors check that themselves rather than trusting the
//! caller's [`Mode`].

use alloy_primitives::{Address, B256};
use rustman_core::text::printable_line;
use rustman_evm::quantity::{Denomination, Form, Quantity};
use rustman_evm::{ChainInfo, EvmError, EvmSettings, LOCAL_CHAIN_ID};

use crate::fees::Fees;
use crate::transaction::UnsignedTx;

/// The refusal when the user answers no.
pub const DECLINED: &str = "cancelled; nothing was signed or sent";

/// The warning of a transaction whose simulation reverted
/// (`--force-revert`).
pub const SIMULATION_REVERTED: &str = "SIMULATION REVERTED";

/// What the confirmation shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    pub chain: ChainInfo,
    pub signer_id: String,
    pub signer: Address,
    /// The ENS name `to` was written as, when it was one.
    pub to_name: Option<String>,
    /// `transfer(to=0x…, amount=10)`.
    pub call: String,
    pub tx: UnsignedTx,
    /// The decoded revert of a forced transaction.
    pub reverted: Option<String>,
}

impl Summary {
    /// The fields in display order: label and value.
    pub fn fields(&self) -> Vec<(&'static str, String)> {
        let native = Denomination::native(&self.chain);
        let eth = |v| Quantity::from(v).show(Form::Native, &native);
        let to = self.tx.to.to_checksum(None);
        let to = match &self.to_name {
            Some(name) => format!("{to} ({name})"),
            None => to,
        };
        let mut fields = vec![
            ("chain", self.chain.label()),
            (
                "signer",
                format!("{} ({})", self.signer.to_checksum(None), self.signer_id),
            ),
            ("to", to),
            ("call", self.call.clone()),
            ("value", eth(self.tx.value)),
            ("gas", self.tx.gas_limit.to_string()),
        ];
        match self.tx.fees {
            Fees::Eip1559 {
                max_fee,
                max_priority_fee,
            } => {
                fields.push(("max fee", max_fee.show()));
                fields.push(("priority fee", max_priority_fee.show()));
            }
            Fees::Legacy { gas_price } => fields.push(("gas price", gas_price.show())),
        }
        fields.push(("nonce", self.tx.nonce.to_string()));
        fields.push(("max cost", eth(self.tx.max_cost())));
        fields
    }

    /// The fields with each value kept on its line: a newline in a name
    /// or a decoded argument is shown escaped, so it cannot draw a line
    /// that looks like another field.
    pub fn lines(&self) -> Vec<(&'static str, String)> {
        let fields = self.fields().into_iter();
        fields
            .map(|(label, value)| (label, printable_line(&value).into_owned()))
            .collect()
    }

    /// The decoded revert of a forced transaction, on one line.
    pub fn warning(&self) -> Option<String> {
        let revert = self.reverted.as_deref()?;
        Some(format!("{SIMULATION_REVERTED}: {}", printable_line(revert)))
    }

    /// The summary as text: an optional warning line, then one field per
    /// line.
    pub fn render(&self) -> String {
        let mut out = String::new();
        if let Some(warning) = self.warning() {
            out.push_str(&format!("WARNING: {warning}\n"));
        }
        for (label, value) in self.lines() {
            out.push_str(&format!("  {label:<13}{value}\n"));
        }
        out
    }
}

/// How the user confirms on `chain_id`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Answer yes.
    YesNo,
    /// Answer yes, then type the chain name exactly (chain 1 and
    /// `[evm] protected_chains`).
    Typed(String),
    /// No prompt: `--yes-local` on chain 31337 or a chain of
    /// `[evm] headless_signing_chains`.
    LocalHeadless,
}

/// Whether `chain_id` may be signed on without a prompt: chain 31337 or
/// a chain of `[evm] headless_signing_chains`, unless it is protected.
pub fn is_local(chain_id: u64, settings: &EvmSettings) -> bool {
    let listed = chain_id == LOCAL_CHAIN_ID || settings.headless_signing_chains.contains(&chain_id);
    listed && !is_protected(chain_id, settings)
}

/// Whether `chain_id` needs its name typed: chain 1 and
/// `[evm] protected_chains`.
pub fn is_protected(chain_id: u64, settings: &EvmSettings) -> bool {
    chain_id == 1 || settings.protected_chains.contains(&chain_id)
}

/// How to confirm on `chain`, given a terminal (`interactive`) and
/// `--yes-local`. Without a terminal, only a local chain with
/// `--yes-local` may sign; a protected chain always asks for its name.
pub fn mode(
    chain: &ChainInfo,
    settings: &EvmSettings,
    interactive: bool,
    yes_local: bool,
) -> Result<Mode, EvmError> {
    let local = is_local(chain.id, settings);
    if local && yes_local {
        return Ok(Mode::LocalHeadless);
    }
    if !interactive {
        return Err(EvmError::Refused(headless_refusal(chain, local)));
    }
    match is_protected(chain.id, settings) {
        true => Ok(Mode::Typed(chain.name.clone())),
        false => Ok(Mode::YesNo),
    }
}

fn headless_refusal(chain: &ChainInfo, local: bool) -> String {
    match local {
        true => format!(
            "no terminal to confirm on; add --yes-local to sign on {} without a prompt",
            chain.label()
        ),
        false => format!(
            "signing on {} needs an interactive confirmation; without a terminal only chain 31337 and [evm] headless_signing_chains can be signed on, with --yes-local",
            chain.label()
        ),
    }
}

/// Whether `answer` is `y` or `yes` (any case).
pub fn is_yes(answer: &str) -> bool {
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// The user's approval of one transaction.
#[derive(Debug, PartialEq, Eq)]
pub struct Confirmation {
    digest: B256,
}

impl Confirmation {
    /// The user's answers to the prompt: `answer` must be
    /// `y` or `yes`; on a protected chain (chain 1, `[evm]
    /// protected_chains`), `typed` must be the chain's name exactly.
    /// Whether the chain is protected is decided here, from the summary's
    /// chain and `settings`. Anything else cancels (exit code 4).
    pub fn interactive(
        summary: &Summary,
        settings: &EvmSettings,
        answer: &str,
        typed: Option<&str>,
    ) -> Result<Self, EvmError> {
        if !is_yes(answer) {
            return Err(EvmError::Refused(DECLINED.to_owned()));
        }
        let name = summary.chain.name.trim();
        let protected = is_protected(summary.chain.id, settings);
        if protected && name.is_empty() {
            return Err(EvmError::Refused(format!(
                "protected chain {} has no name to type; set [evm.chains.{}] name; {DECLINED}",
                summary.chain.id, summary.chain.id
            )));
        }
        if protected && typed.map(str::trim) != Some(name) {
            return Err(EvmError::Refused(format!(
                "the chain name did not match '{name}'; {DECLINED}"
            )));
        }
        Ok(Self::of(summary))
    }

    /// `--yes-local` on a local chain: no prompt.
    pub fn local_headless(summary: &Summary, settings: &EvmSettings) -> Result<Self, EvmError> {
        match is_local(summary.chain.id, settings) {
            true => Ok(Self::of(summary)),
            false => Err(EvmError::Refused(headless_refusal(&summary.chain, false))),
        }
    }

    fn of(summary: &Summary) -> Self {
        Self {
            digest: summary.tx.signing_hash(),
        }
    }

    /// Whether this confirms `tx`.
    pub(crate) fn covers(&self, tx: &UnsignedTx) -> bool {
        self.digest == tx.signing_hash()
    }
}
