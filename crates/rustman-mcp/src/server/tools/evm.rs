//! `evm_call`, `evm_decode`, `evm_tx`, `evm_block`, `evm_logs_query` and
//! `evm_ens` (add-evm-toolkit "Read-only agent tools"). Thin adapters over
//! `rustman-evm`: every node call goes through the session's `rpc_call`
//! sender (the safety policy's allowlist and rate limit) and the JSON-RPC
//! method guards, results are redacted, and log queries span at most
//! 10,000 blocks. No tool takes a signed transaction or sends one: the
//! send-method blocklist refuses `eth_sendRawTransaction`,
//! `eth_sendTransaction` and the signing methods on every path.

use std::collections::BTreeMap;
use std::sync::Arc;

use rustman_core::model::{EvmRequest, Request};
use rustman_core::run::RequestRecord;
use rustman_core::snapshot::SnapshotMode;
use rustman_core::workspace::RequestRef;
use rustman_evm::EvmError;
use rustman_evm::abi::AbiCache;
use rustman_evm::adhoc::{AD_HOC, call_entry};
use rustman_evm::call::{BlockSel, parse_address};
use rustman_evm::codec::parse_bytes;
use rustman_evm::ens::{ReverseName, ens_for, is_ens_name};
use rustman_evm::exec::{Connected, EvmRunner};
use rustman_evm::query::{BlockView, decode_offline, fetch_block, get_logs, log_range};
use rustman_evm::rpc::{JsonrpcTransport, Rpc};
use rustman_evm::stream::{LogFilter, topic};
use rustman_evm::tx::{AbiMap, DecodeContext, TokenCache, fetch_tx, log_view};
use rustman_http::runner::{RunContext, RunOptions, Runner};
use rustman_jsonrpc::agent::Governed;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::execute::{AssertionView, history, outcome, overrides};
use super::{Call, output};
use crate::server::context::ToolOutput;
use crate::server::error::{ErrorCode, ToolError};
use crate::server::jobs::on_own_thread;

pub const CALL_TOOL: &str = "evm_call";
pub const DECODE_TOOL: &str = "evm_decode";
pub const TX_TOOL: &str = "evm_tx";
pub const BLOCK_TOOL: &str = "evm_block";
pub const LOGS_TOOL: &str = "evm_logs_query";
pub const ENS_TOOL: &str = "evm_ens";

/// Every EVM tool, in catalog order.
pub const TOOLS: [&str; 6] = [
    CALL_TOOL,
    DECODE_TOOL,
    TX_TOOL,
    BLOCK_TOOL,
    LOGS_TOOL,
    ENS_TOOL,
];

