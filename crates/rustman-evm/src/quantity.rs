//! Quantities (evm-toolkit "Unit-aware value display", design D5): 256-bit
//! integers parsed from and shown as hex, decimal, wei, gwei, ether (or the
//! chain's native symbol) and token units. Integer arithmetic only.

use std::fmt;

use alloy_primitives::U256;
use serde::Serialize;

use crate::chain::{ChainInfo, NATIVE_DECIMALS};

/// The decimals of gwei.
pub const GWEI_DECIMALS: u8 = 9;

/// A currency: its symbol and decimals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Denomination {
    pub symbol: String,
    pub decimals: u8,
}

impl Default for Denomination {
    /// Ether: `ETH` with 18 decimals.
    fn default() -> Self {
        Self {
            symbol: "ETH".to_owned(),
            decimals: NATIVE_DECIMALS,
        }
    }
}

impl Denomination {
    /// The native currency of `chain`.
    pub fn native(chain: &ChainInfo) -> Self {
        Self {
            symbol: chain.symbol.clone(),
            decimals: chain.decimals,
        }
    }

    /// The decimals `unit` stands for: `wei`, `gwei`, `ether`/`eth`, or
    /// this denomination's symbol (any case).
    fn unit_decimals(&self, unit: &str) -> Option<u8> {
        match unit.to_ascii_lowercase().as_str() {
            "wei" => Some(0),
            "gwei" => Some(GWEI_DECIMALS),
            "ether" | "eth" => Some(NATIVE_DECIMALS),
            other if other == self.symbol.to_ascii_lowercase() => Some(self.decimals),
            _ => None,
        }
    }
}

/// How a quantity is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Form {
    /// In the native currency: `1 ETH`.
    Native,
    Wei,
    Gwei,
    /// A plain decimal integer.
    Decimal,
    Hex,
}

/// The forms a quantity toggles through, the first being the default.
pub fn forms(native_value: bool) -> &'static [Form] {
    if native_value {
        &[Form::Native, Form::Wei, Form::Gwei, Form::Hex]
    } else {
        &[Form::Decimal, Form::Hex]
    }
}

/// Whether a field named `name` holds an amount of the native currency
/// (`value`, `balance`, gas prices and fees).
pub fn is_native_field(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "value" || lower == "balance" || lower.contains("gasprice") || lower.contains("fee")
}

/// A parse failure, naming the text.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid quantity '{text}': {reason}")]
pub struct QuantityError {
    pub text: String,
    pub reason: String,
}

fn invalid(text: &str, reason: impl Into<String>) -> QuantityError {
    QuantityError {
        text: text.to_owned(),
        reason: reason.into(),
    }
}

/// A 256-bit quantity, with the hex text a node sent when it came from one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Quantity {
    pub value: U256,
    /// The hex as received, shown as is by [`Form::Hex`].
    pub raw: Option<String>,
}

impl From<U256> for Quantity {
    fn from(value: U256) -> Self {
        Self { value, raw: None }
    }
}

impl Quantity {
    /// Parse what a user writes: `0x…` hex, a decimal integer, or
    /// `<decimal> <unit>` with unit `wei`, `gwei`, `ether` or the
    /// denomination's symbol.
    pub fn parse(text: &str, native: &Denomination) -> Result<Self, QuantityError> {
        let trimmed = text.trim();
        if trimmed.starts_with("0x") || trimmed.starts_with("0X") {
            return parse_hex(trimmed).map(Self::from);
        }
        let (amount, unit) = match trimmed.split_once(char::is_whitespace) {
            Some((amount, unit)) => (amount, unit.trim()),
            None => (trimmed, "wei"),
        };
        let decimals = native
            .unit_decimals(unit)
            .ok_or_else(|| invalid(text, format!("unknown unit '{unit}'")))?;
        parse_units(amount, decimals)
            .map(Self::from)
            .map_err(|reason| invalid(text, reason))
    }

    /// A node's hex quantity, kept as received for display.
    pub fn from_rpc(text: &str) -> Result<Self, QuantityError> {
        Ok(Self {
            value: parse_hex(text)?,
            raw: Some(text.to_owned()),
        })
    }

    /// The minimal hex a node expects: `0x1`.
    pub fn rpc_hex(&self) -> String {
        format!("{:#x}", self.value)
    }

