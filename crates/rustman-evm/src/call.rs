//! `evm-call` execution (evm-toolkit "evm-call request files", "Simulation
//! with state overrides"): a read-only `eth_call` at a block, with state
//! overrides and an optional `eth_estimateGas`, decoded with the ABI.
//!
//! Calls go through [`Rpc`], so over [`crate::JsonrpcTransport`] they pass
//! the JSON-RPC method guards. Nothing here signs or sends a transaction.

use std::collections::BTreeMap;

use alloy_json_abi::{Function, JsonAbi};
use alloy_primitives::{Address, B256, U256, hex};
use rustman_core::model::{EvmCallRequest, Scalar, StateOverride};
use serde_json::{Map, Value, json};

use crate::codec::{NamedValue, Revert, decode_output, decode_revert, encode_call, value_to_json};
use crate::ens::{ENS_NEEDS_MAINNET, Ens, is_ens_name, resolve_args};
use crate::error::EvmError;
use crate::load::BLOCK_TAGS;
use crate::quantity::{Denomination, Quantity, parse_hex};
use crate::rpc::{METHOD_NOT_FOUND, Rpc, RpcFailure};
use crate::tx::{TokenCache, TokenMeta};

/// The error of a node that rejects the `eth_call` override parameter.
pub const OVERRIDES_UNSUPPORTED: &str = "node does not support state overrides";

/// The JSON-RPC code of invalid params.
const INVALID_PARAMS: i64 = -32602;

/// The ERC-20 functions whose single `uint256` output is a token amount.
const TOKEN_AMOUNT_FUNCTIONS: [&str; 3] = [
    "balanceOf(address)",
    "totalSupply()",
    "allowance(address,address)",
];

/// Where a call runs: a block tag, number or hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockSel {
    Tag(String),
    Number(u64),
    Hash(B256),
}

impl Default for BlockSel {
    fn default() -> Self {
        Self::Tag("latest".to_owned())
    }
}

impl BlockSel {
    /// `latest` (and the other tags), a decimal or hex number, or a
    /// 32-byte hash.
    pub fn parse(text: &str) -> Result<Self, EvmError> {
        let text = text.trim();
        let bad = || {
            EvmError::config(format!(
                "block: '{text}' is not a block tag ({}), number or hash",
                BLOCK_TAGS.join(", ")
            ))
        };
        if BLOCK_TAGS.contains(&text) {
            return Ok(Self::Tag(text.to_owned()));
        }
        if let Some(digits) = text.strip_prefix("0x") {
            if digits.len() == 64 {
                return text.parse().map(Self::Hash).map_err(|_| bad());
            }
            return u64::from_str_radix(digits, 16)
                .map(Self::Number)
                .map_err(|_| bad());
        }
        text.parse().map(Self::Number).map_err(|_| bad())
    }

    /// The block parameter of `eth_call`: a tag, a hex number, or an
    /// EIP-1898 `{"blockHash": ...}` object.
    pub fn to_param(&self) -> Value {
        match self {
            Self::Tag(tag) => Value::String(tag.clone()),
            Self::Number(n) => Value::String(format!("{n:#x}")),
            Self::Hash(hash) => json!({"blockHash": hash.to_string()}),
        }
    }
}

/// A call ready to send.
#[derive(Clone, Debug, PartialEq)]
pub struct CallRequest {
    pub to: Address,
    pub from: Option<Address>,
    pub value: Option<U256>,
    pub data: Vec<u8>,
    pub block: BlockSel,
    /// The `eth_call` state override object, when there is one.
    pub overrides: Option<Value>,
}

impl CallRequest {
    /// The transaction object of `eth_call` and `eth_estimateGas`.
    pub fn tx_object(&self) -> Value {
        let mut tx = Map::new();
        if let Some(from) = self.from {
            tx.insert("from".into(), Value::String(from.to_checksum(None)));
        }
        tx.insert("to".into(), Value::String(self.to.to_checksum(None)));
        if let Some(value) = self.value {
            tx.insert("value".into(), Value::String(format!("{value:#x}")));
        }
        tx.insert(
            "data".into(),
            Value::String(hex::encode_prefixed(&self.data)),
        );
        Value::Object(tx)
    }

