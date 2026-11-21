//! The probe catalog (chain-exploration "Method support matrix"): the
//! read-only methods `rustman evm probe` calls, each with valid minimal
//! parameters built from what the provider already answered (its head, a
//! block hash, a transaction hash). It is also the probe's allowlist:
//! nothing outside it is ever sent.

use serde_json::{Value, json};

use crate::blocklist::{SEND_REFUSED, is_send_method};
use crate::error::EvmError;

/// A hash no block or transaction has, for methods that need one when the
/// provider gave none.
pub const ZERO_HASH: &str = "0x0000000000000000000000000000000000000000000000000000000000000000";

/// The default address of balance and state checks.
pub const ZERO_ADDRESS: &str = "0x0000000000000000000000000000000000000000";

/// What parameters are built from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParamContext {
    /// The provider's head, when known.
    pub head: Option<u64>,
    pub block_hash: Option<String>,
    pub tx_hash: Option<String>,
    /// The address of balance, code and state calls.
    pub address: String,
}

impl ParamContext {
    pub fn new(address: &str) -> Self {
        Self {
            head: None,
            block_hash: None,
            tx_hash: None,
            address: address.to_owned(),
        }
    }

    /// The head as a block parameter, or `latest`.
    fn block(&self) -> Value {
        self.head
            .map_or_else(|| json!("latest"), |n| json!(format!("{n:#x}")))
    }

    fn hash(&self) -> &str {
        self.block_hash.as_deref().unwrap_or(ZERO_HASH)
    }

    fn tx(&self) -> &str {
        self.tx_hash.as_deref().unwrap_or(ZERO_HASH)
    }

    fn call_object(&self) -> Value {
        json!({"to": self.address, "data": "0x"})
    }
}

/// How a method's parameters are built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Params {
    None,
    Block,
    BlockHash,
    AddressAtLatest,
    Storage,
    Call,
    Estimate,
    Logs,
    FeeHistory,
    Tx,
    DebugTraceCall,
    TraceCall,
    DebugTraceTx,
    AccessList,
    Proof,
    Simulate,
}

/// One catalog method.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProbeMethod {
    pub name: &'static str,
    /// In the `standard` set (every method is in `all`).
    pub standard: bool,
    params: Params,
}

const fn method(name: &'static str, standard: bool, params: Params) -> ProbeMethod {
    ProbeMethod {
        name,
        standard,
        params,
    }
}

/// Every method a probe may call, in the order they are probed.
pub const CATALOG: [ProbeMethod; 34] = [
    method("eth_chainId", true, Params::None),
    method("web3_clientVersion", true, Params::None),
    method("net_version", true, Params::None),
    method("eth_blockNumber", true, Params::None),
    method("eth_getBlockByNumber", true, Params::Block),
    method("eth_getBlockByHash", true, Params::BlockHash),
    method("eth_syncing", true, Params::None),
    method("eth_gasPrice", true, Params::None),
    method("eth_maxPriorityFeePerGas", true, Params::None),
    method("eth_feeHistory", true, Params::FeeHistory),
    method("eth_getBalance", true, Params::AddressAtLatest),
    method("eth_getCode", true, Params::AddressAtLatest),
    method("eth_getTransactionCount", true, Params::AddressAtLatest),
    method("eth_getStorageAt", true, Params::Storage),
    method("eth_call", true, Params::Call),
    method("eth_estimateGas", true, Params::Estimate),
    method("eth_getLogs", true, Params::Logs),
    method("eth_getTransactionByHash", true, Params::Tx),
    method("eth_getTransactionReceipt", true, Params::Tx),
    method("debug_traceCall", true, Params::DebugTraceCall),
    method("trace_call", true, Params::TraceCall),
    method("eth_getBlockReceipts", false, Params::Block),
    method("eth_getBlockTransactionCountByNumber", false, Params::Block),
    method("eth_getUncleCountByBlockNumber", false, Params::Block),
    method("eth_createAccessList", false, Params::AccessList),
    method("eth_getProof", false, Params::Proof),
    method("eth_simulateV1", false, Params::Simulate),
    method("eth_blobBaseFee", false, Params::None),
    method("debug_traceTransaction", false, Params::DebugTraceTx),
    method("trace_transaction", false, Params::Tx),
    method("trace_block", false, Params::Block),
    method("txpool_status", false, Params::None),
    method("net_listening", false, Params::None),
    method("net_peerCount", false, Params::None),
];

impl ProbeMethod {
    /// Valid minimal parameters under `cx`.
    pub fn params(&self, cx: &ParamContext) -> Value {
        match self.params {
            Params::None => json!([]),
            Params::Block => json!([cx.block(), false]),
            Params::BlockHash => json!([cx.hash(), false]),
            Params::AddressAtLatest => json!([cx.address, "latest"]),
            Params::Storage => json!([cx.address, "0x0", "latest"]),
            Params::Call => json!([cx.call_object(), "latest"]),
            Params::Estimate => json!([cx.call_object()]),
            Params::Logs => json!([{"fromBlock": cx.block(), "toBlock": cx.block()}]),
            Params::FeeHistory => json!(["0x4", "latest", [25, 75]]),
            Params::Tx => json!([cx.tx()]),
            Params::DebugTraceCall => {
                json!([cx.call_object(), "latest", {"tracer": "callTracer"}])
            }
            Params::TraceCall => json!([cx.call_object(), ["trace"], "latest"]),
            Params::DebugTraceTx => json!([cx.tx(), {"tracer": "callTracer"}]),
            Params::AccessList => json!([cx.call_object(), "latest"]),
            Params::Proof => json!([cx.address, [], "latest"]),
            Params::Simulate => {
                json!([{"blockStateCalls": [{"calls": [cx.call_object()]}]}, "latest"])
            }
        }
    }
}