/// The default endpoint: the environment's `rpc` variable.
const DEFAULT_RPC: &str = "{{rpc}}";

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvmCallArgs {
    /// A saved `kind: evm-call` request: its id or file. Give either
    /// `path` or `to` with `abi` and `function`.
    pub path: Option<String>,
    /// The node of an ad-hoc call: a URL or `{{variable}}` (default
    /// `{{rpc}}` of the session's environment).
    pub rpc: Option<String>,
    /// The contract: an address or ENS name.
    pub to: Option<String>,
    /// `erc20`, `erc721`, `erc1155`, or a JSON ABI or Foundry/Hardhat
    /// artifact path from the workspace root.
    pub abi: Option<String>,
    /// A function name, or a full signature for overloaded functions.
    pub function: Option<String>,
    /// The function's arguments (quantities accept `1.5 ether`).
    pub args: Option<Vec<Value>>,
    /// `latest` (default), `finalized`, a number or a hash.
    pub block: Option<String>,
    /// The caller address.
    pub from: Option<String>,
    /// Also report `eth_estimateGas` for the same call.
    #[serde(default)]
    pub estimate: bool,
    /// Variables for this call, above every other source.
    pub vars: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvmDecodeArgs {
    /// `erc20`, `erc721`, `erc1155`, or an ABI path from the workspace root.
    pub abi: String,
    /// Call data, revert data or log data as 0x-prefixed hex.
    pub data: String,
    /// Log topics (topic 0 first): decode `data` as a log.
    pub topics: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvmTxArgs {
    /// The transaction hash.
    pub hash: String,
    /// The node: a URL or `{{variable}}` (default `{{rpc}}`).
    pub rpc: Option<String>,
    pub vars: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvmBlockArgs {
    /// A block number, tag (`latest`, `finalized`, ...) or hash (default
    /// `latest`).
    pub block: Option<String>,
    /// The node: a URL or `{{variable}}` (default `{{rpc}}`).
    pub rpc: Option<String>,
    pub vars: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvmLogsQueryArgs {
    /// The first block: a number or tag.
    pub from_block: String,
    /// The last block (default `latest`); at most 10000 blocks in all.
    pub to_block: Option<String>,
    /// Logs from these contracts only.
    pub addresses: Option<Vec<String>>,
    /// Topic 0 as an event signature: `Transfer(address,address,uint256)`.
    pub event: Option<String>,
    /// Further topics in order: 32-byte hex, a signature, or `*`.
    pub topics: Option<Vec<String>>,
    /// The node: a URL or `{{variable}}` (default `{{rpc}}`).
    pub rpc: Option<String>,
    pub vars: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvmEnsArgs {
    /// An ENS name to resolve, or an address to reverse-resolve (verified
    /// by a forward lookup).
    pub target: String,
    /// The node: a URL or `{{variable}}` (default `{{rpc}}`).
    pub rpc: Option<String>,
    pub vars: Option<BTreeMap<String, String>>,
}

/// A redacted result: the node's identity and what was read.
#[derive(Debug, Serialize, JsonSchema)]
pub struct EvmResult {
    /// The chain id, chain name and node client.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<Value>,
    pub result: Value,
}

/// The redacted result of `evm_call`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct EvmCallResult {
    /// The saved request's id, or `ad-hoc`.
    pub request: String,
    /// `passed`, `failed` (an assertion), `errored` or `skipped`.
    pub outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The endpoint, outputs or revert, and gas estimate.
    pub evm: Value,
    pub assertions: Vec<AssertionView>,
}

fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::new(ErrorCode::InvalidInput, message)
}

/// An [`EvmError`] as a tool error: refusals are `blocked`.
fn tool_error(error: EvmError) -> ToolError {
    match error {
        EvmError::Refused(message) => ToolError::blocked(message),
        EvmError::NotFound(message) => ToolError::new(ErrorCode::NotFound, message),
        EvmError::Config(message) => invalid(message),
        EvmError::Rpc(message) | EvmError::Interrupted(message) => ToolError::internal(message),
    }
}

/// A runner of the session (its environment, secrets and providers) and
/// an EVM runner sending through the `rpc_call` sender.
struct Session<'c> {
    call: &'c Call,
    evm: EvmRunner,
}

impl<'c> Session<'c> {
    fn new(call: &'c Call) -> Result<Self, ToolError> {
        let sender = Governed(call.ctx.rpc_sender()?);
        Ok(Self {
            call,
            evm: EvmRunner::new(Arc::new(sender)),
        })
    }

    fn runner<'r>(
        &'r self,
        environment: Option<&'r rustman_core::environment::Environment>,
        options: RunOptions<'r>,
    ) -> Result<Runner<'r>, ToolError> {
        let ctx = &self.call.ctx;
        let ws = ctx.workspace();
        let run_ctx = RunContext {
            root: &ws.root,
            settings: &ws.settings,
            environment,
            secrets: ctx.secrets(),
            in_workspace: true,
        };
        let runner =
            Runner::new(run_ctx, options).map_err(|e| ToolError::internal(e.to_string()))?;
        Ok(ctx.with_providers(runner))
    }
}

/// A node, identified, for the tools that read one directly.
struct Node {
    rpc: JsonrpcTransport,
    /// `[evm] ens_rpc`, when set, or why it did not resolve (reported by
    /// the tools that read ENS).
    ens_rpc: Option<Result<JsonrpcTransport, String>>,
    connected: Connected,
}