    /// The `eth_call` params: the transaction, the block and, when set,
    /// the overrides as the third parameter.
    pub fn call_params(&self) -> Value {
        let mut params = vec![self.tx_object(), self.block.to_param()];
        params.extend(self.overrides.clone());
        Value::Array(params)
    }
}

/// The values of an `evm-call` file once its variables are resolved.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CallInputs {
    pub to: String,
    pub from: Option<String>,
    pub value: Option<String>,
    pub args: Vec<Value>,
    pub block: Option<String>,
    pub state_overrides: Option<BTreeMap<String, StateOverride>>,
}

impl CallInputs {
    /// The fields of `request` as written.
    pub fn from_request(request: &EvmCallRequest) -> Self {
        Self {
            to: request
                .to
                .as_ref()
                .map(|t| t.get().clone())
                .unwrap_or_default(),
            from: request.from.clone(),
            value: request.value.as_ref().map(|v| v.as_str().to_owned()),
            args: request.args.clone().unwrap_or_default(),
            block: request.block.as_ref().map(|b| b.as_str().to_owned()),
            state_overrides: request.state_overrides.clone(),
        }
    }
}

/// Resolve ENS names, encode the arguments and parse the block, value and
/// overrides of `inputs` for `function`.
pub async fn prepare(
    inputs: &CallInputs,
    function: &Function,
    native: &Denomination,
    ens: Option<&Ens<'_>>,
) -> Result<CallRequest, EvmError> {
    let to = address(&inputs.to, "to", ens).await?;
    let from = match &inputs.from {
        Some(from) => Some(address(from, "from", ens).await?),
        None => None,
    };
    let value = inputs
        .value
        .as_deref()
        .map(|v| quantity(v, "value", native))
        .transpose()?;
    let args = resolve_args(ens, &function.inputs, &inputs.args).await?;
    let block = inputs
        .block
        .as_deref()
        .map(BlockSel::parse)
        .transpose()?
        .unwrap_or_default();
    let overrides = inputs
        .state_overrides
        .as_ref()
        .map(|o| overrides_json(o, native))
        .transpose()?;
    Ok(CallRequest {
        to,
        from,
        value,
        data: encode_call(function, &args)?,
        block,
        overrides,
    })
}

async fn address(text: &str, field: &str, ens: Option<&Ens<'_>>) -> Result<Address, EvmError> {
    if is_ens_name(text) {
        let ens = ens.ok_or_else(|| EvmError::config(ENS_NEEDS_MAINNET))?;
        return ens.resolve(text).await;
    }
    parse_address(text).map_err(|reason| EvmError::config(format!("{field}: {reason}")))
}

/// A hex address; mixed case must be a valid EIP-55 checksum.
pub fn parse_address(text: &str) -> Result<Address, String> {
    let text = text.trim();
    let digits = text.strip_prefix("0x").unwrap_or(text);
    let mixed = digits.chars().any(|c| c.is_ascii_uppercase())
        && digits.chars().any(|c| c.is_ascii_lowercase());
    if mixed {
        return Address::parse_checksummed(text, None)
            .map_err(|_| "invalid EIP-55 checksum".to_owned());
    }
    text.parse()
        .map_err(|_| format!("'{text}' is not a 20-byte hex address"))
}

fn quantity(text: &str, field: &str, native: &Denomination) -> Result<U256, EvmError> {
    Quantity::parse(text, native)
        .map(|q| q.value)
        .map_err(|e| EvmError::config(format!("{field}: {e}")))
}

