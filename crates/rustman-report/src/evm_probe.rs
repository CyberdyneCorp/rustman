//! The EVM section of an exploration report with `subject: evm-rpc`
//! (`rustman evm probe`, add-evm-toolkit "chain-exploration"): per
//! provider, the node identity, method support, archive state, latency per
//! method, subscription support, the rate limit found, the head drift and
//! the recent blocks. Additive: OpenAPI explorations leave it out.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The `subject` of an EVM RPC probe report.
pub const SUBJECT_EVM_RPC: &str = "evm-rpc";

/// What `rustman evm probe` found.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EvmProbe {
    /// The chain every provider reported.
    pub chain_id: u64,
    /// Shown as `Name (id)`.
    pub chain: String,
    /// `--methods` as given: `standard`, `all` or a list.
    pub methods: String,
    /// The block of the archive check.
    pub archive_block: u64,
    /// The block the providers were compared at (10 below the lowest head).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub common_block: Option<u64>,
    pub providers: Vec<EvmProvider>,
}

/// One provider.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EvmProvider {
    /// The endpoint, redacted.
    pub endpoint: String,
    /// The environment it came from, when named by environment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    /// `geth 1.14.11`, or `unknown`.
    pub client: String,
    /// `archive`, `full`, `unknown` or `skipped (budget)`.
    pub archive: String,
    /// The node's answer behind `archive`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_detail: Option<String>,
    pub methods: Vec<EvmMethodSupport>,
    pub latency: Vec<EvmLatency>,
    /// `newHeads` and `logs`, for `ws://` and `wss://` providers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subscriptions: Vec<EvmSubscription>,
    /// With `--rate-discovery`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<EvmRateLimit>,
    /// The last head seen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<u64>,
    /// Blocks behind the highest provider (the least over the samples).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drift: Option<u64>,
    /// Recent blocks, oldest first (block-time and gas plots).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<EvmBlock>,
    /// The `--call` trace, when the provider traces calls.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trace: Vec<EvmTraceNode>,
    /// Requests sent to this provider.
    pub requests: u64,
    /// `--budget`.
    pub budget: u64,
    /// The budget ran out; the checks left are `skipped (budget)`.
    pub budget_exhausted: bool,
}

/// How one method answered.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EvmMethodSupport {
    pub method: String,
    /// `supported`, `unsupported`, `restricted`, `error` or
    /// `skipped (budget)`.
    pub support: String,
    /// The node's error, redacted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// The latency of one method, in milliseconds.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EvmLatency {
    pub method: String,
    pub samples: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub max_ms: f64,
}

/// One subscription check.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EvmSubscription {
    /// `newHeads` or `logs`.
    pub kind: String,
    /// `supported`, `unsupported`, `timeout` or `skipped (budget)`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// What rate discovery found (`--rate-discovery`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EvmRateLimit {
    /// The highest rate that passed (req/s): the limit when `limited_at`
    /// is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_rps: Option<u32>,
    /// The rate at which rate-limit answers (HTTP 429, `-32005`) started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limited_at: Option<u32>,
    /// The rates tried, in order.
    pub steps: Vec<u32>,
    /// The `Retry-After` honored, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
}

/// One recent block.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EvmBlock {
    pub number: u64,
    /// Unix seconds.
    pub timestamp: u64,
    pub gas_used: u64,
    pub gas_limit: u64,
}

/// One frame of a call trace, in depth-first order.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EvmTraceNode {
    pub id: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<usize>,
    /// `CALL`, `STATICCALL`, `DELEGATECALL`, ...
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gas_used: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl EvmRateLimit {
    /// `rate limit ≈ 20 req/s`, `rate limit < 5 req/s`, or `no rate limit
    /// up to 40 req/s`.
    pub fn summary(&self) -> String {
        let top = self.steps.last().copied().unwrap_or_default();
        match (self.limited_at, self.limit_rps) {
            (Some(_), Some(rps)) => format!("rate limit \u{2248} {rps} req/s"),
            (Some(at), None) => format!("rate limit < {at} req/s"),
            (None, _) => format!("no rate limit up to {top} req/s"),
        }
    }
}