/// Resolve `rpc` in the session, identify the node and run `work`.
async fn with_node<T>(
    call: &Call,
    rpc: Option<&str>,
    vars: Option<&BTreeMap<String, String>>,
    work: impl AsyncFnOnce(&Node) -> Result<T, EvmError>,
) -> Result<T, ToolError> {
    let session = Session::new(call)?;
    let environment = call.ctx.environment();
    let options = RunOptions {
        overrides: overrides(vars)?,
        ..RunOptions::default()
    };
    let runner = session.runner(environment.as_ref(), options)?;
    let mut fields = Map::new();
    fields.insert("method".into(), json!("eth_chainId"));
    let rpc = rpc.unwrap_or(DEFAULT_RPC);
    let entry = rustman_jsonrpc::ad_hoc_entry(rpc, fields).map_err(|e| invalid(e.to_string()))?;
    let ws = call.ctx.workspace();
    let target = RequestRef {
        root: &ws.root,
        http: &ws.settings.http,
        entry: &entry,
        ancestors: Vec::new(),
    };
    let (endpoint, send) = runner
        .resolve_evm_endpoint(&target, rpc)
        .await
        .map_err(invalid)?;
    call.ctx.learn(&runner.known_secrets());
    let node_rpc = session.evm.transport(endpoint, &send);
    let connected = session
        .evm
        .connect(&node_rpc, &ws.settings.evm, environment.as_ref())
        .await
        .map_err(tool_error)?;
    let ens_rpc = match connected.settings.ens_rpc.as_deref() {
        Some(url) => Some(
            runner
                .resolve_evm_endpoint(&target, url)
                .await
                .map(|(endpoint, send)| session.evm.transport(endpoint, &send))
                .map_err(|e| format!("[evm] ens_rpc: {e}")),
        ),
        None => None,
    };
    call.ctx.learn(&runner.known_secrets());
    let node = Node {
        rpc: node_rpc,
        ens_rpc,
        connected,
    };
    work(&node).await.map_err(tool_error)
}

fn result(node: &Node, value: Value, line: String) -> Result<ToolOutput, ToolError> {
    let result = EvmResult {
        endpoint: Some(node.connected.identity.to_json()),
        result: value,
    };
    output(&result, line)
}

/// `evm_call`.
pub async fn evm_call(call: &Call) -> Result<ToolOutput, ToolError> {
    let call = call.clone();
    on_own_thread(move || async move { call_here(&call).await }).await?
}

async fn call_here(call: &Call) -> Result<ToolOutput, ToolError> {
    let args: EvmCallArgs = call.args()?;
    let ctx = &call.ctx;
    let ws = ctx.workspace();
    let entry = match (&args.path, &args.to) {
        (Some(_), None) => None,
        (None, Some(_)) => Some(ad_hoc(&args)?),
        _ => {
            let message =
                "give either `path` (a saved evm-call request) or `to` with `abi` and `function`";
            return Err(invalid(message));
        }
    };
    let target = match (&entry, &args.path) {
        (Some(entry), _) => RequestRef {
            root: &ws.root,
            http: &ws.settings.http,
            entry,
            ancestors: Vec::new(),
        },
        (None, path) => saved_call(call, path.as_deref().unwrap_or_default())?,
    };
    let mut session = Session::new(call)?;
    session.evm.estimate = args.estimate;
    let environment = ctx.environment();
    let options = RunOptions {
        overrides: overrides(args.vars.as_ref())?,
        snapshots: SnapshotMode::Ci,
        history: history(ctx),
        ad_hoc: entry.is_some(),
        evm: Some(&session.evm),
        ..RunOptions::default()
    };
    let runner = session.runner(environment.as_ref(), options)?;
    let sent = runner.send(&target, 1).await;
    if let Err(err) = runner.finish() {
        tracing::warn!("cookies: {err}");
    }
    ctx.learn(&runner.known_secrets());
    let result = call_result(&sent.record);
    if let Some(kind) = result.evm["error"]["kind"].as_str()
        && kind == "refused"
    {
        return Err(ToolError::blocked(result.error.clone().unwrap_or_default()));
    }
    let line = match &result.error {
        Some(error) => format!("{} {}: {error}", result.request, result.outcome),
        None => format!("{} {}", result.request, result.outcome),
    };
    output(&result, line)
}

