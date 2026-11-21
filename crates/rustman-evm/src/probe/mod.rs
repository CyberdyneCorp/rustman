//! `rustman evm probe` (add-evm-toolkit "chain-exploration", design D10):
//! probe one or more EVM RPC providers and report on them as a
//! `rustman.report/v1` exploration with `subject: evm-rpc`.
//!
//! Each provider is identified (`eth_chainId`; providers on different
//! chains stop the probe with exit code 2), then its head is read, every
//! catalog method of `--methods` is called with minimal valid parameters
//! (the support matrix), historical state is read at an old block
//! (archive detection), recent blocks are fetched (block times and gas),
//! WebSocket providers are subscribed to `newHeads` and `logs`, and
//! `--call` is traced. With two or more providers their heads, a block
//! hash, a balance and the `--call` result are compared; with
//! `--rate-discovery` the rate limit is searched for.
//!
//! Only methods of the read-only [`catalog`] are sent; every request
//! passes the safety policy and counts against the provider's budget.

pub mod catalog;
pub mod checks;
pub mod compare;
pub mod meter;
pub mod provider;
pub mod rate;
pub mod report;
pub mod support;

use std::time::{Duration, Instant};

use alloy_primitives::{Address, hex};
use futures_util::future::join_all;
use rustman_core::safety::Limits;
use rustman_report::exploration::{Evidence, ExplorationReport, Severity};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::call::{BlockSel, CallRequest, parse_address};
use crate::chain::ChainRegistry;
use crate::error::EvmError;
use catalog::{MethodSelection, ZERO_ADDRESS};
use checks::{ProviderRun, archive_block};
use meter::Meter;
pub use provider::{Provider, ProviderSpec, governed};

/// The default request budget per provider.
pub const DEFAULT_BUDGET: u64 = 500;
/// The default `--max-rate` of rate discovery.
pub const DEFAULT_MAX_RATE: u32 = 40;
/// The default `--max-drift`, in blocks.
pub const DEFAULT_MAX_DRIFT: u64 = 3;

/// The pacing of the timed checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timing {
    /// Head samples of the drift check.
    pub drift_samples: usize,
    /// Time between head samples.
    pub drift_interval: Duration,
    /// How long a subscription may stay silent.
    pub subscription_timeout: Duration,
    /// How long each rate-discovery step lasts.
    pub rate_window: Duration,
}

impl Default for Timing {
    /// 5 head samples over 10 s, 30 s for a first head, 1 s per rate step.
    fn default() -> Self {
        Self {
            drift_samples: 5,
            drift_interval: Duration::from_millis(2500),
            subscription_timeout: Duration::from_secs(30),
            rate_window: Duration::from_secs(1),
        }
    }
}

/// `--call <ADDRESS>:<CALLDATA>`: an `eth_call` compared across providers
/// and traced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeCall {
    pub to: Address,
    pub data: Vec<u8>,
}

impl ProbeCall {
    pub fn parse(text: &str) -> Result<Self, EvmError> {
        let usage = || {
            EvmError::config(format!(
                "--call '{text}': expected <address>:<calldata>, such as 0xA0b8...eB48:0x18160ddd"
            ))
        };
        let (to, data) = text.split_once(':').ok_or_else(usage)?;
        let to = parse_address(to.trim()).map_err(|e| EvmError::config(format!("--call: {e}")))?;
        let data = hex::decode(data.trim()).map_err(|_| usage())?;
        Ok(Self { to, data })
    }

    /// The call as an `eth_call` at `latest`.
    pub fn request(&self) -> CallRequest {
        CallRequest {
            to: self.to,
            from: None,
            value: None,
            data: self.data.clone(),
            block: BlockSel::default(),
            overrides: None,
        }
    }

    /// The transaction object of `eth_call`.
    pub fn tx_object(&self) -> Value {
        self.request().tx_object()
    }
}

/// How to probe.
#[derive(Clone, Debug)]
pub struct ProbeOptions {
    pub methods: MethodSelection,
    /// `--block`: the archive check's block.
    pub block: Option<u64>,
    /// `--budget`: requests per provider.
    pub budget: u64,
    /// `--address`: the address of balance and state calls.
    pub address: String,
    pub call: Option<ProbeCall>,
    pub rate_discovery: bool,
    pub max_rate: u32,
    pub max_drift: u64,
    pub fail_on: Severity,
    /// The safety limits of the probe calls (for the report).
    pub limits: Limits,
    pub timing: Timing,
    pub cancel: CancellationToken,
    pub registry: ChainRegistry,
    /// The active environment's name.
    pub environment: Option<String>,
}

