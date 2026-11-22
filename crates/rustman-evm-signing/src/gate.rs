//! The preconditions every signature passes (evm-signing "Signing disabled
//! by default", "Chain id must match", "Simulate before signing"), in the
//! order a front end checks them:
//!
//! 1. [`check_enabled`]: `[evm] allow_signing = true` in `rustman.toml`;
//! 2. [`select_signer`]: a signer id, never a raw key;
//! 3. [`check_sign_flag`]: the `--sign` flag;
//! 4. [`require_pin`]: the environment pins `chain_id`;
//! 5. [`check_chain`]: the endpoint's `eth_chainId`, asked again right
//!    before signing, equals the pin;
//! 6. [`simulate`] and [`check_simulation`]: `eth_call` from the signer at
//!    `pending` and `eth_estimateGas` succeed, unless `--force-revert`.
//!
//! Every refusal is an [`EvmError::Refused`] (exit code 4) and nothing is
//! signed or sent.

use alloy_json_abi::{Function, JsonAbi};
use alloy_primitives::U256;
use rustman_evm::call::{BlockSel, CallReport, CallRequest, run_call};
use rustman_evm::identity::Pin;
use rustman_evm::quantity::parse_hex_u64;
use rustman_evm::rpc::{Rpc, call};
use rustman_evm::{ChainInfo, ChainRegistry, EvmError, EvmSettings};
use serde_json::json;

use crate::config::check_id;
use crate::confirm::is_protected;
use crate::rawkey::refuse_raw_key;

/// The refusal when the workspace has not opted in.
pub const SIGNING_DISABLED: &str =
    "signing is disabled for this workspace; set [evm] allow_signing = true";

/// The refusal when `--sign` is missing (after the preview).
pub const SIGN_FLAG_MISSING: &str = "add --sign to sign and send";

/// The error when neither the request nor `--signer` names a signer.
pub const NO_SIGNER: &str = "no signer: set 'signer' in the request file or pass --signer";

fn refused(message: impl Into<String>) -> EvmError {
    EvmError::Refused(message.into())
}

/// `[evm] allow_signing = true`, and no protected chain (chain 1 or one
/// of `[evm] protected_chains`) in `[evm] headless_signing_chains`.
pub fn check_enabled(settings: &EvmSettings) -> Result<(), EvmError> {
    if !settings.allow_signing {
        return Err(refused(SIGNING_DISABLED));
    }
    let headless = &settings.headless_signing_chains;
    match headless.iter().find(|&&id| is_protected(id, settings)) {
        Some(id) => Err(EvmError::config(format!(
            "[evm] headless_signing_chains: chain {id} is protected (chain 1 and [evm] protected_chains always need the chain name typed); remove it"
        ))),
        None => Ok(()),
    }
}

/// The per-invocation opt-in.
pub fn check_sign_flag(sign: bool) -> Result<(), EvmError> {
    match sign {
        true => Ok(()),
        false => Err(refused(SIGN_FLAG_MISSING)),
    }
}

/// The signer id to use: `--signer` (or `RUSTMAN_EVM_SIGNER`) over the
/// request's resolved `signer`. Key material in either is refused, even
/// when the other one wins, and the chosen id must be a plain identifier
/// ([`check_id`]); neither error echoes the value.
pub fn select_signer<'a>(
    flag: Option<&'a str>,
    request: Option<&'a str>,
) -> Result<&'a str, EvmError> {
    for value in [flag, request].into_iter().flatten() {
        refuse_raw_key(value)?;
    }
    let id = flag
        .or(request)
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| EvmError::config(NO_SIGNER))?;
    check_id(id)?;
    Ok(id)
}

/// The environment's `chain_id` pin.
pub fn require_pin(environment: Option<&str>, chain_id: Option<u64>) -> Result<Pin<'_>, EvmError> {
    match (environment, chain_id) {
        (Some(environment), Some(chain_id)) => Ok(Pin {
            environment,
            chain_id,
        }),
        (Some(environment), None) => Err(refused(format!(
            "signing requires 'chain_id' in environment '{environment}'"
        ))),
        (None, _) => Err(refused(
            "signing requires an environment with 'chain_id'; select one with --env",
        )),
    }
}

/// Ask the endpoint's `eth_chainId` now (never from a cache) and check it
/// against `pin`. The chain is described as [`signing_chain`] says.
pub async fn check_chain(
    rpc: &dyn Rpc,
    registry: &ChainRegistry,
    pin: Pin<'_>,
) -> Result<ChainInfo, EvmError> {
    let answer = call(rpc, "eth_chainId", json!([])).await?;
    let chain_id = answer
        .as_str()
        .and_then(|text| parse_hex_u64(text).ok())
        .ok_or_else(|| EvmError::Rpc("eth_chainId: not a quantity".to_owned()))?;
    pin.check(chain_id, registry)?;
    Ok(signing_chain(registry, chain_id))
}

/// The chain `chain_id` as a confirmation shows it. A built-in chain
/// keeps its built-in name, currency symbol and decimals whatever
/// `[evm.chains.<id>]` says (only its explorer can change), so the value,
/// the max cost and the name typed on chain 1 are always the real ones.
pub fn signing_chain(registry: &ChainRegistry, chain_id: u64) -> ChainInfo {
    let configured = registry.get(chain_id);
    let built_in = ChainRegistry::built_in().get(chain_id);
    match built_in.known {
        true => ChainInfo {
            explorer: configured.explorer,
            ..built_in
        },
        false => configured,
    }
}

/// What the simulation found.
#[derive(Clone, Debug, PartialEq)]
pub struct Simulation {
    pub report: CallReport,
}

impl Simulation {
    /// The revert of the `eth_call`, or of `eth_estimateGas` when only the
    /// estimate failed.
    pub fn revert(&self) -> Option<String> {
        if let Some(revert) = self.report.revert() {
            return Some(revert.to_string());
        }
        match &self.report.gas_estimate {
            Some(Err(reason)) => Some(reason.clone()),
            _ => None,
        }
    }

    /// The gas estimate, when the simulation succeeded.
    pub fn gas_estimate(&self) -> Option<U256> {
        match &self.report.gas_estimate {
            Some(Ok(gas)) => Some(*gas),
            _ => None,
        }
    }
}

/// Simulate `request` with `eth_call` from the signer at `pending` and
/// `eth_estimateGas`. A revert is a [`Simulation`] result; a transport
/// failure is an error.
pub async fn simulate(
    rpc: &dyn Rpc,
    request: &CallRequest,
    function: &Function,
    abi: Option<&JsonAbi>,
) -> Result<Simulation, EvmError> {
    let mut pending = request.clone();
    pending.block = BlockSel::Tag("pending".to_owned());
    pending.overrides = None;
    let report = run_call(rpc, &pending, function, abi, true).await?;
    Ok(Simulation { report })
}

/// The refusal of a reverting simulation, unless `force_revert`.
pub fn check_simulation(simulation: &Simulation, force_revert: bool) -> Result<(), EvmError> {
    match (simulation.revert(), force_revert) {
        (Some(revert), false) => Err(refused(simulation_refusal(&revert))),
        _ => Ok(()),
    }
}

/// `simulation reverted: InsufficientBalance(5, 10); nothing was signed
/// or sent (add --force-revert to sign anyway)`.
pub fn simulation_refusal(revert: &str) -> String {
    let what = match revert.starts_with("reverted") {
        true => format!("simulation {revert}"),
        false => format!("simulation failed: {revert}"),
    };
    format!("{what}; nothing was signed or sent (add --force-revert to sign anyway)")
}
