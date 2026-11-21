//! Transactions, receipts and their logs, decoded (evm-toolkit
//! "Transaction, receipt and log decoding", "Built-in token standards").
//!
//! Input data and logs decode with the ABI known for their address, else
//! with the built-in token standards. ERC-20 amounts are shown in token
//! units with the token's `decimals()` and `symbol()`, asked once per token
//! per session ([`TokenCache`]).

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex, PoisonError};

use alloy_dyn_abi::{DynSolType, DynSolValue};
use alloy_json_abi::JsonAbi;
use alloy_primitives::{Address, B256, U256, hex};
use serde_json::{Value, json};

use crate::abi::{BUILT_IN, built_in};
use crate::chain::ChainInfo;
use crate::codec::{
    DecodedCall, DecodedLog, NamedValue, decode_input, decode_log, format_value, parse_bytes,
    value_to_json,
};
use crate::error::EvmError;
use crate::quantity::{Denomination, Form, GWEI_DECIMALS, Quantity, format_fixed, parse_hex};
use crate::rpc::{Rpc, call};

/// The message of an unknown or dropped transaction.
pub const TX_NOT_FOUND: &str = "transaction not found";

/// The warning when a token's `decimals()` fails.
pub const NO_DECIMALS: &str = "decimals() unavailable; showing raw amount";

/// `decimals()` and `symbol()` selectors.
const DECIMALS: &str = "0x313ce567";
const SYMBOL: &str = "0x95d89b41";

/// What a token says about itself.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TokenMeta {
    /// `None` when `decimals()` failed.
    pub decimals: Option<u8>,
    pub symbol: Option<String>,
}

impl TokenMeta {
    /// `amount` in token units (`250.00 USDC`), or raw when the decimals
    /// are unknown.
    pub fn format(&self, amount: U256) -> String {
        let number = match self.decimals {
            Some(decimals) => format_fixed(amount, decimals, 2),
            None => amount.to_string(),
        };
        match (&self.symbol, self.decimals) {
            (Some(symbol), Some(_)) => format!("{number} {symbol}"),
            _ => number,
        }
    }
}

/// Token metadata per `(chain, token)` for the session.
#[derive(Debug, Default)]
pub struct TokenCache {
    known: Mutex<HashMap<(u64, Address), TokenMeta>>,
}

impl TokenCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// The metadata of `token`, asked of the node the first time only.
    pub async fn get(&self, rpc: &dyn Rpc, chain_id: u64, token: Address) -> TokenMeta {
        if let Some(meta) = self.lock().get(&(chain_id, token)) {
            return meta.clone();
        }
        let decimals = read(rpc, token, DECIMALS)
            .await
            .and_then(|data| decode_uint(&data))
            .and_then(|d| u8::try_from(d).ok());
        let symbol = read(rpc, token, SYMBOL)
            .await
            .and_then(|data| decode_symbol(&data));
        let meta = TokenMeta { decimals, symbol };
        self.lock().insert((chain_id, token), meta.clone());
        meta
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(u64, Address), TokenMeta>> {
        self.known.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

async fn read(rpc: &dyn Rpc, token: Address, selector: &str) -> Option<Vec<u8>> {
    let params = json!([{"to": token.to_checksum(None), "data": selector}, "latest"]);
    let value = rpc.call("eth_call", params).await.ok()?;
    let data = parse_bytes(value.as_str()?).ok()?;
    (!data.is_empty()).then_some(data)
}

fn decode_uint(data: &[u8]) -> Option<U256> {
    match DynSolType::Uint(256).abi_decode(data).ok()? {
        DynSolValue::Uint(value, _) => Some(value),
        _ => None,
    }
}

/// A `string` symbol, or the `bytes32` form older tokens return.
fn decode_symbol(data: &[u8]) -> Option<String> {
    if let Ok(DynSolValue::String(symbol)) = DynSolType::String.abi_decode(data)
        && !symbol.is_empty()
    {
        return Some(symbol);
    }
    let word = data.get(..32)?;
    let text: Vec<u8> = word.iter().copied().take_while(|b| *b != 0).collect();
    String::from_utf8(text).ok().filter(|s| !s.is_empty())
}

/// The ABIs known per address.
pub type AbiMap = HashMap<Address, Arc<JsonAbi>>;

/// What decoding needs: the node, its chain, ABIs known per address and
/// the session's token cache.
pub struct DecodeContext<'a> {
    pub rpc: &'a dyn Rpc,
    pub chain: ChainInfo,
    pub abis: &'a HashMap<Address, Arc<JsonAbi>>,
    pub tokens: &'a TokenCache,
}

impl DecodeContext<'_> {
    /// The ABIs to try for `address`: its own, then the built-ins.
    fn abis_for(&self, address: Option<Address>) -> Vec<Arc<JsonAbi>> {
        let own = address.and_then(|a| self.abis.get(&a).cloned());
        own.into_iter()
            .chain(BUILT_IN.iter().filter_map(|name| built_in(name)))
            .collect()
    }
}

