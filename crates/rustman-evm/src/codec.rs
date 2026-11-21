//! Encoding and decoding with a runtime ABI (evm-toolkit "Revert and
//! custom error decoding", "Transaction, receipt and log decoding"): call
//! data from JSON arguments, outputs, input data, logs and revert data.

use std::fmt;

use alloy_dyn_abi::{
    DynSolType, DynSolValue, ErrorExt, EventExt, FunctionExt, JsonAbiExt, Specifier,
};
use alloy_json_abi::{Error as AbiError, Event, Function, JsonAbi, Param};
use alloy_primitives::{Address, B256, U256, hex};
use serde_json::Value;

use crate::error::EvmError;
use crate::quantity::{Denomination, Quantity};

/// The selector of `Error(string)`.
pub const ERROR_SELECTOR: [u8; 4] = [0x08, 0xc3, 0x79, 0xa0];

/// The selector of `Panic(uint256)`.
pub const PANIC_SELECTOR: [u8; 4] = [0x4e, 0x48, 0x7b, 0x71];

/// A decoded value with its parameter name and type.
#[derive(Clone, Debug, PartialEq)]
pub struct NamedValue {
    /// Empty for unnamed parameters.
    pub name: String,
    pub ty: String,
    pub value: DynSolValue,
}

impl fmt::Display for NamedValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.name.is_empty() {
            f.write_str(&format_value(&self.value))
        } else {
            write!(f, "{}={}", self.name, format_value(&self.value))
        }
    }
}

fn named(params: &[Param], values: Vec<DynSolValue>) -> Vec<NamedValue> {
    params
        .iter()
        .zip(values)
        .map(|(param, value)| NamedValue {
            name: param.name.clone(),
            ty: param.selector_type().into_owned(),
            value,
        })
        .collect()
}

/// `name(a=1, b=2)`.
pub fn signature_with_values(name: &str, values: &[NamedValue]) -> String {
    let shown: Vec<String> = values.iter().map(ToString::to_string).collect();
    format!("{name}({})", shown.join(", "))
}

/// A value as shown to users: checksummed addresses, decimal integers,
/// `0x` hex bytes, quoted strings, `[..]` arrays and `(..)` tuples.
pub fn format_value(value: &DynSolValue) -> String {
    match value {
        DynSolValue::Address(a) => a.to_checksum(None),
        DynSolValue::Bool(b) => b.to_string(),
        DynSolValue::Int(i, _) => i.to_string(),
        DynSolValue::Uint(u, _) => u.to_string(),
        DynSolValue::FixedBytes(word, size) => hex::encode_prefixed(&word[..*size]),
        DynSolValue::Bytes(bytes) => hex::encode_prefixed(bytes),
        DynSolValue::Function(f) => f.to_string(),
        DynSolValue::String(s) => Value::String(s.clone()).to_string(),
        DynSolValue::Array(items) | DynSolValue::FixedArray(items) => {
            format!("[{}]", joined(items))
        }
        DynSolValue::Tuple(items) => format!("({})", joined(items)),
        #[allow(unreachable_patterns)]
        other => format!("{other:?}"),
    }
}

fn joined(items: &[DynSolValue]) -> String {
    items
        .iter()
        .map(format_value)
        .collect::<Vec<_>>()
        .join(", ")
}

/// A value as JSON: quantities as decimal strings and addresses
/// checksummed, so captures and `--json` output keep full precision.
pub fn value_to_json(value: &DynSolValue) -> Value {
    match value {
        DynSolValue::Bool(b) => Value::Bool(*b),
        DynSolValue::String(s) => Value::String(s.clone()),
        DynSolValue::Array(items) | DynSolValue::FixedArray(items) | DynSolValue::Tuple(items) => {
            Value::Array(items.iter().map(value_to_json).collect())
        }
        other => Value::String(format_value(other)),
    }
}

/// Encode a call of `function` with JSON `args` (strings, numbers,
/// booleans and nested lists). Integers accept unit forms such as
/// `1.5 ether`; mixed-case addresses must have a valid EIP-55 checksum.
pub fn encode_call(function: &Function, args: &[Value]) -> Result<Vec<u8>, EvmError> {
    if args.len() != function.inputs.len() {
        return Err(EvmError::config(format!(
            "function '{}' takes {} argument(s), got {}",
            function.signature(),
            function.inputs.len(),
            args.len()
        )));
    }
    let values = function
        .inputs
        .iter()
        .zip(args)
        .map(|(param, arg)| coerce_arg(param, arg))
        .collect::<Result<Vec<_>, _>>()?;
    function
        .abi_encode_input(&values)
        .map_err(|e| EvmError::config(format!("{}: {e}", function.signature())))
}

