//! One-shot reads for `rustman evm block`, `rustman evm logs --from-block`,
//! `rustman evm decode` and the agent tools (tasks 4.2, 4.4): a block by
//! number, tag or hash; logs over a bounded block range; and offline
//! decoding of call data, revert data or a log with an ABI. Read-only.

use alloy_json_abi::JsonAbi;
use alloy_primitives::{B256, U256, hex};
use serde_json::{Value, json};

use crate::call::BlockSel;
use crate::codec::{DecodedLog, decode_input, decode_log, decode_revert, value_to_json};
use crate::error::EvmError;
use crate::quantity::{Denomination, Form, GWEI_DECIMALS, Quantity, format_units, parse_hex_u64};
use crate::rpc::{Rpc, call};
use crate::stream::LogFilter;

/// The widest block range an agent's log query (`evm_logs_query`) may span.
/// `rustman evm logs` is not capped: the user asked for the range.
pub const MAX_LOG_RANGE: u64 = 10_000;

/// The error of a log query over more than [`MAX_LOG_RANGE`] blocks.
pub const RANGE_TOO_LARGE: &str = "range too large (max 10000 blocks)";

/// The message of a block the node does not know.
pub const BLOCK_NOT_FOUND: &str = "block not found";

/// The latest block number.
pub async fn block_number(rpc: &dyn Rpc) -> Result<u64, EvmError> {
    let value = call(rpc, "eth_blockNumber", json!([])).await?;
    value
        .as_str()
        .and_then(|n| parse_hex_u64(n).ok())
        .ok_or_else(|| EvmError::Rpc(format!("eth_blockNumber: not a quantity: {value}")))
}

/// A block bound as a number: tags other than `earliest` read the head.
async fn bound(rpc: &dyn Rpc, block: &BlockSel, head: &mut Option<u64>) -> Result<u64, EvmError> {
    match block {
        BlockSel::Number(n) => Ok(*n),
        BlockSel::Tag(tag) if tag == "earliest" => Ok(0),
        BlockSel::Tag(_) => match *head {
            Some(n) => Ok(n),
            None => {
                let n = block_number(rpc).await?;
                *head = Some(n);
                Ok(n)
            }
        },
        BlockSel::Hash(_) => Err(EvmError::config(
            "a log range takes block numbers or tags, not hashes",
        )),
    }
}

/// The block numbers `from..=to` stand for, refused when `to` is before
/// `from`. Only the head is read, and only for tags. Not capped: see
/// [`log_range`] for the agent's cap.
pub async fn block_range(
    rpc: &dyn Rpc,
    from: &BlockSel,
    to: &BlockSel,
) -> Result<(u64, u64), EvmError> {
    let mut head = None;
    let from = bound(rpc, from, &mut head).await?;
    let to = bound(rpc, to, &mut head).await?;
    check_order(from, to)?;
    Ok((from, to))
}

/// [`block_range`], refused when it spans more than [`MAX_LOG_RANGE`]
/// blocks (the agent tool).
pub async fn log_range(
    rpc: &dyn Rpc,
    from: &BlockSel,
    to: &BlockSel,
) -> Result<(u64, u64), EvmError> {
    let (from, to) = block_range(rpc, from, to).await?;
    check_range(from, to)?;
    Ok((from, to))
}

fn check_order(from: u64, to: u64) -> Result<(), EvmError> {
    if to < from {
        return Err(EvmError::config(format!(
            "the range ends ({to}) before it starts ({from})"
        )));
    }
    Ok(())
}

/// `to` is not before `from`, and the range spans at most
/// [`MAX_LOG_RANGE`] blocks.
pub fn check_range(from: u64, to: u64) -> Result<(), EvmError> {
    check_order(from, to)?;
    if to - from >= MAX_LOG_RANGE {
        return Err(EvmError::config(RANGE_TOO_LARGE));
    }
    Ok(())
}

/// The logs matching `filter` in blocks `from..=to` (already checked).
pub async fn get_logs(
    rpc: &dyn Rpc,
    filter: &LogFilter,
    from: u64,
    to: u64,
) -> Result<Vec<Value>, EvmError> {
    let mut object = filter.to_json();
    if let Value::Object(map) = &mut object {
        map.insert("fromBlock".into(), json!(format!("{from:#x}")));
        map.insert("toBlock".into(), json!(format!("{to:#x}")));
    }
    let value = call(rpc, "eth_getLogs", json!([object])).await?;
    match value {
        Value::Array(logs) => Ok(logs),
        other => Err(EvmError::Rpc(format!("eth_getLogs: not a list: {other}"))),
    }
}