impl Default for ProbeOptions {
    fn default() -> Self {
        Self {
            methods: MethodSelection::standard(),
            block: None,
            budget: DEFAULT_BUDGET,
            address: ZERO_ADDRESS.to_owned(),
            call: None,
            rate_discovery: false,
            max_rate: DEFAULT_MAX_RATE,
            max_drift: DEFAULT_MAX_DRIFT,
            fail_on: Severity::High,
            limits: Limits::default(),
            timing: Timing::default(),
            cancel: CancellationToken::new(),
            registry: ChainRegistry::built_in(),
            environment: None,
        }
    }
}

/// How one check ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckOutcome {
    Passed,
    Finding,
    SkippedBudget,
    Failed,
}

/// One check of one provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    pub endpoint: String,
    /// A method, or `archive`, `head-drift`, `block-hash`, ...
    pub operation: String,
    pub outcome: CheckOutcome,
    pub reason: Option<String>,
}

/// A finding rule of the probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rule {
    pub id: &'static str,
    pub severity: Severity,
    pub description: &'static str,
    pub remediation: &'static str,
}

/// One finding about one provider.
#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    pub rule: &'static Rule,
    pub endpoint: String,
    pub operation: String,
    pub message: String,
    pub evidence: Evidence,
}

/// Probe `providers` and build the report. Errors: providers that cannot
/// be identified (exit 3), a pinned chain mismatch (exit 4), providers on
/// different chains (exit 2).
pub async fn probe(
    providers: &[Provider],
    options: &ProbeOptions,
) -> Result<ExplorationReport, EvmError> {
    let started = report::Clock::start();
    let timer = Instant::now();
    let mut runs: Vec<ProviderRun<'_>> = providers
        .iter()
        .map(|p| ProviderRun::new(Meter::new(p, options.budget), options))
        .collect();
    let identified = join_all(runs.iter_mut().map(|r| r.identify(options))).await;
    identified.into_iter().collect::<Result<Vec<()>, _>>()?;
    same_chain(&runs)?;
    join_all(runs.iter_mut().map(ProviderRun::head)).await;
    let lowest = runs.iter().filter_map(|r| r.cx.head).min();
    let block = archive_block(options.block, lowest);
    join_all(runs.iter_mut().map(|r| r.single(options, block))).await;
    let comparison = match runs.len() {
        0 | 1 => compare::Comparison::default(),
        _ => compare::compare(&mut runs, options).await,
    };
    if options.rate_discovery {
        join_all(runs.iter_mut().map(|r| rate::discover(r, options))).await;
    }
    let probed = report::Probed {
        runs: &runs,
        comparison,
        archive_block: block,
        options,
        started,
        took: timer.elapsed(),
    };
    Ok(probed.report())
}

impl ProviderRun<'_> {
    /// The checks of this provider alone.
    async fn single(&mut self, options: &ProbeOptions, archive_block: u64) {
        self.matrix(options).await;
        self.archive(archive_block).await;
        self.recent_blocks().await;
        self.subscriptions(options).await;
        self.trace(options).await;
    }
}

/// Every provider is on the same chain, or the error naming them.
fn same_chain(runs: &[ProviderRun<'_>]) -> Result<(), EvmError> {
    let mut ids: Vec<u64> = Vec::new();
    for run in runs {
        if !ids.contains(&run.chain_id) {
            ids.push(run.chain_id);
        }
    }
    if ids.len() <= 1 {
        return Ok(());
    }
    let shown: Vec<String> = ids.iter().map(u64::to_string).collect();
    Err(EvmError::config(format!(
        "providers report different chains ({})",
        shown.join(", ")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_parse() {
        let call =
            ProbeCall::parse("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48:0x18160ddd").unwrap();
        assert_eq!(call.data, [0x18, 0x16, 0x0d, 0xdd]);
        assert_eq!(
            call.tx_object()["to"],
            "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
        );
        assert_eq!(ProbeCall::parse("nope").unwrap_err().exit_code(), 2);
        assert_eq!(ProbeCall::parse("0x12:0x").unwrap_err().exit_code(), 2);
        assert_eq!(
            ProbeCall::parse("0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48:zz")
                .unwrap_err()
                .exit_code(),
            2
        );
    }
}
