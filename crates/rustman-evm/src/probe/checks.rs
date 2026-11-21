//! The checks of one provider: identity, method support, archive state,
//! recent blocks, subscriptions and the `--call` trace.

use std::collections::BTreeMap;
use std::time::Duration;

use rustman_report::exploration::{EvmBlock, EvmRateLimit, EvmSubscription, EvmTraceNode};
use serde_json::{Value, json};

use super::catalog::ParamContext;
use super::meter::{Answer, Meter, ProbeFailure, truncate};
use super::support::{Archive, Support, archive_of, classify};
use super::{Check, CheckOutcome, ProbeOptions};
use crate::error::EvmError;
use crate::identity::{NodeClient, Pin};
use crate::quantity::parse_hex;
use crate::rpc::{Rpc, RpcFailure, RpcFuture};
use crate::stream::{StreamConfig, StreamKind, item_channel, stream};
use crate::trace::{CallFrame, TraceResult, TraceSupport, trace_call};

/// The detail of a `logs` subscription that was accepted but delivered
/// no log within the subscription timeout.
pub const NO_LOG_YET: &str = "subscribed; no log arrived before the timeout";

/// Recent blocks fetched for the block-time and gas plots.
pub const RECENT_BLOCKS: u64 = 8;

/// The default block of the archive check.
pub const DEFAULT_ARCHIVE_BLOCK: u64 = 1_000_000;

/// How far below the head the archive check stays at least.
pub const ARCHIVE_HEAD_MARGIN: u64 = 10_000;

/// What one provider answered.
pub struct ProviderRun<'p> {
    pub meter: Meter<'p>,
    pub chain_id: u64,
    pub client: NodeClient,
    pub cx: ParamContext,
    /// Answers already received, reused by the method matrix.
    answers: BTreeMap<&'static str, Answer>,
    pub methods: Vec<(&'static str, Support, Option<String>)>,
    pub archive: Archive,
    pub archive_detail: Option<String>,
    pub blocks: Vec<EvmBlock>,
    pub subscriptions: Vec<EvmSubscription>,
    pub trace: Vec<EvmTraceNode>,
    pub rate_limit: Option<EvmRateLimit>,
    pub checks: Vec<Check>,
}

impl<'p> ProviderRun<'p> {
    pub fn new(meter: Meter<'p>, options: &ProbeOptions) -> Self {
        Self {
            meter,
            chain_id: 0,
            client: NodeClient::unknown(),
            cx: ParamContext::new(&options.address),
            answers: BTreeMap::new(),
            methods: Vec::new(),
            archive: Archive::Unknown,
            archive_detail: None,
            blocks: Vec::new(),
            subscriptions: Vec::new(),
            trace: Vec::new(),
            rate_limit: None,
            checks: Vec::new(),
        }
    }

    pub fn endpoint(&self) -> &str {
        &self.meter.provider.endpoint
    }

    /// Record a check of this provider.
    pub fn check(&mut self, operation: &str, outcome: CheckOutcome, reason: Option<String>) {
        let endpoint = self.endpoint().to_owned();
        self.checks.push(Check {
            endpoint,
            operation: operation.to_owned(),
            outcome,
            reason,
        });
    }

    /// `eth_chainId`, checked against the environment's pin.
    pub async fn identify(&mut self, options: &ProbeOptions) -> Result<(), EvmError> {
        let answer = self.ask("eth_chainId", json!([])).await;
        let id = answer.as_ref().ok().and_then(quantity);
        let Some(id) = id else {
            let detail = answer.err().map_or("not a quantity".into(), |f| f.detail());
            return Err(EvmError::Rpc(format!(
                "{}: eth_chainId: {detail}",
                self.endpoint()
            )));
        };
        let provider = self.meter.provider;
        if let (Some(chain_id), Some(environment)) = (provider.pin, &provider.environment) {
            let pin = Pin {
                environment,
                chain_id,
            };
            pin.check(id, &options.registry)?;
        }
        self.chain_id = id;
        Ok(())
    }