/// One log of a receipt.
#[derive(Clone, Debug, PartialEq)]
pub struct LogView {
    pub address: Address,
    pub index: Option<u64>,
    pub topics: Vec<B256>,
    pub data: Vec<u8>,
    /// `Transfer(from=…, to=…, value=250.00 USDC)`, when an ABI decodes it.
    pub decoded: Option<String>,
    pub fields: Option<Value>,
    /// A log a reorg removed.
    pub removed: bool,
}

/// A transaction with its receipt, decoded.
#[derive(Clone, Debug, PartialEq)]
pub struct TxReport {
    pub hash: String,
    pub chain: ChainInfo,
    pub from: Address,
    pub to: Option<Address>,
    pub value: U256,
    pub nonce: u64,
    pub block: Option<u64>,
    /// `None` while pending.
    pub status: Option<bool>,
    pub gas_used: Option<U256>,
    pub effective_gas_price: Option<U256>,
    pub input: Vec<u8>,
    pub decoded_input: Option<DecodedCall>,
    pub logs: Vec<LogView>,
    pub warnings: Vec<String>,
}

fn field<'a>(object: &'a Value, name: &str) -> Result<&'a str, EvmError> {
    object[name]
        .as_str()
        .ok_or_else(|| EvmError::Rpc(format!("missing or invalid '{name}' in node answer")))
}

fn address_of(object: &Value, name: &str) -> Result<Address, EvmError> {
    field(object, name)?
        .parse()
        .map_err(|_| EvmError::Rpc(format!("invalid address in '{name}'")))
}

fn quantity_of(object: &Value, name: &str) -> Result<U256, EvmError> {
    parse_hex(field(object, name)?).map_err(|e| EvmError::Rpc(e.to_string()))
}

fn optional_u64(object: &Value, name: &str) -> Option<u64> {
    let value = parse_hex(object[name].as_str()?).ok()?;
    u64::try_from(value).ok()
}

/// Fetch the transaction `hash` and its receipt, and decode them.
pub async fn fetch_tx(ctx: &DecodeContext<'_>, hash: &str) -> Result<TxReport, EvmError> {
    let tx = call(ctx.rpc, "eth_getTransactionByHash", json!([hash])).await?;
    if tx.is_null() {
        return Err(EvmError::NotFound(TX_NOT_FOUND.to_owned()));
    }
    let receipt = call(ctx.rpc, "eth_getTransactionReceipt", json!([hash])).await?;
    let to = match tx["to"].as_str() {
        Some(_) => Some(address_of(&tx, "to")?),
        None => None,
    };
    let input = parse_bytes(field(&tx, "input")?)?;
    let mut report = TxReport {
        hash: hash.to_owned(),
        chain: ctx.chain.clone(),
        from: address_of(&tx, "from")?,
        to,
        value: quantity_of(&tx, "value")?,
        nonce: optional_u64(&tx, "nonce").unwrap_or_default(),
        block: optional_u64(&tx, "blockNumber"),
        status: receipt["status"].as_str().map(|s| s == "0x1"),
        gas_used: quantity_of(&receipt, "gasUsed").ok(),
        effective_gas_price: quantity_of(&receipt, "effectiveGasPrice").ok(),
        decoded_input: ctx
            .abis_for(to)
            .iter()
            .find_map(|abi| decode_input(abi, &input)),
        input,
        logs: Vec::new(),
        warnings: Vec::new(),
    };
    for log in receipt["logs"].as_array().into_iter().flatten() {
        let view = log_view(ctx, log, &mut report.warnings).await?;
        report.logs.push(view);
    }
    Ok(report)
}