fn coerce_arg(param: &Param, arg: &Value) -> Result<DynSolValue, EvmError> {
    let label = if param.name.is_empty() {
        format!("argument ({})", param.ty)
    } else {
        format!("argument '{}' ({})", param.name, param.ty)
    };
    let fail = |reason: String| EvmError::config(format!("{label}: {reason}"));
    let ty: DynSolType = param.resolve().map_err(|e| fail(e.to_string()))?;
    if let (DynSolType::String, Value::String(s)) = (&ty, arg) {
        return Ok(DynSolValue::String(s.clone()));
    }
    let text = arg_text(&ty, arg).map_err(fail)?;
    if ty == DynSolType::Address {
        check_checksum(&text).map_err(fail)?;
    }
    ty.coerce_str(&text).map_err(|e| fail(e.to_string()))
}

/// The text `coerce_str` reads for `arg` as `ty`.
fn arg_text(ty: &DynSolType, arg: &Value) -> Result<String, String> {
    match (ty, arg) {
        (DynSolType::Array(inner) | DynSolType::FixedArray(inner, _), Value::Array(items)) => {
            let items: Result<Vec<String>, String> =
                items.iter().map(|i| arg_text(inner, i)).collect();
            Ok(format!("[{}]", items?.join(",")))
        }
        (DynSolType::Tuple(types), Value::Array(items)) if types.len() == items.len() => {
            let items: Result<Vec<String>, String> = types
                .iter()
                .zip(items)
                .map(|(t, i)| arg_text(t, i))
                .collect();
            Ok(format!("({})", items?.join(",")))
        }
        (DynSolType::Uint(_), Value::String(s)) if s.trim().contains(' ') => {
            let quantity =
                Quantity::parse(s, &Denomination::default()).map_err(|e| e.to_string())?;
            Ok(quantity.value.to_string())
        }
        (DynSolType::String, Value::String(s)) => Ok(Value::String(s.clone()).to_string()),
        (_, Value::String(s)) => Ok(s.clone()),
        (_, Value::Number(n)) => Ok(n.to_string()),
        (_, Value::Bool(b)) => Ok(b.to_string()),
        (_, other) => Err(format!("cannot use {other} here")),
    }
}

/// Mixed-case addresses must carry a valid EIP-55 checksum.
fn check_checksum(text: &str) -> Result<(), String> {
    let digits = text.strip_prefix("0x").unwrap_or(text);
    let mixed = digits.chars().any(|c| c.is_ascii_uppercase())
        && digits.chars().any(|c| c.is_ascii_lowercase());
    if mixed && Address::parse_checksummed(text, None).is_err() {
        return Err("invalid EIP-55 checksum".to_owned());
    }
    if text.contains('.') && !text.starts_with("0x") {
        return Err(format!("'{text}' is an ENS name; resolve it first"));
    }
    Ok(())
}

/// Decode the return data of `function`.
pub fn decode_output(function: &Function, data: &[u8]) -> Result<Vec<NamedValue>, EvmError> {
    let values = function
        .abi_decode_output(data)
        .map_err(|e| EvmError::config(format!("cannot decode {} output: {e}", function.name)))?;
    Ok(named(&function.outputs, values))
}

/// Call data decoded with an ABI.
#[derive(Clone, Debug, PartialEq)]
pub struct DecodedCall {
    pub function: Function,
    pub args: Vec<NamedValue>,
}

impl fmt::Display for DecodedCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&signature_with_values(&self.function.name, &self.args))
    }
}

/// Decode call data (`selector ++ arguments`) with the function of `abi`
/// whose selector matches.
pub fn decode_input(abi: &JsonAbi, data: &[u8]) -> Option<DecodedCall> {
    let selector = data.get(..4)?;
    let function = abi.functions().find(|f| f.selector() == selector)?;
    let values = function.abi_decode_input(&data[4..]).ok()?;
    Some(DecodedCall {
        function: function.clone(),
        args: named(&function.inputs, values),
    })
}

/// A log decoded with an ABI: its fields in declaration order.
#[derive(Clone, Debug, PartialEq)]
pub struct DecodedLog {
    pub event: Event,
    pub fields: Vec<NamedValue>,
}

impl DecodedLog {
    pub fn field(&self, name: &str) -> Option<&NamedValue> {
        self.fields.iter().find(|f| f.name == name)
    }
}

impl fmt::Display for DecodedLog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&signature_with_values(&self.event.name, &self.fields))
    }
}