    /// The quantity shown in `form`.
    pub fn show(&self, form: Form, native: &Denomination) -> String {
        match form {
            Form::Native => format!(
                "{} {}",
                format_units(self.value, native.decimals),
                native.symbol
            ),
            Form::Wei => format!("{} wei", self.value),
            Form::Gwei => format!("{} gwei", format_units(self.value, GWEI_DECIMALS)),
            Form::Decimal => self.value.to_string(),
            Form::Hex => self.raw.clone().unwrap_or_else(|| self.rpc_hex()),
        }
    }
}

impl fmt::Display for Quantity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.value.fmt(f)
    }
}

/// `0x`-prefixed hex as a 256-bit integer.
pub fn parse_hex(text: &str) -> Result<U256, QuantityError> {
    let digits = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .ok_or_else(|| invalid(text, "hex must start with 0x"))?;
    if digits.is_empty() {
        return Err(invalid(text, "no digits"));
    }
    U256::from_str_radix(digits, 16).map_err(|e| invalid(text, e.to_string()))
}

/// `0x`-prefixed hex as a `u64` (chain ids, block numbers, nonces).
pub fn parse_hex_u64(text: &str) -> Result<u64, QuantityError> {
    let value = parse_hex(text)?;
    u64::try_from(value).map_err(|_| invalid(text, "too large"))
}

/// `10^decimals`, or `None` past the 256-bit range (`decimals` > 77).
fn ten_pow(decimals: u8) -> Option<U256> {
    U256::from(10u8).checked_pow(U256::from(decimals))
}

/// The decimal `amount` (`1.5`, `250`, `.25`) in units of `10^-decimals`:
/// exact, or an error when it has more decimals than the unit allows.
pub fn parse_units(amount: &str, decimals: u8) -> Result<U256, String> {
    let (whole, fraction) = amount.split_once('.').unwrap_or((amount, ""));
    let fraction = fraction.trim_end_matches('0');
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if (whole.is_empty() && fraction.is_empty()) || !digits(whole) || !digits(fraction) {
        return Err("expected a decimal number, hex or '<number> <unit>'".to_owned());
    }
    if fraction.len() > usize::from(decimals) {
        return Err(format!("more than {decimals} decimals"));
    }
    let overflow = || "out of range for uint256".to_owned();
    let parse = |s: &str| {
        if s.is_empty() {
            Ok(U256::ZERO)
        } else {
            U256::from_str_radix(s, 10).map_err(|_| overflow())
        }
    };
    let pad = u8::try_from(fraction.len()).map_err(|_| overflow())?;
    let scaled = |digits: U256, by: u8| match ten_pow(by) {
        Some(scale) => digits.checked_mul(scale),
        None => digits.is_zero().then_some(U256::ZERO),
    };
    let whole = scaled(parse(whole)?, decimals).ok_or_else(overflow)?;
    let fraction = scaled(parse(fraction)?, decimals - pad).ok_or_else(overflow)?;
    whole.checked_add(fraction).ok_or_else(overflow)
}

/// `value` in units of `10^-decimals`, without trailing zeros: `1`, `0.5`.
pub fn format_units(value: U256, decimals: u8) -> String {
    format_fixed(value, decimals, 0)
}

