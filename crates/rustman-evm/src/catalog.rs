//! The built-in catalog of standard `eth_*`, `net_*` and `web3_*` methods
//! (evm-toolkit "Method catalog with typed parameters"): each method's
//! param names and types (address, quantity, data, block tag, hash, filter
//! object, transaction object, bool).
//!
//! The catalog builds the params of `rustman rpc call <url> <method>
//! --param name=value`, checks the params of every call to a catalog
//! method before it is sent (a mixed-case address with a wrong EIP-55
//! checksum, non-hex data, an unknown block tag), names the typed params
//! in the TUI, and says which results are amounts of the native currency
//! (`eth_getBalance` is shown as `1 ETH`). Params of methods outside the
//! catalog, and params given by name, are not checked.

use std::sync::{Arc, Once};

use alloy_primitives::Address;
use rustman_jsonrpc::guard::{MethodGuard, register_guard};
use serde_json::{Map, Value, json};

use crate::chain::ChainRegistry;
use crate::load::BLOCK_TAGS;
use crate::quantity::{Denomination, Form, Quantity, forms};
use crate::settings::EvmSettings;

/// The type of one param.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamType {
    /// 20 bytes of hex; a mixed-case one must carry a valid EIP-55
    /// checksum.
    Address,
    /// A `0x` hex integer.
    Quantity,
    /// `0x` and an even number of hex digits.
    Data,
    /// A block: a tag (`latest`, ...), a quantity, a block hash or an
    /// EIP-1898 object.
    Block,
    /// 32 bytes of hex.
    Hash,
    /// An `eth_getLogs` filter object.
    Filter,
    /// A call object (`to`, `data`, `value`, ...).
    Transaction,
    Bool,
    /// Any JSON (a list of percentiles, storage keys).
    Json,
}

impl ParamType {
    /// The name shown in typed forms and messages.
    pub fn label(self) -> &'static str {
        match self {
            Self::Address => "address",
            Self::Quantity => "quantity",
            Self::Data => "data",
            Self::Block => "block tag",
            Self::Hash => "hash",
            Self::Filter => "filter object",
            Self::Transaction => "transaction object",
            Self::Bool => "bool",
            Self::Json => "json",
        }
    }
}

/// One param of a catalog method.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Param {
    pub name: &'static str,
    pub ty: ParamType,
    pub required: bool,
}

/// One method of the catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Method {
    pub name: &'static str,
    pub params: &'static [Param],
    /// The result is an amount of the native currency (`eth_getBalance`,
    /// gas prices and fees).
    pub native_result: bool,
}

impl Method {
    /// `eth_getBalance(address: address, block: block tag)`; optional
    /// params end with `?`.
    pub fn signature(&self) -> String {
        let params: Vec<String> = self
            .params
            .iter()
            .map(|p| {
                let optional = if p.required { "" } else { "?" };
                format!("{}{optional}: {}", p.name, p.ty.label())
            })
            .collect();
        format!("{}({})", self.name, params.join(", "))
    }
}

const fn req(name: &'static str, ty: ParamType) -> Param {
    Param {
        name,
        ty,
        required: true,
    }
}

const fn opt(name: &'static str, ty: ParamType) -> Param {
    Param {
        name,
        ty,
        required: false,
    }
}

const fn method(name: &'static str, params: &'static [Param]) -> Method {
    Method {
        name,
        params,
        native_result: false,
    }
}

const fn native(name: &'static str) -> Method {
    Method {
        name,
        params: &[],
        native_result: true,
    }
}

use ParamType::{Address as A, Block as B, Bool, Data as D, Filter as F, Hash as H, Json};
use ParamType::{Quantity as Q, Transaction as T};