/// Decode a log with the event of `abi` whose signature hash is the first
/// topic and whose indexed-field count fits (ERC-20 and ERC-721
/// `Transfer` share a signature but not their topics).
pub fn decode_log(abi: &JsonAbi, topics: &[B256], data: &[u8]) -> Option<DecodedLog> {
    let first = topics.first()?;
    abi.events()
        .filter(|e| !e.anonymous && e.selector() == *first)
        .filter(|e| e.inputs.iter().filter(|p| p.indexed).count() + 1 == topics.len())
        .find_map(|event| {
            let decoded = event.decode_log_parts(topics.iter().copied(), data).ok()?;
            let mut indexed = decoded.indexed.into_iter();
            let mut body = decoded.body.into_iter();
            let fields = event
                .inputs
                .iter()
                .map(|p| {
                    let value = if p.indexed {
                        indexed.next()
                    } else {
                        body.next()
                    };
                    value.map(|value| NamedValue {
                        name: p.name.clone(),
                        ty: p.selector_type().into_owned(),
                        value,
                    })
                })
                .collect::<Option<Vec<_>>>()?;
            Some(DecodedLog {
                event: event.clone(),
                fields,
            })
        })
}

/// Why a call reverted.
#[derive(Clone, Debug, PartialEq)]
pub enum Revert {
    /// No revert data.
    Empty,
    /// `Error(string)`.
    Reason(String),
    /// `Panic(uint256)`.
    Panic(U256),
    /// A custom error of the loaded ABI.
    Custom {
        error: AbiError,
        fields: Vec<NamedValue>,
    },
    /// Data no known error decodes.
    Raw(Vec<u8>),
}

/// The Solidity meaning of a panic code.
pub fn panic_meaning(code: U256) -> &'static str {
    let Ok(code) = u8::try_from(code) else {
        return "unknown panic code";
    };
    match code {
        0x00 => "generic compiler panic",
        0x01 => "assertion failed",
        0x11 => "arithmetic overflow",
        0x12 => "division or modulo by zero",
        0x21 => "invalid enum value",
        0x22 => "invalid storage byte array encoding",
        0x31 => "pop on empty array",
        0x32 => "array index out of bounds",
        0x41 => "out of memory",
        0x51 => "call to an uninitialized function",
        _ => "unknown panic code",
    }
}

impl Revert {
    /// What `reverts_with` matches: the reason, the error name, or `Panic`.
    pub fn name(&self) -> Option<&str> {
        match self {
            Self::Reason(reason) => Some(reason),
            Self::Panic(_) => Some("Panic"),
            Self::Custom { error, .. } => Some(&error.name),
            Self::Empty | Self::Raw(_) => None,
        }
    }
}

impl fmt::Display for Revert {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("reverted (no data)"),
            Self::Reason(reason) => write!(f, "reverted: {reason}"),
            Self::Panic(code) => write!(f, "reverted: Panic({code:#x}) {}", panic_meaning(*code)),
            Self::Custom { error, fields } => {
                write!(
                    f,
                    "reverted: {}",
                    signature_with_values(&error.name, fields)
                )
            }
            Self::Raw(data) if data.len() < 4 => {
                write!(f, "reverted: {} (no selector)", hex::encode_prefixed(data))
            }
            Self::Raw(data) => write!(
                f,
                "reverted: {} (selector {})",
                hex::encode_prefixed(data),
                hex::encode_prefixed(&data[..4])
            ),
        }
    }
}

/// Decode revert data: `Error(string)`, `Panic(uint256)`, a custom error
/// of `abi`, or raw bytes.
pub fn decode_revert(abi: Option<&JsonAbi>, data: &[u8]) -> Revert {
    let Some(selector) = data.get(..4) else {
        return if data.is_empty() {
            Revert::Empty
        } else {
            Revert::Raw(data.to_vec())
        };
    };
    let body = &data[4..];
    if selector == ERROR_SELECTOR
        && let Ok(DynSolValue::String(reason)) = DynSolType::String.abi_decode(body)
    {
        return Revert::Reason(reason);
    }
    if selector == PANIC_SELECTOR
        && let Ok(DynSolValue::Uint(code, _)) = DynSolType::Uint(256).abi_decode(body)
    {
        return Revert::Panic(code);
    }
    abi.and_then(|abi| custom_error(abi, data))
        .unwrap_or_else(|| Revert::Raw(data.to_vec()))
}