/// `value` in units of `10^-decimals` with at least `min_fraction`
/// decimals: `250.00` for 250 USDC with `min_fraction` 2.
pub fn format_fixed(value: U256, decimals: u8, min_fraction: usize) -> String {
    if decimals == 0 {
        return value.to_string();
    }
    let (whole, fraction) = match ten_pow(decimals) {
        Some(scale) => (value / scale, (value % scale).to_string()),
        None => (U256::ZERO, value.to_string()),
    };
    let padded = format!("{fraction:0>width$}", width = usize::from(decimals));
    let trimmed = padded.trim_end_matches('0');
    let keep = trimmed.len().max(min_fraction.min(padded.len()));
    if keep == 0 {
        whole.to_string()
    } else {
        format!("{whole}.{}", &padded[..keep])
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn eth() -> Denomination {
        Denomination::default()
    }

    #[test]
    fn exact_input_parsing() {
        let q = Quantity::parse("0.000000000000000001 ether", &eth()).unwrap();
        assert_eq!(q.rpc_hex(), "0x1");
        let cases = [
            ("1.5 ether", "1500000000000000000"),
            ("30 gwei", "30000000000"),
            ("0x0de0b6b3a7640000", "1000000000000000000"),
            ("1000000000000000000", "1000000000000000000"),
            ("2 ETH", "2000000000000000000"),
            (".5 gwei", "500000000"),
            ("1.50000000000000000000 ether", "1500000000000000000"),
        ];
        for (text, wei) in cases {
            let q = Quantity::parse(text, &eth()).unwrap();
            assert_eq!(q.value.to_string(), wei, "{text}");
        }
    }

    #[test]
    fn bad_inputs_name_the_problem() {
        let cases = [
            ("1.5", "more than 0 decimals"),
            ("0.0000000001 gwei", "more than 9 decimals"),
            ("1 finney", "unknown unit 'finney'"),
            ("-1 ether", "expected a decimal number"),
            ("1e18", "expected a decimal number"),
            ("0x", "no digits"),
            ("0xzz", "invalid digit"),
        ];
        for (text, reason) in cases {
            let err = Quantity::parse(text, &eth()).unwrap_err();
            assert!(err.to_string().contains(reason), "{text}: {err}");
        }
        let max = format!("{} wei", U256::MAX);
        assert!(Quantity::parse(&max, &eth()).is_ok());
        let over = "115792089237316195423570985008687907853269984665640564039458 ether";
        assert!(
            Quantity::parse(over, &eth())
                .unwrap_err()
                .to_string()
                .contains("out of range")
        );
    }

    #[test]
    fn balance_shown_in_ether_with_toggles() {
        let q = Quantity::from_rpc("0x0de0b6b3a7640000").unwrap();
        let shown: Vec<String> = forms(true).iter().map(|f| q.show(*f, &eth())).collect();
        assert_eq!(
            shown,
            [
                "1 ETH",
                "1000000000000000000 wei",
                "1000000000 gwei",
                "0x0de0b6b3a7640000"
            ]
        );
        let block = Quantity::from_rpc("0x10").unwrap();
        let shown: Vec<String> = forms(false)
            .iter()
            .map(|f| block.show(*f, &eth()))
            .collect();
        assert_eq!(shown, ["16", "0x10"]);
    }

    #[test]
    fn the_native_symbol_is_a_unit() {
        let myl = Denomination {
            symbol: "MYL".into(),
            decimals: 18,
        };
        let q = Quantity::parse("2 MYL", &myl).unwrap();
        assert_eq!(q.show(Form::Native, &myl), "2 MYL");
    }

    #[test]
    fn token_amounts_keep_a_minimum_of_decimals() {
        let amount = U256::from(250_000_000u64);
        assert_eq!(format_fixed(amount, 6, 2), "250.00");
        assert_eq!(format_fixed(U256::from(250_500_001u64), 6, 2), "250.500001");
        assert_eq!(format_units(U256::ZERO, 18), "0");
        assert_eq!(format_units(U256::from(5u8), 0), "5");
    }

    #[test]
    fn native_fields() {
        for name in [
            "value",
            "balance",
            "gasPrice",
            "maxFeePerGas",
            "effectiveGasPrice",
            "fee",
        ] {
            assert!(is_native_field(name), "{name}");
        }
        assert!(!is_native_field("nonce") && !is_native_field("gasUsed"));
    }

    fn any_u256() -> impl Strategy<Value = U256> {
        any::<[u64; 4]>().prop_map(U256::from_limbs)
    }

    proptest! {
        #[test]
        fn parse_after_format_round_trips(value in any_u256(), decimals in prop::sample::select(vec![0u8, 6, 9, 18, 30, 77, 80])) {
            prop_assert_eq!(parse_units(&format_units(value, decimals), decimals), Ok(value));
            prop_assert_eq!(parse_units(&format_fixed(value, decimals, 2), decimals), Ok(value));
        }

        #[test]
        fn every_display_form_parses_back(value in any_u256()) {
            let q = Quantity::from(value);
            for form in [Form::Native, Form::Wei, Form::Gwei, Form::Decimal, Form::Hex] {
                let shown = q.show(form, &eth());
                prop_assert_eq!(Quantity::parse(&shown, &eth()).map(|q| q.value), Ok(value));
            }
        }
    }
}