/// A block as the node sent it (without full transactions).
pub async fn fetch_block(rpc: &dyn Rpc, block: &BlockSel) -> Result<Value, EvmError> {
    let (method, params) = match block {
        BlockSel::Hash(hash) => ("eth_getBlockByHash", json!([hash.to_string(), false])),
        other => ("eth_getBlockByNumber", json!([other.to_param(), false])),
    };
    match call(rpc, method, params).await? {
        Value::Null => Err(EvmError::NotFound(BLOCK_NOT_FOUND.to_owned())),
        block => Ok(block),
    }
}

/// A block's main fields, quantities decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockView {
    pub number: Option<u64>,
    pub hash: Option<String>,
    pub parent: Option<String>,
    pub timestamp: Option<u64>,
    pub miner: Option<String>,
    pub gas_used: Option<U256>,
    pub gas_limit: Option<U256>,
    pub base_fee: Option<U256>,
    pub transactions: usize,
}

fn text(block: &Value, key: &str) -> Option<String> {
    block[key].as_str().map(str::to_owned)
}

fn quantity(block: &Value, key: &str) -> Option<U256> {
    block[key]
        .as_str()
        .and_then(|q| crate::quantity::parse_hex(q).ok())
}

impl BlockView {
    pub fn of(block: &Value) -> Self {
        let number = |key: &str| block[key].as_str().and_then(|n| parse_hex_u64(n).ok());
        Self {
            number: number("number"),
            hash: text(block, "hash"),
            parent: text(block, "parentHash"),
            timestamp: number("timestamp"),
            miner: text(block, "miner"),
            gas_used: quantity(block, "gasUsed"),
            gas_limit: quantity(block, "gasLimit"),
            base_fee: quantity(block, "baseFeePerGas"),
            transactions: block["transactions"].as_array().map_or(0, Vec::len),
        }
    }

    /// One `name: value` line per field.
    pub fn render(&self) -> String {
        let show = |v: Option<String>| v.unwrap_or_else(|| "-".to_owned());
        let gwei = |v: U256| format!("{} gwei", format_units(v, GWEI_DECIMALS));
        [
            format!("number: {}", show(self.number.map(|n| n.to_string()))),
            format!("hash: {}", show(self.hash.clone())),
            format!("parent: {}", show(self.parent.clone())),
            format!("timestamp: {}", show(self.timestamp.map(|t| t.to_string()))),
            format!("miner: {}", show(self.miner.clone())),
            format!(
                "gas used: {} / {}",
                show(self.gas_used.map(|g| g.to_string())),
                show(self.gas_limit.map(|g| g.to_string()))
            ),
            format!("base fee: {}", show(self.base_fee.map(gwei))),
            format!("transactions: {}", self.transactions),
        ]
        .join("\n")
    }

    pub fn to_json(&self) -> Value {
        json!({
            "number": self.number,
            "hash": self.hash,
            "parent_hash": self.parent,
            "timestamp": self.timestamp,
            "miner": self.miner,
            "gas_used": self.gas_used.map(|g| g.to_string()),
            "gas_limit": self.gas_limit.map(|g| g.to_string()),
            "base_fee_per_gas": self.base_fee.map(|g| g.to_string()),
            "transactions": self.transactions,
        })
    }
}

/// What offline decoding found.
#[derive(Clone, Debug, PartialEq)]
pub enum Decoded {
    /// Call data of a function of the ABI.
    Call(crate::codec::DecodedCall),
    /// A log of an event of the ABI.
    Log(DecodedLog),
    /// Revert data (`Error`, `Panic`, a custom error, or raw).
    Revert(crate::codec::Revert),
}

impl Decoded {
    /// `transfer(to=0x…, amount=…)`, `Transfer(from=…, …)` or
    /// `reverted: …`.
    pub fn render(&self) -> String {
        match self {
            Self::Call(call) => call.to_string(),
            Self::Log(log) => log.to_string(),
            Self::Revert(revert) => revert.to_string(),
        }
    }

    pub fn to_json(&self) -> Value {
        let fields = |values: &[crate::codec::NamedValue]| {
            values
                .iter()
                .enumerate()
                .map(|(i, v)| (crate::call::output_key(v, i), value_to_json(&v.value)))
                .collect::<serde_json::Map<_, _>>()
        };
        match self {
            Self::Call(call) => json!({
                "kind": "call", "function": call.function.signature(), "args": fields(&call.args),
            }),
            Self::Log(log) => json!({
                "kind": "log", "event": log.event.signature(), "fields": fields(&log.fields),
            }),
            Self::Revert(revert) => json!({
                "kind": "revert", "name": revert.name(), "message": revert.to_string(),
            }),
        }
    }
}