fn custom_error(abi: &JsonAbi, data: &[u8]) -> Option<Revert> {
    let error = abi.errors().find(|e| e.selector() == data[..4])?;
    let decoded = error.decode_error(data).ok()?;
    Some(Revert::Custom {
        error: error.clone(),
        fields: named(&error.inputs, decoded.body),
    })
}

/// `0x`-prefixed hex as bytes.
pub fn parse_bytes(text: &str) -> Result<Vec<u8>, EvmError> {
    hex::decode(text.trim()).map_err(|e| EvmError::config(format!("invalid hex '{text}': {e}")))
}

#[cfg(test)]
mod tests {
    use alloy_primitives::address;
    use serde_json::json;

    use super::*;
    use crate::abi::built_in;

    fn custom_abi() -> JsonAbi {
        serde_json::from_value(json!([
            {"type": "error", "name": "InsufficientBalance", "inputs": [
                {"name": "available", "type": "uint256"}, {"name": "required", "type": "uint256"}]},
            {"type": "error", "name": "Unauthorized", "inputs": []},
            {"type": "function", "name": "positions", "stateMutability": "view",
             "inputs": [{"name": "account", "type": "address"}],
             "outputs": [{"name": "", "type": "tuple", "components": [
                {"name": "shares", "type": "uint256"}, {"name": "locked", "type": "bool"}]}]}
        ]))
        .unwrap()
    }

    #[test]
    fn encodes_a_transfer_with_known_calldata() {
        let erc20 = built_in("erc20").unwrap();
        let transfer = &erc20.function("transfer").unwrap()[0];
        let data = encode_call(
            transfer,
            &[
                json!("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045"),
                json!("1.5 ether"),
            ],
        )
        .unwrap();
        assert_eq!(
            hex::encode_prefixed(&data),
            "0xa9059cbb000000000000000000000000d8da6bf26964af9d7eed9e03e53415d37aa9604500000000000000000000000000000000000000000000000014d1120d7b160000"
        );
        let decoded = decode_input(&erc20, &data).unwrap();
        assert_eq!(
            decoded.to_string(),
            "transfer(to=0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045, amount=1500000000000000000)"
        );
    }

    #[test]
    fn argument_errors_name_the_parameter() {
        let erc20 = built_in("erc20").unwrap();
        let transfer = &erc20.function("transfer").unwrap()[0];
        let bad_checksum = encode_call(
            transfer,
            &[
                json!("0xd8DA6BF26964aF9D7eEd9e03E53415D37aA96045"),
                json!(1),
            ],
        )
        .unwrap_err();
        assert_eq!(
            bad_checksum.to_string(),
            "argument 'to' (address): invalid EIP-55 checksum"
        );
        let ens = encode_call(transfer, &[json!("vitalik.eth"), json!(1)]).unwrap_err();
        assert_eq!(
            ens.to_string(),
            "argument 'to' (address): 'vitalik.eth' is an ENS name; resolve it first"
        );
        let lower = encode_call(
            transfer,
            &[
                json!("0xd8da6bf26964af9d7eed9e03e53415d37aa96045"),
                json!({"a": 1}),
            ],
        )
        .unwrap_err();
        assert!(
            lower
                .to_string()
                .starts_with("argument 'amount' (uint256): cannot use"),
            "{lower}"
        );
        let count = encode_call(transfer, &[json!(1)]).unwrap_err();
        assert_eq!(
            count.to_string(),
            "function 'transfer(address,uint256)' takes 2 argument(s), got 1"
        );
    }

    #[test]
    fn tuples_arrays_and_strings_encode() {
        let abi: JsonAbi = serde_json::from_value(json!([
            {"type": "function", "name": "f", "stateMutability": "nonpayable", "outputs": [],
             "inputs": [
                {"name": "p", "type": "tuple", "components": [
                    {"name": "a", "type": "uint256"}, {"name": "b", "type": "bool"}]},
                {"name": "list", "type": "address[]"},
                {"name": "s", "type": "string"}]}
        ]))
        .unwrap();
        let f = &abi.function("f").unwrap()[0];
        let data = encode_call(
            f,
            &[
                json!([7, true]),
                json!(["0x0000000000000000000000000000000000000001"]),
                json!("hello, \"world\""),
            ],
        )
        .unwrap();
        let decoded = decode_input(&abi, &data).unwrap();
        assert_eq!(
            decoded.to_string(),
            "f(p=(7, true), list=[0x0000000000000000000000000000000000000001], s=\"hello, \\\"world\\\"\")"
        );
    }