/// The saved `evm-call` request `path`.
fn saved_call<'c>(call: &'c Call, path: &str) -> Result<RequestRef<'c>, ToolError> {
    let saved = super::workspace::find(&call.ctx, path)?;
    let is_call =
        matches!(saved.request(), Request::Evm(evm) if matches!(**evm, EvmRequest::Call(_)));
    if !is_call {
        return Err(invalid(format!("'{path}' is not an evm-call request")));
    }
    Ok(saved)
}

fn ad_hoc(args: &EvmCallArgs) -> Result<rustman_core::collection::RequestEntry, ToolError> {
    let (Some(to), Some(abi), Some(function)) = (&args.to, &args.abi, &args.function) else {
        return Err(invalid("an ad-hoc call needs `to`, `abi` and `function`"));
    };
    let mut fields = Map::new();
    fields.insert("to".into(), json!(to));
    fields.insert("abi".into(), json!(abi));
    fields.insert("function".into(), json!(function));
    fields.insert("args".into(), json!(args.args.clone().unwrap_or_default()));
    if let Some(block) = &args.block {
        fields.insert("block".into(), json!(block));
    }
    if let Some(from) = &args.from {
        fields.insert("from".into(), json!(from));
    }
    let rpc = args.rpc.as_deref().unwrap_or(DEFAULT_RPC);
    call_entry(rpc, fields).map_err(|e| invalid(e.to_string()))
}

fn call_result(record: &RequestRecord) -> EvmCallResult {
    let mut evm = record.evm.clone().unwrap_or(Value::Null);
    if let Value::Object(map) = &mut evm {
        map.remove("rendered");
    }
    EvmCallResult {
        request: record.id.clone(),
        outcome: outcome(record.status).to_owned(),
        error: record.error.clone(),
        evm,
        assertions: record
            .assertions
            .iter()
            .map(|a| AssertionView {
                passed: a.passed(),
                text: a.to_string(),
            })
            .collect(),
    }
}

/// `evm_decode`: offline.
pub fn evm_decode(call: &Call) -> Result<ToolOutput, ToolError> {
    let args: EvmDecodeArgs = call.args()?;
    let ws = call.ctx.workspace();
    let anchor = std::path::Path::new(AD_HOC);
    let decode = || -> Result<_, EvmError> {
        let abi = AbiCache::new().load(&ws.root, anchor, &args.abi)?;
        let data = parse_bytes(&args.data)?;
        let topics = args
            .topics
            .iter()
            .flatten()
            .map(|t| topic(t))
            .collect::<Result<Vec<_>, _>>()?;
        decode_offline(&abi.abi, &data, &topics)
    };
    let decoded = decode().map_err(tool_error)?;
    let result = EvmResult {
        endpoint: None,
        result: decoded.to_json(),
    };
    output(&result, decoded.render())
}

/// `evm_tx`.
pub async fn evm_tx(call: &Call) -> Result<ToolOutput, ToolError> {
    let call = call.clone();
    on_own_thread(move || async move {
        let args: EvmTxArgs = call.args()?;
        with_node(
            &call,
            args.rpc.as_deref(),
            args.vars.as_ref(),
            async |node| {
                let abis = AbiMap::new();
                let tokens = TokenCache::new();
                let ctx = DecodeContext {
                    rpc: &node.rpc,
                    chain: node.connected.identity.chain.clone(),
                    abis: &abis,
                    tokens: &tokens,
                };
                let report = fetch_tx(&ctx, &args.hash).await?;
                Ok(result(node, report.to_json(), report.render()))
            },
        )
        .await?
    })
    .await?
}

/// `evm_block`.
pub async fn evm_block(call: &Call) -> Result<ToolOutput, ToolError> {
    let call = call.clone();
    on_own_thread(move || async move {
        let args: EvmBlockArgs = call.args()?;
        with_node(
            &call,
            args.rpc.as_deref(),
            args.vars.as_ref(),
            async |node| {
                let block = BlockSel::parse(args.block.as_deref().unwrap_or("latest"))?;
                let view = BlockView::of(&fetch_block(&node.rpc, &block).await?);
                Ok(result(node, view.to_json(), view.render()))
            },
        )
        .await?
    })
    .await?
}