/// The catalog, in the order a picker shows it.
pub const METHODS: &[Method] = &[
    Method {
        name: "eth_getBalance",
        params: &[req("address", A), req("block", B)],
        native_result: true,
    },
    method(
        "eth_getTransactionCount",
        &[req("address", A), req("block", B)],
    ),
    method("eth_getCode", &[req("address", A), req("block", B)]),
    method(
        "eth_getStorageAt",
        &[req("address", A), req("position", Q), req("block", B)],
    ),
    method("eth_call", &[req("transaction", T), opt("block", B)]),
    method("eth_estimateGas", &[req("transaction", T), opt("block", B)]),
    method(
        "eth_createAccessList",
        &[req("transaction", T), opt("block", B)],
    ),
    method(
        "eth_getBlockByNumber",
        &[req("block", B), req("full", Bool)],
    ),
    method("eth_getBlockByHash", &[req("hash", H), req("full", Bool)]),
    method("eth_getBlockTransactionCountByNumber", &[req("block", B)]),
    method("eth_getBlockTransactionCountByHash", &[req("hash", H)]),
    method("eth_getBlockReceipts", &[req("block", B)]),
    method("eth_getTransactionByHash", &[req("hash", H)]),
    method("eth_getTransactionReceipt", &[req("hash", H)]),
    method(
        "eth_getTransactionByBlockHashAndIndex",
        &[req("hash", H), req("index", Q)],
    ),
    method(
        "eth_getTransactionByBlockNumberAndIndex",
        &[req("block", B), req("index", Q)],
    ),
    method("eth_getLogs", &[req("filter", F)]),
    method(
        "eth_getProof",
        &[req("address", A), req("keys", Json), req("block", B)],
    ),
    method(
        "eth_feeHistory",
        &[req("count", Q), req("newest", B), opt("percentiles", Json)],
    ),
    method("eth_newFilter", &[req("filter", F)]),
    method("eth_newBlockFilter", &[]),
    method("eth_getFilterChanges", &[req("id", Q)]),
    method("eth_getFilterLogs", &[req("id", Q)]),
    method("eth_uninstallFilter", &[req("id", Q)]),
    method("eth_chainId", &[]),
    method("eth_blockNumber", &[]),
    native("eth_gasPrice"),
    native("eth_maxPriorityFeePerGas"),
    native("eth_blobBaseFee"),
    method("eth_syncing", &[]),
    method("eth_accounts", &[]),
    method("net_version", &[]),
    method("net_listening", &[]),
    method("net_peerCount", &[]),
    method("web3_clientVersion", &[]),
    method("web3_sha3", &[req("data", D)]),
];

/// The catalog method named `name`.
pub fn find(name: &str) -> Option<&'static Method> {
    METHODS.iter().find(|m| m.name == name.trim())
}

/// `param '<name>': <reason>`, the message of every rejected value.
fn param_error(name: &str, reason: &str) -> String {
    format!("param '{name}': {reason}")
}

/// Check the params of a call to `method` before it is sent. Methods
/// outside the catalog and params given by name (an object) pass; so do
/// params after the catalog's last one.
pub fn check_params(method: &str, params: Option<&Value>) -> Result<(), String> {
    let Some(spec) = find(method) else {
        return Ok(());
    };
    let Some(Value::Array(values)) = params else {
        return Ok(());
    };
    for (param, value) in spec.params.iter().zip(values) {
        check_value(param.ty, value).map_err(|reason| param_error(param.name, &reason))?;
    }
    Ok(())
}

/// Why `value` is not a valid `ty`, if it is not.
pub fn check_value(ty: ParamType, value: &Value) -> Result<(), String> {
    match ty {
        ParamType::Bool => expect(value.is_boolean(), "expected true or false"),
        ParamType::Json => Ok(()),
        ParamType::Filter => check_filter(value),
        ParamType::Transaction => check_transaction(value),
        ParamType::Block => check_block(value),
        _ => check_text(ty, as_text(value)?),
    }
}

fn expect(ok: bool, reason: &str) -> Result<(), String> {
    if ok { Ok(()) } else { Err(reason.to_owned()) }
}

fn as_text(value: &Value) -> Result<&str, String> {
    value
        .as_str()
        .ok_or_else(|| format!("expected a string, got {value}"))
}

/// A hex string param: address, quantity, data or hash.
fn check_text(ty: ParamType, text: &str) -> Result<(), String> {
    match ty {
        ParamType::Address => check_address(text),
        ParamType::Quantity => expect(is_hex(text, None, false), "not a hex quantity (0x…)"),
        ParamType::Hash => expect(is_hex(text, Some(32), true), "not a 32-byte hex hash"),
        _ => expect(
            is_hex(text, None, true),
            "not hex data (0x and an even number of hex digits)",
        ),
    }
}

/// `0x` and hex digits: exactly `bytes` bytes, else any count (an even
/// one when `whole_bytes`, at least one digit otherwise).
fn is_hex(text: &str, bytes: Option<usize>, whole_bytes: bool) -> bool {
    let Some(digits) = text.strip_prefix("0x") else {
        return false;
    };
    let hex = digits.bytes().all(|b| b.is_ascii_hexdigit());
    let length = match bytes {
        Some(n) => digits.len() == n * 2,
        None if whole_bytes => digits.len() % 2 == 0,
        None => !digits.is_empty(),
    };
    hex && length
}