    #[test]
    fn outputs_decode_with_names_and_json() {
        let abi = custom_abi();
        let f = &abi.function("positions").unwrap()[0];
        let data = DynSolValue::Tuple(vec![DynSolValue::Tuple(vec![
            DynSolValue::Uint(U256::from(5u8), 256),
            DynSolValue::Bool(true),
        ])])
        .abi_encode_params();
        let out = decode_output(f, &data).unwrap();
        assert_eq!(out[0].to_string(), "(5, true)");
        assert_eq!(value_to_json(&out[0].value), json!(["5", true]));
        let owner = DynSolValue::Address(address!("d8dA6BF26964aF9D7eEd9e03E53415D37aA96045"));
        assert_eq!(
            value_to_json(&owner),
            json!("0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")
        );
    }

    #[test]
    fn revert_vectors() {
        let reason = hex::decode("0x08c379a00000000000000000000000000000000000000000000000000000000000000020000000000000000000000000000000000000000000000000000000000000001a4e6f7420656e6f7567682045746865722070726f76696465642e000000000000").unwrap();
        assert_eq!(
            decode_revert(None, &reason).to_string(),
            "reverted: Not enough Ether provided."
        );
        let panic = hex::decode(
            "0x4e487b710000000000000000000000000000000000000000000000000000000000000012",
        )
        .unwrap();
        let revert = decode_revert(None, &panic);
        assert_eq!(
            revert.to_string(),
            "reverted: Panic(0x12) division or modulo by zero"
        );
        assert_eq!(revert.name(), Some("Panic"));
        let overflow = hex::decode(
            "0x4e487b710000000000000000000000000000000000000000000000000000000000000011",
        )
        .unwrap();
        assert_eq!(
            decode_revert(None, &overflow).to_string(),
            "reverted: Panic(0x11) arithmetic overflow"
        );
        let abi = custom_abi();
        let custom = abi.error("InsufficientBalance").unwrap()[0].selector();
        let mut data = custom.to_vec();
        data.extend(
            DynSolValue::Tuple(vec![
                DynSolValue::Uint(U256::from(5u8), 256),
                DynSolValue::Uint(U256::from(10u8), 256),
            ])
            .abi_encode_params(),
        );
        let revert = decode_revert(Some(&abi), &data);
        assert_eq!(
            revert.to_string(),
            "reverted: InsufficientBalance(available=5, required=10)"
        );
        assert_eq!(revert.name(), Some("InsufficientBalance"));
        let unauthorized = abi.error("Unauthorized").unwrap()[0].selector();
        assert_eq!(
            decode_revert(Some(&abi), unauthorized.as_slice()).to_string(),
            "reverted: Unauthorized()"
        );
        assert_eq!(
            decode_revert(None, &data).to_string(),
            format!(
                "reverted: {} (selector {})",
                hex::encode_prefixed(&data),
                hex::encode_prefixed(custom)
            )
        );
        assert_eq!(decode_revert(None, &[]).to_string(), "reverted (no data)");
        assert_eq!(
            decode_revert(None, &[0xab]).to_string(),
            "reverted: 0xab (no selector)"
        );
    }

    #[test]
    fn panic_meanings() {
        assert_eq!(panic_meaning(U256::from(0x01u8)), "assertion failed");
        assert_eq!(
            panic_meaning(U256::from(0x32u8)),
            "array index out of bounds"
        );
        assert_eq!(panic_meaning(U256::from(0x99u8)), "unknown panic code");
        assert_eq!(panic_meaning(U256::MAX), "unknown panic code");
    }

    #[test]
    fn logs_decode_by_signature_and_topic_count() {
        let erc20 = built_in("erc20").unwrap();
        let erc721 = built_in("erc721").unwrap();
        let topic = erc20.event("Transfer").unwrap()[0].selector();
        let from = B256::left_padding_from(
            address!("d8dA6BF26964aF9D7eEd9e03E53415D37aA96045").as_slice(),
        );
        let to = B256::left_padding_from(&[1]);
        let value = U256::from(250_000_000u64).to_be_bytes::<32>();
        let log = decode_log(&erc20, &[topic, from, to], &value).unwrap();
        assert_eq!(
            log.to_string(),
            "Transfer(from=0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045, to=0x0000000000000000000000000000000000000001, value=250000000)"
        );
        assert!(decode_log(&erc721, &[topic, from, to], &value).is_none());
        let nft = decode_log(
            &erc721,
            &[topic, from, to, B256::from(U256::from(7u8))],
            &[],
        )
        .unwrap();
        assert_eq!(nft.field("tokenId").unwrap().to_string(), "tokenId=7");
        assert!(decode_log(&erc20, &[], &[]).is_none());
    }
}