    /// The head and its block, which the other checks build on.
    pub async fn head(&mut self) {
        let head = self.ask("eth_blockNumber", json!([])).await;
        self.cx.head = head.as_ref().ok().and_then(quantity);
        let Some(number) = self.cx.head else {
            return;
        };
        let block = self
            .ask(
                "eth_getBlockByNumber",
                json!([format!("{number:#x}"), false]),
            )
            .await;
        if let Ok(block) = &block {
            self.cx.block_hash = block["hash"].as_str().map(str::to_owned);
            self.cx.tx_hash = block["transactions"][0].as_str().map(str::to_owned);
            self.blocks.extend(block_of(block));
        }
    }

    /// Call `method` once and keep its answer for the matrix.
    async fn ask(&mut self, method: &'static str, params: Value) -> Answer {
        let answer = self.meter.call(method, params).await;
        self.answers.insert(method, answer.clone());
        answer
    }

    /// The method matrix: every selected method, classified.
    pub async fn matrix(&mut self, options: &ProbeOptions) {
        for method in &options.methods.methods {
            let answer = match self.answers.get(method.name) {
                Some(answer) => answer.clone(),
                None => self.ask(method.name, method.params(&self.cx)).await,
            };
            if method.name == "web3_clientVersion"
                && let Ok(Value::String(version)) = &answer
            {
                self.client = NodeClient::parse(version);
            }
            let (support, detail) = classify(&answer);
            self.methods.push((method.name, support, detail.clone()));
            let outcome = match support {
                Support::Supported | Support::Unsupported | Support::Restricted => {
                    CheckOutcome::Passed
                }
                Support::Error => CheckOutcome::Failed,
                Support::SkippedBudget => CheckOutcome::SkippedBudget,
            };
            let reason = (support != Support::Supported).then(|| support.to_string());
            self.check(method.name, outcome, reason.or(detail));
        }
    }

    /// `eth_getBalance` of the address at `block`.
    pub async fn archive(&mut self, block: u64) {
        let params = json!([self.cx.address, format!("{block:#x}")]);
        let answer = self.meter.call("eth_getBalance", params).await;
        self.archive = archive_of(&answer);
        self.archive_detail = answer.as_ref().err().map(ProbeFailure::detail);
        let outcome = match self.archive {
            Archive::SkippedBudget => CheckOutcome::SkippedBudget,
            _ => CheckOutcome::Passed,
        };
        self.check(
            "archive",
            outcome,
            Some(format!("{} at block {block}", self.archive.as_str())),
        );
    }

    /// The blocks before the head, for block times and gas.
    pub async fn recent_blocks(&mut self) {
        let Some(head) = self.cx.head else {
            return;
        };
        let first = head.saturating_sub(RECENT_BLOCKS - 1);
        let mut blocks = Vec::new();
        for number in first..head {
            let params = json!([format!("{number:#x}"), false]);
            match self.meter.call("eth_getBlockByNumber", params).await {
                Ok(block) => blocks.extend(block_of(&block)),
                Err(_) => break,
            }
        }
        blocks.append(&mut self.blocks);
        self.blocks = blocks;
    }

    /// `newHeads` and `logs` subscriptions, for WebSocket providers.
    pub async fn subscriptions(&mut self, options: &ProbeOptions) {
        if self.meter.provider.stream.is_none() {
            return;
        }
        for (name, kind) in [
            ("newHeads", StreamKind::Heads),
            ("logs", StreamKind::Logs(Default::default())),
        ] {
            let (status, detail) = self.subscribe(&kind, options).await;
            let outcome = match status {
                "skipped (budget)" => CheckOutcome::SkippedBudget,
                _ => CheckOutcome::Passed,
            };
            self.check(
                &format!("eth_subscribe {name}"),
                outcome,
                Some(status.into()),
            );
            self.subscriptions.push(EvmSubscription {
                kind: name.to_owned(),
                status: status.to_owned(),
                detail,
            });
        }
    }

