//! The confirmation before signing (evm-signing "Mandatory decoded
//! confirmation", "Mainnet typed confirmation", "Headless signing
//! restricted to local chains").
//!
//! A [`Summary`] lists what will be signed; [`Summary::new`] refuses one
//! whose transaction is for another chain than the one shown, and its
//! fields cannot be changed afterwards. A [`Confirmation`] is the
//! user's approval of exactly that summary: it can only come from
//! [`Confirmation::interactive`] (the CLI prompt, with the user's
//! answers) or [`Confirmation::local_headless`] (`--yes-local`
//! on a local chain). There is no other way to make one, and the send
//! step refuses a confirmation of another transaction.
//!
//! Chain 1 and `[evm] protected_chains` are never local: a protected
//! chain listed in `headless_signing_chains` still needs its name typed,
//! and both constructors check that themselves rather than trusting the
//! caller's [`Mode`], from the chain id the transaction is signed for.
//!
//! `--yes-local` never signs a transaction whose max cost (gas limit ×
//! max fee + value) is above `[evm] max_cost` (default: 1 of the chain's
//! native unit); on a terminal, the summary warns about it.

use alloy_primitives::{Address, B256, U256};
use rustman_core::text::printable_line;
use rustman_evm::quantity::{Denomination, Form, Quantity, parse_units};
use rustman_evm::{ChainInfo, EvmError, EvmSettings, LOCAL_CHAIN_ID};

use crate::fees::Fees;
use crate::transaction::UnsignedTx;

/// The refusal when the user answers no.
pub const DECLINED: &str = "cancelled; nothing was signed or sent";

/// The warning of a transaction whose simulation reverted
/// (`--force-revert`).
pub const SIMULATION_REVERTED: &str = "SIMULATION REVERTED";

/// What the confirmation shows, as the parts of a [`Summary`].
#[derive(Clone, Debug, PartialEq)]
pub struct SummaryParts {
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

/// What the confirmation shows: built by [`Summary::new`] only, read
/// through [`std::ops::Deref`], never changed.
#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    parts: SummaryParts,
}

impl std::ops::Deref for Summary {
    type Target = SummaryParts;

    fn deref(&self) -> &SummaryParts {
        &self.parts
    }
}

/// The refusal of a summary whose transaction is for another chain.
fn chain_mismatch(parts: &SummaryParts) -> EvmError {
    EvmError::Refused(format!(
        "the transaction is for chain {} but the summary shows {}; nothing was signed or sent",
        parts.tx.chain_id,
        parts.chain.label()
    ))
}

impl Summary {
    /// The summary of `parts`; refused when the transaction's chain id is
    /// not the chain shown.
    pub fn new(parts: SummaryParts) -> Result<Self, EvmError> {
        match parts.tx.chain_id == parts.chain.id {
            true => Ok(Self { parts }),
            false => Err(chain_mismatch(&parts)),
        }
    }