/// The `eth_call` override object of `state_overrides`: quantities as
/// hex, storage as 32-byte `stateDiff` words.
pub fn overrides_json(
    overrides: &BTreeMap<String, StateOverride>,
    native: &Denomination,
) -> Result<Value, EvmError> {
    let mut all = Map::new();
    for (address, state) in overrides {
        let key = parse_address(address)
            .map_err(|reason| EvmError::config(format!("state_overrides: {reason}")))?;
        all.insert(key.to_checksum(None), one_override(address, state, native)?);
    }
    Ok(Value::Object(all))
}

fn one_override(
    address: &str,
    state: &StateOverride,
    native: &Denomination,
) -> Result<Value, EvmError> {
    let mut out = Map::new();
    let scalars = [("balance", &state.balance), ("nonce", &state.nonce)];
    for (name, scalar) in scalars {
        if let Some(text) = scalar.as_ref().map(Scalar::as_str) {
            let field = format!("state_overrides.{address}.{name}");
            let value = quantity(text, &field, native)?;
            out.insert(name.into(), Value::String(format!("{value:#x}")));
        }
    }
    if let Some(code) = &state.code {
        out.insert("code".into(), Value::String(code.clone()));
    }
    if let Some(storage) = &state.storage {
        let mut diff = Map::new();
        for (slot, value) in storage {
            diff.insert(word(slot, address)?, Value::String(word(value, address)?));
        }
        out.insert("stateDiff".into(), Value::Object(diff));
    }
    Ok(Value::Object(out))
}

fn word(text: &str, address: &str) -> Result<String, EvmError> {
    let value = parse_hex(text)
        .map_err(|e| EvmError::config(format!("state_overrides.{address}.storage: {e}")))?;
    Ok(B256::from(value).to_string())
}

/// How a call ended.
#[derive(Clone, Debug, PartialEq)]
pub enum CallOutcome {
    /// The call returned: its data and the decoded outputs (or why they
    /// did not decode).
    Returned {
        data: Vec<u8>,
        values: Result<Vec<NamedValue>, String>,
    },
    Reverted(Revert),
}

/// What a call returned, decoded.
#[derive(Clone, Debug, PartialEq)]
pub struct CallReport {
    pub function: Function,
    pub outcome: CallOutcome,
    /// `--estimate`: the gas estimate, or why there is none.
    pub gas_estimate: Option<Result<U256, String>>,
    /// The token's metadata when the output is an ERC-20 amount.
    pub token: Option<TokenMeta>,
}

/// Run `request` as `eth_call` (and `eth_estimateGas` when `estimate`),
/// decoding the output with `function` and a revert with `abi`.
pub async fn run_call(
    rpc: &dyn Rpc,
    request: &CallRequest,
    function: &Function,
    abi: Option<&JsonAbi>,
    estimate: bool,
) -> Result<CallReport, EvmError> {
    let outcome = match rpc.call("eth_call", request.call_params()).await {
        Ok(value) => {
            let data = crate::codec::parse_bytes(value.as_str().unwrap_or("0x"))?;
            let values = decode_output(function, &data).map_err(|e| e.to_string());
            CallOutcome::Returned { data, values }
        }
        Err(failure) => CallOutcome::Reverted(revert_or_error(failure, request, abi)?),
    };
    let gas_estimate = match estimate {
        true => Some(estimate_gas(rpc, request, abi).await),
        false => None,
    };
    Ok(CallReport {
        function: function.clone(),
        outcome,
        gas_estimate,
        token: None,
    })
}

/// The revert a failed `eth_call` carries, or the error to report.
fn revert_or_error(
    failure: RpcFailure,
    request: &CallRequest,
    abi: Option<&JsonAbi>,
) -> Result<Revert, EvmError> {
    let RpcFailure::Error {
        code,
        message,
        data,
    } = &failure
    else {
        return Err(failure.into_error("eth_call"));
    };
    if let Some(revert) = revert_data(*code, message, data.as_ref()) {
        return Ok(decode_revert(abi, &revert));
    }
    if request.overrides.is_some() && rejects_overrides(*code, message) {
        return Err(EvmError::Rpc(OVERRIDES_UNSUPPORTED.to_owned()));
    }
    Err(failure.into_error("eth_call"))
}