/// One receipt or streamed log, decoded when an ABI fits. Warnings (such
/// as a token without `decimals()`) are added to `warnings` once each.
pub async fn log_view(
    ctx: &DecodeContext<'_>,
    log: &Value,
    warnings: &mut Vec<String>,
) -> Result<LogView, EvmError> {
    let address = address_of(log, "address")?;
    let topics = log["topics"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| t.as_str().and_then(|s| s.parse::<B256>().ok()))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| EvmError::Rpc("invalid log topics".to_owned()))?;
    let data = parse_bytes(field(log, "data")?)?;
    let decoded = ctx
        .abis_for(Some(address))
        .iter()
        .find_map(|abi| decode_log(abi, &topics, &data));
    let (shown, fields) = match &decoded {
        Some(decoded) => {
            let (shown, fields) = render_log(ctx, address, decoded, warnings).await;
            (Some(shown), Some(fields))
        }
        None => (None, None),
    };
    Ok(LogView {
        address,
        index: optional_u64(log, "logIndex"),
        topics,
        data,
        decoded: shown,
        fields,
        removed: log["removed"].as_bool().unwrap_or(false),
    })
}

/// The field holding an ERC-20 amount: `Transfer` and `Approval` with two
/// indexed addresses and a `uint256` (ERC-721 indexes its third field).
fn erc20_amount(decoded: &DecodedLog) -> Option<usize> {
    let event = &decoded.event;
    let signature = event.signature();
    let indexed = event.inputs.iter().filter(|p| p.indexed).count();
    let erc20 = matches!(
        signature.as_str(),
        "Transfer(address,address,uint256)" | "Approval(address,address,uint256)"
    );
    (erc20 && indexed == 2).then_some(2)
}

async fn render_log(
    ctx: &DecodeContext<'_>,
    address: Address,
    decoded: &DecodedLog,
    warnings: &mut Vec<String>,
) -> (String, Value) {
    let amount = match erc20_amount(decoded) {
        Some(index) => {
            let meta = ctx.tokens.get(ctx.rpc, ctx.chain.id, address).await;
            if meta.decimals.is_none() && !warnings.iter().any(|w| w == NO_DECIMALS) {
                warnings.push(NO_DECIMALS.to_owned());
            }
            Some((index, meta))
        }
        None => None,
    };
    let mut parts = Vec::new();
    let mut fields = serde_json::Map::new();
    for (i, field) in decoded.fields.iter().enumerate() {
        let shown = match (&amount, &field.value) {
            (Some((index, meta)), DynSolValue::Uint(value, _)) if *index == i => {
                meta.format(*value)
            }
            _ => format_value(&field.value),
        };
        parts.push(labelled(field, &shown));
        fields.insert(field_key(field, i), value_to_json(&field.value));
    }
    let shown = format!("{}({})", decoded.event.name, parts.join(", "));
    (shown, Value::Object(fields))
}

fn labelled(field: &NamedValue, shown: &str) -> String {
    if field.name.is_empty() {
        shown.to_owned()
    } else {
        format!("{}={shown}", field.name)
    }
}

fn field_key(field: &NamedValue, index: usize) -> String {
    if field.name.is_empty() {
        index.to_string()
    } else {
        field.name.clone()
    }
}

impl TxReport {
    /// The fee paid: gas used times the effective gas price.
    pub fn fee(&self) -> Option<U256> {
        self.gas_used?.checked_mul(self.effective_gas_price?)
    }