/// Decode `data` with `abi`, offline: a log when `topics` are given, else
/// call data when its selector names a function, else revert data.
pub fn decode_offline(abi: &JsonAbi, data: &[u8], topics: &[B256]) -> Result<Decoded, EvmError> {
    if !topics.is_empty() {
        return decode_log(abi, topics, data)
            .map(Decoded::Log)
            .ok_or_else(|| {
                EvmError::config(format!("no event of the ABI decodes topic {}", topics[0]))
            });
    }
    if let Some(call) = decode_input(abi, data) {
        return Ok(Decoded::Call(call));
    }
    let revert = decode_revert(Some(abi), data);
    if matches!(revert, crate::codec::Revert::Raw(_)) && data.len() >= 4 {
        return Err(EvmError::config(format!(
            "no function or error of the ABI has selector {}",
            hex::encode_prefixed(&data[..4])
        )));
    }
    Ok(Decoded::Revert(revert))
}

/// A native amount as `1 ETH`, for balance-like fields.
pub fn native_amount(value: U256, native: &Denomination) -> String {
    Quantity::from(value).show(Form::Native, native)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abi::built_in;
    use crate::recorded::Recorded;

    #[test]
    fn ranges_are_capped() {
        assert!(check_range(0, 9_999).is_ok());
        let err = check_range(0, 50_000).unwrap_err();
        assert_eq!(err.to_string(), RANGE_TOO_LARGE);
        assert_eq!(err.exit_code(), 2);
        assert!(check_range(10, 9).is_err());
    }

    #[tokio::test]
    async fn tags_read_the_head_once() {
        let rpc = Recorded::from_json(
            "fixture",
            r#"[{"method": "eth_blockNumber", "result": "0x4e20"}]"#,
        )
        .unwrap();
        let from = BlockSel::Number(19_000);
        let latest = BlockSel::default();
        assert_eq!(
            log_range(&rpc, &from, &latest).await.unwrap(),
            (19_000, 20_000)
        );
        let err = log_range(&rpc, &BlockSel::Tag("earliest".into()), &latest)
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), RANGE_TOO_LARGE);
        assert_eq!(rpc.methods(), ["eth_blockNumber", "eth_blockNumber"]);
        // `rustman evm logs` is not capped: only agents are.
        let wide = block_range(&rpc, &BlockSel::Number(0), &BlockSel::Number(50_000)).await;
        assert_eq!(wide.unwrap(), (0, 50_000));
        let backwards = block_range(&rpc, &BlockSel::Number(9), &BlockSel::Number(8)).await;
        assert!(backwards.is_err());
    }

    #[test]
    fn calldata_reverts_and_logs_decode_offline() {
        let erc20 = built_in("erc20").unwrap();
        let data = hex::decode(
            "a9059cbb000000000000000000000000d8da6bf26964af9d7eed9e03e53415d37aa96045000000000000000000000000000000000000000000000000000000000000000a",
        )
        .unwrap();
        let decoded = decode_offline(&erc20, &data, &[]).unwrap();
        assert_eq!(
            decoded.render(),
            "transfer(to=0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045, amount=10)"
        );
        let panic =
            hex::decode("4e487b710000000000000000000000000000000000000000000000000000000000000012")
                .unwrap();
        assert_eq!(
            decode_offline(&erc20, &panic, &[]).unwrap().render(),
            "reverted: Panic(0x12) division or modulo by zero"
        );
        assert!(decode_offline(&erc20, &[1, 2, 3, 4, 5], &[]).is_err());
        let topics = [
            crate::stream::topic("Transfer(address,address,uint256)").unwrap(),
            B256::ZERO,
            B256::left_padding_from(&[0xab]),
        ];
        let amount = U256::from(7).to_be_bytes::<32>();
        let log = decode_offline(&erc20, &amount, &topics).unwrap();
        assert_eq!(
            log.render(),
            "Transfer(from=0x0000000000000000000000000000000000000000, to=0x00000000000000000000000000000000000000AB, value=7)"
        );
        assert_eq!(log.to_json()["fields"]["value"], json!("7"));
    }

    #[test]
    fn blocks_render_their_fields() {
        let block = json!({
            "number": "0x10", "hash": "0xaa", "parentHash": "0xbb", "timestamp": "0x5",
            "miner": "0x0000000000000000000000000000000000000000",
            "gasUsed": "0x5208", "gasLimit": "0x1c9c380", "baseFeePerGas": "0x3b9aca00",
            "transactions": ["0x1", "0x2"],
        });
        let view = BlockView::of(&block);
        insta::assert_snapshot!(view.render());
        assert_eq!(view.to_json()["gas_used"], json!("21000"));
    }
}