/// The revert data of a node error: `error.data` as hex (or nested under
/// `data.data`), or empty data for a revert without any.
pub fn revert_data(code: i64, message: &str, data: Option<&Value>) -> Option<Vec<u8>> {
    let hex = data.and_then(|d| d.as_str().or_else(|| d.get("data").and_then(Value::as_str)));
    if let Some(bytes) = hex.and_then(|h| crate::codec::parse_bytes(h).ok()) {
        return Some(bytes);
    }
    let reverted = code == 3 || message.to_ascii_lowercase().contains("revert");
    reverted.then(Vec::new)
}

fn rejects_overrides(code: i64, message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    code == INVALID_PARAMS
        || code == METHOD_NOT_FOUND
        || ["override", "too many", "argument", "params"]
            .iter()
            .any(|m| message.contains(m))
}

async fn estimate_gas(
    rpc: &dyn Rpc,
    request: &CallRequest,
    abi: Option<&JsonAbi>,
) -> Result<U256, String> {
    let mut params = vec![request.tx_object(), request.block.to_param()];
    params.extend(request.overrides.clone());
    match rpc.call("eth_estimateGas", Value::Array(params)).await {
        Ok(value) => parse_hex(value.as_str().unwrap_or_default()).map_err(|e| e.to_string()),
        Err(RpcFailure::Error {
            code,
            message,
            data,
        }) => Err(match revert_data(code, &message, data.as_ref()) {
            Some(revert) => decode_revert(abi, &revert).to_string(),
            None => format!("eth_estimateGas: rpc error {code}: {message}"),
        }),
        Err(other) => Err(other.into_error("eth_estimateGas").to_string()),
    }
}

impl CallReport {
    /// Ask the token's `decimals()` and `symbol()` when the output is an
    /// ERC-20 amount, so it is shown in token units.
    pub async fn with_token(
        mut self,
        rpc: &dyn Rpc,
        chain_id: u64,
        token: Address,
        tokens: &TokenCache,
    ) -> Self {
        let signature = self.function.signature();
        if TOKEN_AMOUNT_FUNCTIONS.contains(&signature.as_str()) && self.returned_values().is_some()
        {
            self.token = Some(tokens.get(rpc, chain_id, token).await);
        }
        self
    }

    /// The decoded outputs of a call that returned.
    pub fn returned_values(&self) -> Option<&[NamedValue]> {
        match &self.outcome {
            CallOutcome::Returned { values: Ok(v), .. } => Some(v),
            _ => None,
        }
    }

    pub fn revert(&self) -> Option<&Revert> {
        match &self.outcome {
            CallOutcome::Reverted(revert) => Some(revert),
            CallOutcome::Returned { .. } => None,
        }
    }

    /// The result as text: one `name = value` line per output (token
    /// amounts in token units), or the revert, then the gas estimate.
    pub fn render(&self) -> String {
        let mut lines = match &self.outcome {
            CallOutcome::Reverted(revert) => vec![revert.to_string()],
            CallOutcome::Returned { values: Ok(v), .. } => self.output_lines(v),
            CallOutcome::Returned {
                data,
                values: Err(reason),
            } => vec![format!(
                "{reason}; raw output {}",
                hex::encode_prefixed(data)
            )],
        };
        match &self.gas_estimate {
            Some(Ok(gas)) => lines.push(format!("gas estimate: {gas}")),
            Some(Err(reason)) => lines.push(format!("gas estimate unavailable: {reason}")),
            None => {}
        }
        lines.join("\n")
    }

    fn output_lines(&self, values: &[NamedValue]) -> Vec<String> {
        if values.is_empty() {
            return vec!["(no outputs)".to_owned()];
        }
        values
            .iter()
            .enumerate()
            .map(|(i, v)| format!("{} = {}", output_key(v, i), self.shown(v)))
            .collect()
    }