    /// The chain id the transaction is signed for, when the summary shows
    /// that chain.
    fn signed_chain(&self) -> Result<u64, EvmError> {
        match self.tx.chain_id == self.chain.id {
            true => Ok(self.tx.chain_id),
            false => Err(chain_mismatch(&self.parts)),
        }
    }

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

/// The default `[evm] max_cost`: 1 of the chain's native unit.
pub const DEFAULT_MAX_COST: &str = "1";

/// `[evm] max_cost` (a decimal amount of `chain`'s native unit) in wei.
pub fn max_cost(chain: &ChainInfo, settings: &EvmSettings) -> Result<U256, EvmError> {
    let text = settings.max_cost.as_deref().unwrap_or(DEFAULT_MAX_COST);
    parse_units(text.trim(), chain.decimals)
        .map_err(|e| EvmError::config(format!("[evm] max_cost = \"{text}\": {e}")))
}

/// `max cost 2 ETH is above [evm] max_cost (1 ETH)` when the summary's
/// max cost (gas limit × max fee + value) is above the cap; an invalid
/// `[evm] max_cost` is an error.
pub fn cost_warning(summary: &Summary, settings: &EvmSettings) -> Result<Option<String>, EvmError> {
    let cap = max_cost(&summary.chain, settings)?;
    let cost = summary.tx.max_cost();
    if cost <= cap {
        return Ok(None);
    }
    let native = Denomination::native(&summary.chain);
    let shown = |v: U256| Quantity::from(v).show(Form::Native, &native);
    Ok(Some(format!(
        "max cost {} is above [evm] max_cost ({})",
        shown(cost),
        shown(cap)
    )))
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
    /// Whether the chain is protected is decided here, from the chain id
    /// the transaction is signed for and `settings`. Anything else
    /// cancels (exit code 4).
    pub fn interactive(
        summary: &Summary,
        settings: &EvmSettings,
        answer: &str,
        typed: Option<&str>,
    ) -> Result<Self, EvmError> {
        let chain_id = summary.signed_chain()?;
        if !is_yes(answer) {
            return Err(EvmError::Refused(DECLINED.to_owned()));
        }
        let name = summary.chain.name.trim();
        let protected = is_protected(chain_id, settings);
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

    /// `--yes-local` on a local chain: no prompt, and never above
    /// `[evm] max_cost`.
    pub fn local_headless(summary: &Summary, settings: &EvmSettings) -> Result<Self, EvmError> {
        let chain_id = summary.signed_chain()?;
        if !is_local(chain_id, settings) {
            return Err(EvmError::Refused(headless_refusal(&summary.chain, false)));
        }
        if let Some(over) = cost_warning(summary, settings)? {
            return Err(EvmError::Refused(format!(
                "{over}; --yes-local does not sign above it (confirm on a terminal, or raise [evm] max_cost); nothing was signed or sent"
            )));
        }
        Ok(Self::of(summary))
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

#[cfg(test)]
mod tests {
    use alloy_primitives::U256;
    use rustman_evm::ChainRegistry;

    use super::*;
    use crate::fees::Fee;

    fn parts(chain_id: u64, tx_chain_id: u64) -> SummaryParts {
        let fee = Fee {
            wei: U256::from(1_000_000_000_u64),
            suggested: true,
        };
        SummaryParts {
            chain: ChainRegistry::built_in().get(chain_id),
            signer_id: "dev".to_owned(),
            signer: Address::repeat_byte(1),
            to_name: None,
            call: "f()".to_owned(),
            tx: UnsignedTx {
                chain_id: tx_chain_id,
                nonce: 0,
                to: Address::repeat_byte(2),
                value: U256::ZERO,
                input: Vec::new(),
                gas_limit: U256::from(21_000),
                fees: Fees::Eip1559 {
                    max_fee: fee,
                    max_priority_fee: fee,
                },
            },
            reverted: None,
        }
    }

    #[test]
    fn a_summary_showing_another_chain_than_the_transaction_is_never_confirmed() {
        // Regression: the local/protected decision read the summary's
        // chain while the signature covered the transaction's chain id, so
        // a summary of chain 31337 with a chain-1 transaction got a
        // headless confirmation and a mainnet signature.
        let crafted = Summary {
            parts: parts(31_337, 1),
        };
        let settings = EvmSettings::default();
        let err = Confirmation::local_headless(&crafted, &settings).unwrap_err();
        assert_eq!(err.exit_code(), 4);
        assert_eq!(
            err.to_string(),
            "the transaction is for chain 1 but the summary shows Anvil/Hardhat local (31337); nothing was signed or sent"
        );
        let err = Confirmation::interactive(&crafted, &settings, "y", None).unwrap_err();
        assert_eq!(err.exit_code(), 4);
        assert!(Summary::new(parts(31_337, 1)).is_err());
        let fine = Summary::new(parts(31_337, 31_337)).unwrap();
        assert!(Confirmation::local_headless(&fine, &settings).is_ok());
    }
}