/// The catalog method named `name`.
pub fn find(name: &str) -> Option<&'static ProbeMethod> {
    CATALOG.iter().find(|m| m.name == name)
}

/// Whether a probe may send `method`: a catalog method, or the
/// subscription calls of the subscription check. Never a sending method.
pub fn is_allowed(method: &str) -> bool {
    if is_send_method(method) {
        return false;
    }
    let subscription = matches!(method, "eth_subscribe" | "eth_unsubscribe");
    subscription || CATALOG.iter().any(|m| m.name == method)
}

/// `--methods`: `standard` (default), `all`, or a comma-separated list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MethodSelection {
    /// As given.
    pub spec: String,
    pub methods: Vec<&'static ProbeMethod>,
}

impl MethodSelection {
    pub fn standard() -> Self {
        Self::parse("standard").expect("standard is a selection")
    }

    /// `standard`, `all` or a list of catalog methods. A sending method is
    /// refused (exit code 4); another unknown name is a configuration
    /// error.
    pub fn parse(spec: &str) -> Result<Self, EvmError> {
        let spec = spec.trim();
        let methods = match spec {
            "standard" => CATALOG.iter().filter(|m| m.standard).collect(),
            "all" => CATALOG.iter().collect(),
            list => list
                .split(',')
                .map(str::trim)
                .filter(|m| !m.is_empty())
                .map(listed)
                .collect::<Result<Vec<_>, _>>()?,
        };
        if methods.is_empty() {
            return Err(EvmError::config("--methods: no method given"));
        }
        Ok(Self {
            spec: spec.to_owned(),
            methods,
        })
    }

    pub fn contains(&self, name: &str) -> bool {
        self.methods.iter().any(|m| m.name == name)
    }
}

fn listed(name: &str) -> Result<&'static ProbeMethod, EvmError> {
    if is_send_method(name) {
        return Err(EvmError::Refused(format!(
            "{name}: {SEND_REFUSED}; probes send read-only methods only"
        )));
    }
    find(name).ok_or_else(|| {
        let known: Vec<&str> = CATALOG.iter().map(|m| m.name).collect();
        EvmError::config(format!(
            "--methods: '{name}' is not a probe method; probe methods: {}",
            known.join(", ")
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalog_holds_only_read_methods() {
        for m in &CATALOG {
            assert!(!is_send_method(m.name), "{}", m.name);
            assert!(is_allowed(m.name), "{}", m.name);
            let ns = m.name.split('_').next().unwrap();
            assert!(
                ["eth", "net", "web3", "debug", "trace", "txpool"].contains(&ns),
                "{}",
                m.name
            );
        }
        for m in [
            "eth_sendRawTransaction",
            "eth_sendTransaction",
            "eth_sign",
            "personal_sign",
        ] {
            assert!(!is_allowed(m), "{m}");
        }
        assert!(!is_allowed("anvil_setBalance"));
        assert!(is_allowed("eth_subscribe"));
    }

    #[test]
    fn selections_parse() {
        let standard = MethodSelection::standard();
        assert!(standard.contains("debug_traceCall"));
        assert!(!standard.contains("txpool_status"));
        let all = MethodSelection::parse("all").unwrap();
        assert_eq!(all.methods.len(), CATALOG.len());
        let list = MethodSelection::parse("eth_call, eth_getBalance").unwrap();
        assert_eq!(list.methods.len(), 2);
        let refused = MethodSelection::parse("eth_call,eth_sendRawTransaction").unwrap_err();
        assert_eq!(refused.exit_code(), 4);
        assert!(refused.to_string().contains(SEND_REFUSED), "{refused}");
        let unknown = MethodSelection::parse("eth_nope").unwrap_err();
        assert_eq!(unknown.exit_code(), 2);
        let shown = unknown.to_string();
        assert!(
            shown.contains("'eth_nope' is not a probe method"),
            "{shown}"
        );
        assert!(
            shown.contains("eth_getProof") && shown.contains("txpool_status"),
            "{shown}"
        );
        assert_eq!(MethodSelection::parse(" , ").unwrap_err().exit_code(), 2);
    }

    #[test]
    fn params_use_the_context() {
        let mut cx = ParamContext::new(ZERO_ADDRESS);
        let block = find("eth_getBlockByNumber").unwrap();
        assert_eq!(block.params(&cx), json!(["latest", false]));
        cx.head = Some(255);
        cx.tx_hash = Some("0xab".into());
        assert_eq!(block.params(&cx), json!(["0xff", false]));
        let receipt = find("eth_getTransactionReceipt").unwrap();
        assert_eq!(receipt.params(&cx), json!(["0xab"]));
        let by_hash = find("eth_getBlockByHash").unwrap();
        assert_eq!(by_hash.params(&cx), json!([ZERO_HASH, false]));
    }
}