/// 20 bytes of hex; when it mixes cases, its EIP-55 checksum must hold.
pub fn check_address(text: &str) -> Result<(), String> {
    if !is_hex(text, Some(20), true) {
        return Err("not a 20-byte hex address".to_owned());
    }
    let digits = &text[2..];
    let mixed = digits.bytes().any(|b| b.is_ascii_uppercase())
        && digits.bytes().any(|b| b.is_ascii_lowercase());
    if mixed && Address::parse_checksummed(text, None).is_err() {
        return Err("invalid EIP-55 checksum".to_owned());
    }
    Ok(())
}

fn check_block(value: &Value) -> Result<(), String> {
    match value {
        Value::Object(fields) => check_fields(
            fields,
            &[
                ("blockHash", ParamType::Hash),
                ("blockNumber", ParamType::Quantity),
            ],
        ),
        Value::String(text) if is_hex(text, None, false) => Ok(()),
        Value::String(text) if BLOCK_TAGS.contains(&text.as_str()) => Ok(()),
        Value::String(text) => Err(format!(
            "unknown block tag '{text}' (use {}, a 0x number or a block hash)",
            BLOCK_TAGS.join(", ")
        )),
        other => Err(format!("expected a block tag, got {other}")),
    }
}

fn check_filter(value: &Value) -> Result<(), String> {
    let fields = value.as_object().ok_or("expected a filter object")?;
    if let Some(address) = fields.get("address") {
        match address {
            Value::Array(list) => list
                .iter()
                .try_for_each(|a| check_value(ParamType::Address, a))?,
            single => check_value(ParamType::Address, single)?,
        }
    }
    check_fields(
        fields,
        &[
            ("fromBlock", ParamType::Block),
            ("toBlock", ParamType::Block),
            ("blockHash", ParamType::Hash),
        ],
    )
}

fn check_transaction(value: &Value) -> Result<(), String> {
    let fields = value.as_object().ok_or("expected a transaction object")?;
    check_fields(
        fields,
        &[
            ("from", ParamType::Address),
            ("to", ParamType::Address),
            ("data", ParamType::Data),
            ("input", ParamType::Data),
            ("value", ParamType::Quantity),
            ("gas", ParamType::Quantity),
            ("gasPrice", ParamType::Quantity),
            ("maxFeePerGas", ParamType::Quantity),
            ("maxPriorityFeePerGas", ParamType::Quantity),
        ],
    )
}

/// Check the fields of an object that are present: `<field>: <reason>`.
fn check_fields(fields: &Map<String, Value>, typed: &[(&str, ParamType)]) -> Result<(), String> {
    for (name, ty) in typed {
        let Some(value) = fields.get(*name).filter(|v| !v.is_null()) else {
            continue;
        };
        check_value(*ty, value).map_err(|reason| format!("{name}: {reason}"))?;
    }
    Ok(())
}

/// The params of a call to `method` from `name=value` pairs, in catalog
/// order, each checked. Quantities accept the unit forms of
/// [`Quantity::parse`] (`1.5 ether`, `30 gwei`, decimal); a block accepts a
/// decimal number; objects and lists are JSON. An omitted block is
/// `latest`; another omitted required param is an error.
pub fn build_params(
    method: &str,
    pairs: &[(String, String)],
    native: &Denomination,
) -> Result<Value, String> {
    let spec = find(method).ok_or_else(|| {
        format!("method '{method}' is not in the EVM method catalog; give PARAMS_JSON instead")
    })?;
    if let Some((unknown, _)) = pairs
        .iter()
        .find(|(name, _)| !spec.params.iter().any(|p| p.name == name))
    {
        let known: Vec<&str> = spec.params.iter().map(|p| p.name).collect();
        return Err(format!(
            "unknown param '{unknown}' for {method}; it takes {}",
            if known.is_empty() {
                "none".to_owned()
            } else {
                known.join(", ")
            }
        ));
    }
    let mut values = Vec::new();
    for param in spec.params {
        let given = pairs.iter().rev().find(|(name, _)| name == param.name);
        match (given, param) {
            (Some((_, text)), _) => values.push(typed(param, text, native)?),
            (
                None,
                Param {
                    ty: ParamType::Block,
                    required: true,
                    ..
                },
            ) => {
                values.push(json!("latest"));
            }
            (None, Param { required: true, .. }) => {
                return Err(param_error(param.name, "required"));
            }
            (None, _) => break,
        }
    }
    Ok(Value::Array(values))
}