/// `evm_logs_query`: a bounded range, checked before any log is asked.
pub async fn evm_logs_query(call: &Call) -> Result<ToolOutput, ToolError> {
    let call = call.clone();
    on_own_thread(move || async move {
        let args: EvmLogsQueryArgs = call.args()?;
        let from = BlockSel::parse(&args.from_block).map_err(tool_error)?;
        let to =
            BlockSel::parse(args.to_block.as_deref().unwrap_or("latest")).map_err(tool_error)?;
        if let (BlockSel::Number(first), BlockSel::Number(last)) = (&from, &to) {
            rustman_evm::query::check_range(*first, *last).map_err(tool_error)?;
        }
        let filter = LogFilter::parse(
            &args.addresses.clone().unwrap_or_default(),
            args.event.as_deref(),
            &args.topics.clone().unwrap_or_default(),
        )
        .map_err(tool_error)?;
        with_node(
            &call,
            args.rpc.as_deref(),
            args.vars.as_ref(),
            async |node| {
                let (first, last) = log_range(&node.rpc, &from, &to).await?;
                let logs = get_logs(&node.rpc, &filter, first, last).await?;
                let decoded = decode_logs(node, &logs).await?;
                let line = format!("{} log(s) in blocks {first}..={last}", decoded.len());
                let value = json!({"from_block": first, "to_block": last, "logs": decoded});
                Ok(result(node, value, line))
            },
        )
        .await?
    })
    .await?
}

async fn decode_logs(node: &Node, logs: &[Value]) -> Result<Vec<Value>, EvmError> {
    let abis = AbiMap::new();
    let tokens = TokenCache::new();
    let ctx = DecodeContext {
        rpc: &node.rpc,
        chain: node.connected.identity.chain.clone(),
        abis: &abis,
        tokens: &tokens,
    };
    let mut decoded = Vec::with_capacity(logs.len());
    let mut warnings = Vec::new();
    for log in logs {
        decoded.push(log_view(&ctx, log, &mut warnings).await?.to_json());
    }
    Ok(decoded)
}

/// `evm_ens`.
pub async fn evm_ens(call: &Call) -> Result<ToolOutput, ToolError> {
    let call = call.clone();
    on_own_thread(move || async move {
        let args: EvmEnsArgs = call.args()?;
        with_node(
            &call,
            args.rpc.as_deref(),
            args.vars.as_ref(),
            async |node| {
                let configured = match &node.ens_rpc {
                    Some(Ok(rpc)) => Some(rpc as &dyn Rpc),
                    Some(Err(reason)) => return Err(EvmError::config(reason.clone())),
                    None => None,
                };
                let ens = ens_for(&node.rpc, node.connected.identity.chain.id, configured)?;
                if is_ens_name(&args.target) {
                    let address = ens.resolve(&args.target).await?.to_checksum(None);
                    let value = json!({"name": args.target, "address": address});
                    return Ok(result(node, value, address));
                }
                let address = parse_address(&args.target)
                    .map_err(|e| EvmError::config(format!("target: {e}")))?;
                let name = match ens.reverse(address).await? {
                    ReverseName::Verified(name) => Some(name),
                    ReverseName::Unverified(_) | ReverseName::None => None,
                };
                let line = name
                    .clone()
                    .unwrap_or_else(|| "no verified primary name".to_owned());
                let value = json!({"address": address.to_checksum(None), "name": name});
                Ok(result(node, value, line))
            },
        )
        .await?
    })
    .await?
}

/// Run an EVM tool; `None` when `name` is not one.
pub async fn dispatch(name: &str, call: &Call) -> Option<Result<ToolOutput, ToolError>> {
    let result = match name {
        CALL_TOOL => evm_call(call).await,
        DECODE_TOOL => evm_decode(call),
        TX_TOOL => evm_tx(call).await,
        BLOCK_TOOL => evm_block(call).await,
        LOGS_TOOL => evm_logs_query(call).await,
        ENS_TOOL => evm_ens(call).await,
        _ => return None,
    };
    Some(result)
}