    fn shown(&self, value: &NamedValue) -> String {
        match (&self.token, &value.value) {
            (Some(meta), alloy_dyn_abi::DynSolValue::Uint(amount, _))
                if meta.decimals.is_some() =>
            {
                format!("{} ({amount})", meta.format(*amount))
            }
            _ => crate::codec::format_value(&value.value),
        }
    }

    /// The result as JSON: outputs by name (or index) with full-precision
    /// values, the revert, and the gas estimate.
    pub fn to_json(&self) -> Value {
        let outputs = self.returned_values().map(|values| {
            values
                .iter()
                .enumerate()
                .map(|(i, v)| (output_key(v, i), value_to_json(&v.value)))
                .collect::<Map<_, _>>()
        });
        let reverted = self
            .revert()
            .map(|r| json!({"message": r.to_string(), "name": r.name()}));
        json!({
            "function": self.function.signature(),
            "outputs": outputs,
            "reverted": reverted,
            "gas_estimate": self.gas_estimate.as_ref().map(|g| match g {
                Ok(gas) => json!(gas.to_string()),
                Err(reason) => json!({"error": reason}),
            }),
            "token": self.token.as_ref().map(|t| json!({"decimals": t.decimals, "symbol": t.symbol})),
        })
    }
}

/// An output's name, or its index when unnamed.
pub fn output_key(value: &NamedValue, index: usize) -> String {
    if value.name.is_empty() {
        index.to_string()
    } else {
        value.name.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_selectors_become_call_params() {
        assert_eq!(
            BlockSel::parse("latest").unwrap().to_param(),
            json!("latest")
        );
        assert_eq!(
            BlockSel::parse("19000000").unwrap().to_param(),
            json!("0x121eac0")
        );
        assert_eq!(BlockSel::parse("0x10").unwrap(), BlockSel::Number(16));
        let hash = format!("0x{}", "ab".repeat(32));
        assert_eq!(
            BlockSel::parse(&hash).unwrap().to_param(),
            json!({"blockHash": hash})
        );
        assert!(BlockSel::parse("newest").is_err());
    }

    #[test]
    fn overrides_become_the_third_param() {
        let mut storage = BTreeMap::new();
        storage.insert("0x0".to_owned(), "0x1".to_owned());
        let state = StateOverride {
            balance: Some(Scalar("100 ether".into())),
            nonce: Some(Scalar("7".into())),
            code: Some("0x6000".into()),
            storage: Some(storage),
        };
        let mut overrides = BTreeMap::new();
        overrides.insert(
            "0x00000000000000000000000000000000000000ab".to_owned(),
            state,
        );
        let json = overrides_json(&overrides, &Denomination::default()).unwrap();
        let zero = format!("0x{}", "0".repeat(64));
        let one = format!("0x{}1", "0".repeat(63));
        assert_eq!(
            json,
            json!({"0x00000000000000000000000000000000000000AB": {
                "balance": "0x56bc75e2d63100000",
                "nonce": "0x7",
                "code": "0x6000",
                "stateDiff": {zero: one},
            }})
        );
    }

    #[test]
    fn revert_data_is_found_in_node_errors() {
        assert_eq!(
            revert_data(3, "execution reverted", Some(&json!("0x01"))),
            Some(vec![1])
        );
        assert_eq!(
            revert_data(-32000, "x", Some(&json!({"data": "0x02"}))),
            Some(vec![2])
        );
        assert_eq!(
            revert_data(-32000, "execution reverted", None),
            Some(vec![])
        );
        assert_eq!(revert_data(-32000, "header not found", None), None);
    }

    #[test]
    fn addresses_check_their_checksum() {
        assert!(parse_address("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045").is_ok());
        assert_eq!(
            parse_address("0xd8DA6BF26964aF9D7eEd9e03E53415D37aA96045").unwrap_err(),
            "invalid EIP-55 checksum"
        );
        assert!(parse_address("0xd8da6bf26964af9d7eed9e03e53415d37aa96045").is_ok());
        assert!(parse_address("0x12").is_err());
    }
}