/// The JSON value `text` stands for as `param`, checked.
fn typed(param: &Param, text: &str, native: &Denomination) -> Result<Value, String> {
    let fail = |reason: &str| param_error(param.name, reason);
    let text = text.trim();
    let value = match param.ty {
        ParamType::Quantity => {
            let quantity = Quantity::parse(text, native).map_err(|e| fail(&e.to_string()))?;
            json!(quantity.rpc_hex())
        }
        ParamType::Block if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) => {
            let quantity = Quantity::parse(text, native).map_err(|e| fail(&e.to_string()))?;
            json!(quantity.rpc_hex())
        }
        ParamType::Bool => match text {
            "true" => json!(true),
            "false" => json!(false),
            _ => return Err(fail("expected true or false")),
        },
        ParamType::Filter | ParamType::Transaction | ParamType::Json => {
            serde_json::from_str(text).map_err(|e| fail(&format!("not JSON: {e}")))?
        }
        ParamType::Block if text.starts_with('{') => {
            serde_json::from_str(text).map_err(|e| fail(&format!("not JSON: {e}")))?
        }
        _ => json!(text),
    };
    check_value(param.ty, &value).map_err(|reason| fail(&reason))?;
    Ok(value)
}

/// The native currency of the chain `chain_id` (an environment's pin),
/// with the workspace's `[evm]` chain overrides; ether when the chain is
/// not known.
pub fn native_of(evm: &toml::Table, chain_id: Option<u64>) -> Denomination {
    let registry = EvmSettings::from_table(evm)
        .and_then(|settings| ChainRegistry::with_overrides(&settings))
        .unwrap_or_else(|_| ChainRegistry::built_in());
    match chain_id {
        Some(id) => Denomination::native(&registry.get(id)),
        None => Denomination::default(),
    }
}

/// How a result of `method` is shown: for a native amount, each form of
/// the toggle with the default (`1 ETH`) first; `None` for other results.
pub fn native_forms(method: &str, result: &Value, native: &Denomination) -> Option<Vec<String>> {
    find(method).filter(|m| m.native_result)?;
    let quantity = Quantity::from_rpc(result.as_str()?).ok()?;
    Some(
        forms(true)
            .iter()
            .map(|form: &Form| quantity.show(*form, native))
            .collect(),
    )
}

/// The guard [`install`] registers: a call to a catalog method whose
/// params do not fit is refused before anything is sent.
#[derive(Clone, Copy, Debug, Default)]
pub struct ParamGuard;

impl MethodGuard for ParamGuard {
    fn check(&self, method: &str, params: Option<&Value>) -> Option<String> {
        check_params(method, params).err()
    }
}

/// Register [`ParamGuard`] once per process: every `jsonrpc` request,
/// `rustman rpc call`, the TUI and agent calls check catalog params.
pub fn install() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| register_guard(Arc::new(ParamGuard)));
}

#[cfg(test)]
mod tests {
    use super::*;

    const VITALIK: &str = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045";
    const BAD_CHECKSUM: &str = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96046";

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn typed_balance_query() {
        // Scenario "Typed balance query".
        let params = build_params(
            "eth_getBalance",
            &pairs(&[("address", VITALIK), ("block", "latest")]),
            &Denomination::default(),
        )
        .unwrap();
        let request = json!({"method": "eth_getBalance", "params": params});
        assert_eq!(
            request.to_string(),
            r#"{"method":"eth_getBalance","params":["0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045","latest"]}"#
        );
    }

    #[test]
    fn bad_checksum_rejected() {
        // Scenario "Bad checksum rejected", typed and raw.
        let typed = build_params(
            "eth_getBalance",
            &pairs(&[("address", BAD_CHECKSUM)]),
            &Denomination::default(),
        );
        assert_eq!(
            typed.unwrap_err(),
            "param 'address': invalid EIP-55 checksum"
        );
        let raw = check_params("eth_getBalance", Some(&json!([BAD_CHECKSUM, "latest"])));
        assert_eq!(raw.unwrap_err(), "param 'address': invalid EIP-55 checksum");
        let lower = VITALIK.to_ascii_lowercase();
        assert!(check_params("eth_getBalance", Some(&json!([lower, "latest"]))).is_ok());
        assert!(check_params("eth_getBalance", Some(&json!([VITALIK, "latest"]))).is_ok());
    }

