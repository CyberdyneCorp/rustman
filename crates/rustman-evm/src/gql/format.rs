//! Blockchain scalar display (blockchain-graphql "Blockchain scalar
//! display"): `BigInt`, `BigDecimal` and `Long` as exact decimals, `Bytes`
//! and `Bytes32` as `0x` hex, addresses (typed `Address`, and any other
//! 20-byte `0x` value, as the TUI's links take them) EIP-55 checksummed.
//! Display only: assertions and captures read the raw JSON, and raw mode
//! shows it unchanged.

use std::collections::BTreeMap;

use alloy_primitives::{Address, U256};
use serde_json::{Map, Value};

use crate::quantity::{format_units, parse_hex};

/// How a result is shown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Display {
    /// Blockchain scalars formatted.
    #[default]
    Formatted,
    /// Formatted, and a `BigInt` next to a `decimals` field also in token
    /// units: `1000 (18 decimals)`.
    Units,
    /// The JSON as received.
    Raw,
}

impl Display {
    /// Formatted → units → raw → formatted.
    pub fn next(self) -> Self {
        match self {
            Self::Formatted => Self::Units,
            Self::Units => Self::Raw,
            Self::Raw => Self::Formatted,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Formatted => "formatted",
            Self::Units => "units",
            Self::Raw => "raw",
        }
    }
}

/// `data` for display: each leaf at a path of `scalars` (as
/// `rustman_graphql::preset::scalar_paths` names them) formatted per its
/// scalar, and every 20-byte `0x` value checksummed as an address.
pub fn format_data(data: &Value, scalars: &BTreeMap<String, String>, display: Display) -> Value {
    if display == Display::Raw {
        return data.clone();
    }
    let walker = Walker {
        scalars,
        units: display == Display::Units,
    };
    walker.value(data, "")
}

struct Walker<'s> {
    scalars: &'s BTreeMap<String, String>,
    units: bool,
}

impl Walker<'_> {
    fn value(&self, value: &Value, path: &str) -> Value {
        match value {
            Value::Object(object) => Value::Object(self.object(object, path)),
            Value::Array(items) => {
                let items = items.iter().map(|v| self.value(v, path));
                Value::Array(items.collect())
            }
            leaf => self.leaf(leaf, path, None),
        }
    }

    fn object(&self, object: &Map<String, Value>, prefix: &str) -> Map<String, Value> {
        let decimals = object.get("decimals").and_then(decimals);
        object
            .iter()
            .map(|(key, value)| {
                let path = child_path(prefix, key, value.is_array());
                let shown = match value {
                    Value::Object(_) | Value::Array(_) => self.value(value, &path),
                    leaf => self.leaf(leaf, &path, decimals),
                };
                (key.clone(), shown)
            })
            .collect()
    }

    fn leaf(&self, value: &Value, path: &str, decimals: Option<u8>) -> Value {
        let scalar = self.scalars.get(path).map(String::as_str);
        let shown = match (scalar, value) {
            (Some("BigInt"), _) => big_int(value, self.units.then_some(decimals).flatten()),
            (Some("Long"), _) => big_int(value, None),
            (Some("BigDecimal"), _) => big_decimal(value),
            (Some("Address"), Value::String(text)) => checksum(text),
            (Some("Bytes" | "Bytes32"), Value::String(text)) => bytes(text),
            // Any 20-byte `0x` value is an address, as the TUI's links
            // take it (a subgraph's `id` is often one).
            (None | Some("ID" | "String"), Value::String(text)) => checksum(text),
            _ => None,
        };
        shown.map_or_else(|| value.clone(), Value::String)
    }
}

/// The path of `key` under `prefix`; lists get `[]` like scalar paths.
fn child_path(prefix: &str, key: &str, list: bool) -> String {
    let list = if list { "[]" } else { "" };
    match prefix {
        "" => format!("{key}{list}"),
        _ => format!("{prefix}.{key}{list}"),
    }
}