    async fn subscribe(
        &self,
        kind: &StreamKind,
        options: &ProbeOptions,
    ) -> (&'static str, Option<String>) {
        let Some(transport) = &self.meter.provider.stream else {
            return ("unsupported", None);
        };
        // The subscribe and unsubscribe calls.
        if !self.meter.reserve(2) {
            return ("skipped (budget)", None);
        }
        let _slot = match self.meter.provider.gate_ws().await {
            Ok(slot) => slot,
            Err(refusal) => return ("unsupported", Some(refusal)),
        };
        let config = StreamConfig {
            max_events: Some(1),
            max_duration: Some(options.timing.subscription_timeout),
            cancel: options.cancel.child_token(),
            ..StreamConfig::default()
        };
        let (items, _received) = item_channel();
        match stream(transport, kind, &config, &items).await {
            Ok(summary) if summary.events > 0 => ("supported", None),
            // Logs can be rare: an accepted subscription is support, and
            // the detail says no log was seen (never `timeout`).
            Ok(_) if matches!(kind, StreamKind::Logs(_)) => {
                ("supported", Some(NO_LOG_YET.to_owned()))
            }
            Ok(_) => ("timeout", None),
            Err(error) => ("unsupported", Some(error.to_string())),
        }
    }

    /// Trace `--call` (when given) for the call-trace graph.
    pub async fn trace(&mut self, options: &ProbeOptions) {
        let Some(call) = &options.call else {
            return;
        };
        let request = call.request();
        let result = trace_call(&self.meter, &request, &self.client, &TraceSupport::new()).await;
        let reason = match result {
            TraceResult::Tree(root) => {
                self.trace = trace_nodes(&root);
                None
            }
            TraceResult::Unsupported(note) | TraceResult::Failed(note) => Some(note),
        };
        self.check("trace --call", CheckOutcome::Passed, reason);
    }
}

impl Rpc for Meter<'_> {
    fn endpoint(&self) -> String {
        self.provider.endpoint.clone()
    }

    fn call<'a>(&'a self, method: &'a str, params: Value) -> RpcFuture<'a> {
        Box::pin(async move {
            Meter::call(self, method, params)
                .await
                .map_err(|failure| match failure {
                    ProbeFailure::Budget => RpcFailure::Refused(failure.detail()),
                    ProbeFailure::Rpc(rpc) => rpc,
                })
        })
    }
}

/// The block of the archive check: `--block` (default 1,000,000), at
/// least 10,000 blocks below the lowest head.
pub fn archive_block(requested: Option<u64>, lowest_head: Option<u64>) -> u64 {
    let block = requested.unwrap_or(DEFAULT_ARCHIVE_BLOCK);
    lowest_head.map_or(block, |head| {
        block.min(head.saturating_sub(ARCHIVE_HEAD_MARGIN))
    })
}

/// A hex quantity as an integer.
pub fn quantity(value: &Value) -> Option<u64> {
    let text = value.as_str()?;
    let number = parse_hex(text).ok()?;
    u64::try_from(number).ok()
}

fn block_of(block: &Value) -> Option<EvmBlock> {
    Some(EvmBlock {
        number: quantity(&block["number"])?,
        timestamp: quantity(&block["timestamp"]).unwrap_or_default(),
        gas_used: quantity(&block["gasUsed"]).unwrap_or_default(),
        gas_limit: quantity(&block["gasLimit"]).unwrap_or_default(),
    })
}

/// Frames kept in the call-trace graph; a provider's larger trace is cut
/// and ends in one marker node counting the frames left out.
pub const MAX_TRACE_NODES: usize = 1000;

/// Bytes kept of a frame's provider-supplied `kind` and `error` text.
const MAX_FRAME_TEXT: usize = 256;

/// The frames of a trace in depth-first order, at most
/// [`MAX_TRACE_NODES`] of them plus, when frames were cut, a marker node
/// (`… N more frames omitted`, child of the root).
pub fn trace_nodes(root: &CallFrame) -> Vec<EvmTraceNode> {
    let mut nodes = Vec::new();
    let mut stack = vec![(root, None)];
    while let Some((frame, parent)) = stack.pop() {
        if nodes.len() == MAX_TRACE_NODES {
            stack.push((frame, parent));
            break;
        }
        let id = nodes.len();
        nodes.push(EvmTraceNode {
            id,
            parent,
            kind: truncate(frame.kind.clone(), MAX_FRAME_TEXT),
            to: frame.to.map(|a| a.to_checksum(None)),
            gas_used: frame.gas_used.and_then(|g| u64::try_from(g).ok()),
            error: frame.error.clone().map(|e| truncate(e, MAX_FRAME_TEXT)),
        });
        stack.extend(frame.calls.iter().rev().map(|c| (c, Some(id))));
    }
    let omitted = count_frames(stack.into_iter().map(|(frame, _)| frame));
    if omitted > 0 {
        let frames = if omitted == 1 { "frame" } else { "frames" };
        nodes.push(EvmTraceNode {
            id: nodes.len(),
            parent: Some(0),
            kind: format!("\u{2026} {omitted} more {frames} omitted"),
            ..EvmTraceNode::default()
        });
    }
    nodes
}