    #[test]
    fn data_tags_quantities_and_objects_are_checked() {
        let cases = [
            (
                "web3_sha3",
                json!(["0x123"]),
                "param 'data': not hex data (0x and an even number of hex digits)",
            ),
            (
                "eth_getBalance",
                json!([VITALIK, "newest"]),
                "param 'block': unknown block tag 'newest' (use latest, earliest, pending, safe, finalized, a 0x number or a block hash)",
            ),
            (
                "eth_getStorageAt",
                json!([VITALIK, "12", "latest"]),
                "param 'position': not a hex quantity (0x…)",
            ),
            (
                "eth_call",
                json!([{"to": BAD_CHECKSUM}]),
                "param 'transaction': to: invalid EIP-55 checksum",
            ),
            (
                "eth_getLogs",
                json!([{"fromBlock": "soon"}]),
                "param 'filter': fromBlock: unknown block tag 'soon' (use latest, earliest, pending, safe, finalized, a 0x number or a block hash)",
            ),
            (
                "eth_getBlockByNumber",
                json!(["latest", "yes"]),
                "param 'full': expected true or false",
            ),
        ];
        for (method, params, message) in cases {
            assert_eq!(
                check_params(method, Some(&params)).unwrap_err(),
                message,
                "{method}"
            );
        }
        let fine = [
            (
                "eth_getBalance",
                json!([VITALIK, {"blockHash": format!("0x{}", "ab".repeat(32))}]),
            ),
            ("eth_getBlockByNumber", json!(["0x10", false])),
            (
                "eth_getLogs",
                json!([{"address": [VITALIK], "fromBlock": "0x1", "toBlock": "latest"}]),
            ),
            (
                "eth_call",
                json!([{"to": VITALIK, "data": "0x70a08231"}, "latest"]),
            ),
            ("eth_getBalance", json!({"address": "anything"})),
            ("not_in_catalog", json!(["whatever"])),
        ];
        for (method, params) in fine {
            assert!(
                check_params(method, Some(&params)).is_ok(),
                "{method} {params}"
            );
        }
    }

    #[test]
    fn params_take_unit_forms_and_name_their_mistakes() {
        let native = Denomination::default();
        let params = build_params(
            "eth_getStorageAt",
            &pairs(&[
                ("address", VITALIK),
                ("position", "10"),
                ("block", "20000000"),
            ]),
            &native,
        )
        .unwrap();
        assert_eq!(params, json!([VITALIK, "0xa", "0x1312d00"]));
        let params = build_params("eth_getBalance", &pairs(&[("address", VITALIK)]), &native);
        assert_eq!(params.unwrap(), json!([VITALIK, "latest"]));
        let call = build_params(
            "eth_call",
            &pairs(&[("transaction", r#"{"to":"0x1"}"#)]),
            &native,
        );
        assert_eq!(
            call.unwrap_err(),
            "param 'transaction': to: not a 20-byte hex address"
        );
        let unknown = build_params("eth_getBalance", &pairs(&[("who", VITALIK)]), &native);
        assert_eq!(
            unknown.unwrap_err(),
            "unknown param 'who' for eth_getBalance; it takes address, block"
        );
        let missing = build_params("eth_getCode", &[], &native);
        assert_eq!(missing.unwrap_err(), "param 'address': required");
        let outside = build_params("foo_bar", &[], &native);
        assert!(
            outside
                .unwrap_err()
                .contains("not in the EVM method catalog")
        );
        assert_eq!(
            find("eth_getBalance").unwrap().signature(),
            "eth_getBalance(address: address, block: block tag)"
        );
        assert_eq!(
            find("eth_call").unwrap().signature(),
            "eth_call(transaction: transaction object, block?: block tag)"
        );
    }

    #[test]
    fn balance_shown_in_ether() {
        // Scenario "Balance shown in ether".
        let forms = native_forms(
            "eth_getBalance",
            &json!("0x0de0b6b3a7640000"),
            &Denomination::default(),
        )
        .unwrap();
        assert_eq!(forms[0], "1 ETH");
        assert!(forms.contains(&"1000000000000000000 wei".to_owned()));
        assert!(forms.contains(&"0x0de0b6b3a7640000".to_owned()));
        assert!(
            native_forms("eth_blockNumber", &json!("0x10"), &Denomination::default()).is_none()
        );
        let polygon = native_of(&toml::Table::new(), Some(137));
        let forms = native_forms("eth_getBalance", &json!("0x0de0b6b3a7640000"), &polygon);
        assert_eq!(forms.unwrap()[0], format!("1 {}", polygon.symbol));
        assert_eq!(
            native_of(&toml::Table::new(), None),
            Denomination::default()
        );
    }

    #[test]
    fn the_catalog_holds_no_send_method_and_its_guard_checks_params() {
        assert!(
            !METHODS
                .iter()
                .any(|m| crate::blocklist::is_send_method(m.name))
        );
        let refusal = ParamGuard.check("eth_getBalance", Some(&json!([BAD_CHECKSUM, "latest"])));
        assert_eq!(
            refusal.as_deref(),
            Some("param 'address': invalid EIP-55 checksum")
        );
        assert!(
            ParamGuard
                .check("eth_getBalance", Some(&json!([VITALIK, "latest"])))
                .is_none()
        );
    }
}