fn decimals(value: &Value) -> Option<u8> {
    match value {
        Value::Number(n) => n.as_u64().and_then(|n| u8::try_from(n).ok()),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

/// A `BigInt` or `Long` as an exact decimal (from a decimal string, hex or
/// a JSON number), with token units when `decimals` is given.
fn big_int(value: &Value, decimals: Option<u8>) -> Option<String> {
    let parsed = match value {
        Value::String(text) if text.starts_with("0x") => parse_hex(text).ok(),
        Value::String(text) => U256::from_str_radix(text, 10).ok(),
        Value::Number(n) => n.as_u64().map(U256::from),
        _ => None,
    }?;
    Some(match decimals {
        Some(d) => format!("{} ({d} decimals)", format_units(parsed, d)),
        None => parsed.to_string(),
    })
}

/// The largest exponent a `BigDecimal` is expanded for; beyond it the value
/// is shown as received rather than as thousands of zeros.
const MAX_EXPONENT: u64 = 1000;

/// A `BigDecimal` as an exact plain decimal: `1.5e-3` as `0.0015`, `2E+3`
/// as `2000`. Digits are kept as received, never rounded.
fn big_decimal(value: &Value) -> Option<String> {
    let text = match value {
        Value::String(text) => text.clone(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    let (mantissa, exponent) = match text.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, exponent.parse::<i64>().ok()?),
        None => (text.as_str(), 0),
    };
    if exponent.unsigned_abs() > MAX_EXPONENT {
        return None;
    }
    let (sign, unsigned) = match mantissa.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", mantissa.strip_prefix('+').unwrap_or(mantissa)),
    };
    let (int, frac) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    let digits = format!("{int}{frac}");
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let point = i64::try_from(int.len()).ok()? + exponent;
    let shown = place_point(&digits, point);
    let sign = if shown.bytes().all(|b| b == b'0' || b == b'.') {
        ""
    } else {
        sign
    };
    Some(format!("{sign}{shown}"))
}

/// `digits` with the decimal point after the first `point` of them
/// (padding with zeros either side), without leading integer zeros.
fn place_point(digits: &str, point: i64) -> String {
    let (int, frac) = match usize::try_from(point) {
        Err(_) => (
            String::new(),
            format!("{}{digits}", "0".repeat(point.unsigned_abs() as usize)),
        ),
        Ok(at) if at >= digits.len() => (
            format!("{digits}{}", "0".repeat(at - digits.len())),
            String::new(),
        ),
        Ok(at) => (digits[..at].to_owned(), digits[at..].to_owned()),
    };
    let int = match int.trim_start_matches('0') {
        "" => "0",
        trimmed => trimmed,
    };
    match frac.is_empty() {
        true => int.to_owned(),
        false => format!("{int}.{frac}"),
    }
}

/// Bytes as `0x` hex; 20 bytes (a subgraph's usual address id) checksummed.
fn bytes(text: &str) -> Option<String> {
    let digits = text.strip_prefix("0x").unwrap_or(text);
    if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let hex = format!("0x{}", digits.to_ascii_lowercase());
    match digits.len() == 40 {
        true => checksum(&hex),
        false => Some(hex),
    }
}

/// A 20-byte hex value in EIP-55 form.
pub fn checksum(text: &str) -> Option<String> {
    let digits = text.strip_prefix("0x")?;
    if digits.len() != 40 {
        return None;
    }
    let address: Address = format!("0x{}", digits.to_ascii_lowercase()).parse().ok()?;
    Some(address.to_checksum(None))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn scalars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn big_ints_with_and_without_units() {
        let data = json!({"token": {"totalSupply": "1000000000000000000000", "decimals": 18}});
        let map = scalars(&[("token.totalSupply", "BigInt")]);
        let shown = format_data(&data, &map, Display::Formatted);
        assert_eq!(shown["token"]["totalSupply"], "1000000000000000000000");
        let units = format_data(&data, &map, Display::Units);
        assert_eq!(units["token"]["totalSupply"], "1000 (18 decimals)");
        assert_eq!(format_data(&data, &map, Display::Raw), data);
    }

    #[test]
    fn big_decimals_show_as_exact_plain_decimals() {
        let data = json!({"pool": {
            "price": "1.5e-3", "volume": "2E+3", "fee": "0.000300",
            "tvl": "-12.5e1", "zero": "-0e5", "n": 1.25, "huge": "1e100000", "bad": "abc"
        }});
        let map = scalars(&[
            ("pool.price", "BigDecimal"),
            ("pool.volume", "BigDecimal"),
            ("pool.fee", "BigDecimal"),
            ("pool.tvl", "BigDecimal"),
            ("pool.zero", "BigDecimal"),
            ("pool.n", "BigDecimal"),
            ("pool.huge", "BigDecimal"),
            ("pool.bad", "BigDecimal"),
        ]);
        let shown = format_data(&data, &map, Display::Formatted);
        assert_eq!(
            shown,
            json!({"pool": {
                "price": "0.0015", "volume": "2000", "fee": "0.000300",
                "tvl": "-125", "zero": "0", "n": "1.25", "huge": "1e100000", "bad": "abc"
            }})
        );
        assert_eq!(format_data(&data, &map, Display::Raw), data);
    }

    #[test]
    fn geth_hex_longs_become_decimals() {
        let data = json!({"block": {"number": "0x10", "gasUsed": 21000, "hash": "AB"}});
        let map = scalars(&[
            ("block.number", "Long"),
            ("block.gasUsed", "Long"),
            ("block.hash", "Bytes32"),
        ]);
        let shown = format_data(&data, &map, Display::Formatted);
        assert_eq!(
            shown,
            json!({"block": {"number": "16", "gasUsed": "21000", "hash": "0xab"}})
        );
    }

    #[test]
    fn every_20_byte_value_is_checksummed() {
        let lower = "0xd8da6bf26964af9d7eed9e03e53415d37aa96045";
        let data = json!({"transfers": [{"from": lower, "token": {"id": lower}, "miner": lower}]});
        let map = scalars(&[("transfers[].miner", "Address")]);
        let shown = format_data(&data, &map, Display::Formatted);
        let item = &shown["transfers"][0];
        assert_eq!(item["from"], "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045");
        assert_eq!(item["miner"], "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045");
        assert_eq!(
            item["token"]["id"], "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045",
            "any 20-byte value is an address, whatever its name"
        );
        let raw = format_data(&data, &map, Display::Raw);
        assert_eq!(raw["transfers"][0]["token"]["id"], lower, "raw keeps it");
        let other = json!({"note": "0xd8da", "name": "vitalik"});
        assert_eq!(format_data(&other, &map, Display::Formatted), other);
        let account = json!({"account": {"id": lower}, "hash": "0xAB"});
        let typed = scalars(&[("account.id", "Bytes"), ("hash", "Bytes")]);
        let shown = format_data(&account, &typed, Display::Formatted);
        assert_eq!(
            shown["account"]["id"], "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045",
            "a 20-byte Bytes id is an address"
        );
        assert_eq!(shown["hash"], "0xab");
        assert_eq!(checksum("0x12"), None);
        assert_eq!(Display::Formatted.next().next().next(), Display::Formatted);
    }
}