    fn status_text(&self) -> &'static str {
        match self.status {
            Some(true) => "success",
            Some(false) => "reverted",
            None => "pending",
        }
    }

    /// The report as text, one field per line, then the logs.
    pub fn render(&self) -> String {
        let native = Denomination::native(&self.chain);
        let eth = |v: U256| Quantity::from(v).show(Form::Native, &native);
        let gwei = |v: U256| format!("{} gwei", format_fixed(v, GWEI_DECIMALS, 0));
        let none = || "-".to_owned();
        let mut out = String::new();
        let mut line = |name: &str, value: String| {
            let _ = writeln!(out, "{name:<10} {value}");
        };
        line("tx", self.hash.clone());
        line("chain", self.chain.label());
        line("status", self.status_text().to_owned());
        line("block", self.block.map_or_else(none, |b| b.to_string()));
        line("from", self.from.to_checksum(None));
        line("to", self.to.map_or_else(none, |a| a.to_checksum(None)));
        line("value", eth(self.value));
        line("nonce", self.nonce.to_string());
        line(
            "gas used",
            self.gas_used.map_or_else(none, |g| g.to_string()),
        );
        line(
            "gas price",
            self.effective_gas_price.map_or_else(none, gwei),
        );
        line("fee", self.fee().map_or_else(none, eth));
        line("input", self.input_text());
        if !self.logs.is_empty() {
            let _ = writeln!(out, "logs");
        }
        for log in &self.logs {
            let _ = writeln!(out, "  {}", log_line(log));
        }
        for warning in &self.warnings {
            let _ = writeln!(out, "warning: {warning}");
        }
        out
    }

    fn input_text(&self) -> String {
        match &self.decoded_input {
            Some(call) => call.to_string(),
            None if self.input.is_empty() => "-".to_owned(),
            None => hex::encode_prefixed(&self.input),
        }
    }

    /// The report for `--json`: quantities as decimal strings.
    pub fn to_json(&self) -> Value {
        json!({
            "hash": self.hash,
            "chain_id": self.chain.id,
            "status": self.status_text(),
            "block": self.block,
            "from": self.from.to_checksum(None),
            "to": self.to.map(|a| a.to_checksum(None)),
            "value": self.value.to_string(),
            "nonce": self.nonce,
            "gas_used": self.gas_used.map(|g| g.to_string()),
            "effective_gas_price": self.effective_gas_price.map(|g| g.to_string()),
            "fee": self.fee().map(|f| f.to_string()),
            "input": hex::encode_prefixed(&self.input),
            "decoded_input": self.decoded_input.as_ref().map(|c| json!({
                "function": c.function.signature(),
                "args": c.args.iter().enumerate().map(|(i, a)| (field_key(a, i), value_to_json(&a.value))).collect::<serde_json::Map<_, _>>(),
            })),
            "logs": self.logs.iter().map(LogView::to_json).collect::<Vec<_>>(),
            "warnings": self.warnings,
        })
    }
}

/// `[912] 0xA0b8… Transfer(…)`, or the topics and data as hex.
pub fn log_line(log: &LogView) -> String {
    let index = log.index.map(|i| format!("[{i}] ")).unwrap_or_default();
    let reorg = if log.removed { "REORGED " } else { "" };
    let body = match &log.decoded {
        Some(decoded) => decoded.clone(),
        None => {
            let topics: Vec<String> = log.topics.iter().map(ToString::to_string).collect();
            format!(
                "topics=[{}] data={}",
                topics.join(", "),
                hex::encode_prefixed(&log.data)
            )
        }
    };
    format!("{reorg}{index}{} {body}", log.address.to_checksum(None))
}

impl LogView {
    pub fn to_json(&self) -> Value {
        json!({
            "address": self.address.to_checksum(None),
            "index": self.index,
            "topics": self.topics.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "data": hex::encode_prefixed(&self.data),
            "decoded": self.decoded,
            "fields": self.fields,
            "removed": self.removed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_amounts_and_symbols() {
        let usdc = TokenMeta {
            decimals: Some(6),
            symbol: Some("USDC".into()),
        };
        assert_eq!(usdc.format(U256::from(250_000_000u64)), "250.00 USDC");
        let raw = TokenMeta {
            decimals: None,
            symbol: Some("ODD".into()),
        };
        assert_eq!(raw.format(U256::from(250_000_000u64)), "250000000");
    }

    #[test]
    fn symbols_decode_as_string_or_bytes32() {
        let string = DynSolValue::String("USDC".into()).abi_encode_params();
        assert_eq!(decode_symbol(&string).as_deref(), Some("USDC"));
        let mut word = [0u8; 32];
        word[..3].copy_from_slice(b"MKR");
        assert_eq!(decode_symbol(&word).as_deref(), Some("MKR"));
        assert_eq!(decode_symbol(&[0u8; 32]), None);
    }
}