/// The frames under (and including) `roots`.
fn count_frames<'a>(roots: impl Iterator<Item = &'a CallFrame>) -> usize {
    let mut stack: Vec<&CallFrame> = roots.collect();
    let mut count = 0;
    while let Some(frame) = stack.pop() {
        count += 1;
        stack.extend(frame.calls.iter());
    }
    count
}

/// A pause that ends early when `cancel` fires; `false` when it did.
pub async fn pause(duration: Duration, cancel: &tokio_util::sync::CancellationToken) -> bool {
    tokio::select! {
        () = cancel.cancelled() => false,
        () = tokio::time::sleep(duration) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_archive_block_stays_below_the_head() {
        assert_eq!(archive_block(None, Some(20_000_000)), 1_000_000);
        assert_eq!(archive_block(Some(5), Some(20_000_000)), 5);
        assert_eq!(archive_block(None, Some(500_000)), 490_000);
        assert_eq!(archive_block(None, Some(100)), 0);
        assert_eq!(archive_block(Some(7), None), 7);
    }

    #[test]
    fn traces_flatten_depth_first() {
        let leaf = |kind: &str| CallFrame {
            kind: kind.into(),
            ..CallFrame::default()
        };
        let root = CallFrame {
            kind: "CALL".into(),
            calls: vec![
                CallFrame {
                    kind: "STATICCALL".into(),
                    calls: vec![leaf("DELEGATECALL")],
                    ..CallFrame::default()
                },
                leaf("CALL"),
            ],
            ..CallFrame::default()
        };
        let nodes = trace_nodes(&root);
        let shape: Vec<(usize, Option<usize>, &str)> = nodes
            .iter()
            .map(|n| (n.id, n.parent, n.kind.as_str()))
            .collect();
        assert_eq!(
            shape,
            [
                (0, None, "CALL"),
                (1, Some(0), "STATICCALL"),
                (2, Some(1), "DELEGATECALL"),
                (3, Some(0), "CALL"),
            ]
        );
    }

    #[test]
    fn hostile_traces_are_capped() {
        let wide = CallFrame {
            kind: "CALL".into(),
            calls: vec![
                CallFrame {
                    kind: "CALL".into(),
                    error: Some("e".repeat(100_000)),
                    ..CallFrame::default()
                };
                MAX_TRACE_NODES * 3
            ],
            ..CallFrame::default()
        };
        let nodes = trace_nodes(&wide);
        assert_eq!(nodes.len(), MAX_TRACE_NODES + 1);
        let error = nodes[1].error.as_deref().unwrap_or_default();
        assert!(error.len() < MAX_FRAME_TEXT + 32, "{}", error.len());
        let marker = nodes.last().expect("marker");
        let omitted = MAX_TRACE_NODES * 3 + 1 - MAX_TRACE_NODES;
        assert_eq!(
            marker.kind,
            format!("\u{2026} {omitted} more frames omitted")
        );
        assert_eq!((marker.id, marker.parent), (MAX_TRACE_NODES, Some(0)));
    }

    #[test]
    fn small_traces_have_no_marker() {
        let nodes = trace_nodes(&CallFrame::default());
        assert_eq!(nodes.len(), 1);
        assert!(!nodes[0].kind.contains("omitted"));
    }

    #[test]
    fn quantities_parse() {
        assert_eq!(quantity(&json!("0x10")), Some(16));
        assert_eq!(quantity(&json!(16)), None);
    }
}
