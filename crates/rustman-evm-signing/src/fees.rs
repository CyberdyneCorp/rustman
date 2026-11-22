//! Fees and the gas limit (evm-signing "Transaction types and fees",
//! design D9).
//!
//! EIP-1559 (the default): `max_priority_fee` is the median of the 50th
//! percentile rewards of the last 10 blocks (`eth_feeHistory(10, "latest",
//! [50])`), `max_fee` is 2 × the latest base fee + the priority fee.
//! Legacy (`type: legacy`): `gas_price` from `eth_gasPrice`. A value set in
//! the request wins and is not labelled `suggested`. The gas limit is the
//! simulation's estimate × 1.2 unless `gas` is set.

use alloy_primitives::U256;
use rustman_core::model::TxType;
use rustman_evm::EvmError;
use rustman_evm::quantity::{Denomination, GWEI_DECIMALS, Quantity, format_fixed, parse_hex};
use rustman_evm::rpc::{Rpc, call};
use serde_json::{Value, json};

/// The blocks `eth_feeHistory` looks back over.
pub const FEE_HISTORY_BLOCKS: u64 = 10;

/// The highest fee per gas the node may suggest (10 000 gwei). A node
/// cannot raise the fee past it: above it, the request must set the fee
/// itself (the confirmation is not read under `--yes-local`).
pub const MAX_SUGGESTED_FEE_GWEI: u64 = 10_000;

/// The highest gas limit derived from the node's estimate (30 million, a
/// mainnet block); above it, the request must set `gas`.
pub const MAX_SUGGESTED_GAS: u64 = 30_000_000;

/// One fee: its value and whether the node suggested it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fee {
    pub wei: U256,
    pub suggested: bool,
}

impl Fee {
    fn set(wei: U256) -> Self {
        Self {
            wei,
            suggested: false,
        }
    }

    fn suggested(wei: U256) -> Self {
        Self {
            wei,
            suggested: true,
        }
    }

    /// A suggested fee above [`MAX_SUGGESTED_FEE_GWEI`] is refused.
    fn capped(self, field: &str) -> Result<Self, EvmError> {
        let ceiling = U256::from(MAX_SUGGESTED_FEE_GWEI) * U256::from(1_000_000_000_u64);
        match self.suggested && self.wei > ceiling {
            true => Err(EvmError::Refused(format!(
                "the node suggests {field} = {}, above the {MAX_SUGGESTED_FEE_GWEI} gwei ceiling; set {field} in the request to sign anyway",
                self.show()
            ))),
            false => Ok(self),
        }
    }

    /// `31.5 gwei`, or `31.5 gwei (suggested)`.
    pub fn show(&self) -> String {
        let gwei = format!("{} gwei", format_fixed(self.wei, GWEI_DECIMALS, 0));
        match self.suggested {
            true => format!("{gwei} (suggested)"),
            false => gwei,
        }
    }
}

/// The fees of a transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fees {
    Eip1559 { max_fee: Fee, max_priority_fee: Fee },
    Legacy { gas_price: Fee },
}

impl Fees {
    /// The most one unit of gas can cost.
    pub fn max_per_gas(&self) -> U256 {
        match self {
            Self::Eip1559 { max_fee, .. } => max_fee.wei,
            Self::Legacy { gas_price } => gas_price.wei,
        }
    }
}

/// The fee fields of a request, resolved but not parsed.
#[derive(Clone, Copy, Debug, Default)]
pub struct FeeInputs<'a> {
    pub tx_type: TxType,
    pub max_fee: Option<&'a str>,
    pub max_priority_fee: Option<&'a str>,
    pub gas_price: Option<&'a str>,
}

/// The fees to sign with: the request's own, the rest suggested by the
/// node. Only the calls a missing value needs are made.
pub async fn fees(
    rpc: &dyn Rpc,
    inputs: &FeeInputs<'_>,
    native: &Denomination,
) -> Result<Fees, EvmError> {
    check_fields(inputs)?;
    let parse = |field: &str, text: Option<&str>| -> Result<Option<U256>, EvmError> {
        text.map(|t| quantity(field, t, native)).transpose()
    };
    if inputs.tx_type == TxType::Legacy {
        let gas_price = match parse("gas_price", inputs.gas_price)? {
            Some(wei) => Fee::set(wei),
            None => Fee::suggested(gas_price(rpc).await?),
        };
        let gas_price = gas_price.capped("gas_price")?;
        return Ok(Fees::Legacy { gas_price });
    }
    let max_fee = parse("max_fee", inputs.max_fee)?;
    let priority = parse("max_priority_fee", inputs.max_priority_fee)?;
    let (max_fee, max_priority_fee) = match (max_fee, priority) {
        (Some(max_fee), Some(priority)) => (Fee::set(max_fee), Fee::set(priority)),
        (max_fee, priority) => {
            let suggestion = suggest(&fee_history(rpc).await?)?;
            let priority = priority.map_or(Fee::suggested(suggestion.priority), Fee::set);
            let max_fee = max_fee.map_or_else(
                || Fee::suggested(suggestion.max_fee(priority.wei)),
                Fee::set,
            );
            (max_fee, priority)
        }
    };
    let max_fee = max_fee.capped("max_fee")?;
    let max_priority_fee = max_priority_fee.capped("max_priority_fee")?;
    if max_priority_fee.wei > max_fee.wei {
        return Err(EvmError::config(
            "max_priority_fee is above max_fee; lower it or raise max_fee",
        ));
    }
    Ok(Fees::Eip1559 {
        max_fee,
        max_priority_fee,
    })
}

/// A legacy transaction has a gas price only; an EIP-1559 one has none.
fn check_fields(inputs: &FeeInputs<'_>) -> Result<(), EvmError> {
    match inputs.tx_type {
        TxType::Legacy if inputs.max_fee.is_some() || inputs.max_priority_fee.is_some() => {
            Err(EvmError::config(
                "a legacy transaction takes gas_price, not max_fee or max_priority_fee",
            ))
        }
        TxType::Eip1559 if inputs.gas_price.is_some() => Err(EvmError::config(
            "gas_price is for type: legacy; an EIP-1559 transaction takes max_fee and max_priority_fee",
        )),
        _ => Ok(()),
    }
}

fn quantity(field: &str, text: &str, native: &Denomination) -> Result<U256, EvmError> {
    Quantity::parse(text, native)
        .map(|q| q.value)
        .map_err(|e| EvmError::config(format!("{field}: {e}")))
}

async fn gas_price(rpc: &dyn Rpc) -> Result<U256, EvmError> {
    let answer = call(rpc, "eth_gasPrice", json!([])).await?;
    hex_of(&answer, "eth_gasPrice")
}

async fn fee_history(rpc: &dyn Rpc) -> Result<Value, EvmError> {
    let blocks = format!("{FEE_HISTORY_BLOCKS:#x}");
    call(rpc, "eth_feeHistory", json!([blocks, "latest", [50]])).await
}

fn hex_of(value: &Value, what: &str) -> Result<U256, EvmError> {
    value
        .as_str()
        .and_then(|text| parse_hex(text).ok())
        .ok_or_else(|| EvmError::Rpc(format!("{what}: not a quantity")))
}

/// What `eth_feeHistory` suggests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Suggestion {
    /// The latest base fee: the one the next block will charge.
    pub base_fee: U256,
    /// The median of the 50th percentile rewards.
    pub priority: U256,
}

impl Suggestion {
    /// 2 × base fee + `priority`.
    pub fn max_fee(&self, priority: U256) -> U256 {
        self.base_fee
            .saturating_mul(U256::from(2))
            .saturating_add(priority)
    }
}

/// The suggestion of an `eth_feeHistory` answer.
pub fn suggest(history: &Value) -> Result<Suggestion, EvmError> {
    let no_1559 = || {
        EvmError::Rpc(
            "eth_feeHistory: the node reports no base fee; use type: legacy on a chain without EIP-1559"
                .to_owned(),
        )
    };
    let base_fee = history["baseFeePerGas"]
        .as_array()
        .and_then(|fees| fees.last())
        .ok_or_else(no_1559)
        .and_then(|fee| hex_of(fee, "eth_feeHistory baseFeePerGas"))?;
    let mut rewards = history["reward"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|block| block.get(0))
        .map(|reward| hex_of(reward, "eth_feeHistory reward"))
        .collect::<Result<Vec<_>, _>>()?;
    rewards.sort_unstable();
    Ok(Suggestion {
        base_fee,
        priority: median(&rewards),
    })
}

/// The median of sorted `values` (the mean of the two middle ones for an
/// even count); zero for none.
fn median(values: &[U256]) -> U256 {
    let n = values.len();
    match n {
        0 => U256::ZERO,
        _ if n % 2 == 1 => values[n / 2],
        _ => (values[n / 2 - 1] + values[n / 2]) / U256::from(2),
    }
}

/// The gas limit: `gas` from the request, else the estimate × 1.2
/// (rounded up).
pub fn gas_limit(
    gas: Option<&str>,
    estimate: Option<U256>,
    native: &Denomination,
) -> Result<U256, EvmError> {
    if let Some(text) = gas {
        return quantity("gas", text, native);
    }
    let estimate = estimate.ok_or_else(|| {
        EvmError::config("no gas estimate (the simulation reverted); set 'gas' in the request")
    })?;
    let limit = estimate
        .saturating_mul(U256::from(6))
        .div_ceil(U256::from(5));
    match limit > U256::from(MAX_SUGGESTED_GAS) {
        true => Err(EvmError::Refused(format!(
            "the node's gas estimate gives a gas limit of {limit}, above {MAX_SUGGESTED_GAS}; set 'gas' in the request to sign anyway"
        ))),
        false => Ok(limit),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_median_of_rewards() {
        let u = |v: &[u64]| v.iter().map(|x| U256::from(*x)).collect::<Vec<_>>();
        assert_eq!(median(&[]), U256::ZERO);
        assert_eq!(median(&u(&[1, 5, 9])), U256::from(5));
        assert_eq!(median(&u(&[1, 3, 5, 9])), U256::from(4));
    }

    #[test]
    fn the_gas_limit_is_the_estimate_plus_a_fifth_rounded_up() {
        let native = Denomination::default();
        let limit = |estimate: u64| gas_limit(None, Some(U256::from(estimate)), &native).unwrap();
        assert_eq!(limit(21_000), U256::from(25_200));
        assert_eq!(limit(46_097), U256::from(55_317));
        assert_eq!(
            gas_limit(Some("100000"), Some(U256::from(1)), &native).unwrap(),
            U256::from(100_000)
        );
        let err = gas_limit(None, None, &native).unwrap_err();
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn a_node_cannot_inflate_the_gas_limit_or_the_fees() {
        // Regression: the node's estimate and fees had no ceiling, and
        // nobody reads the confirmation under --yes-local.
        let native = Denomination::default();
        let err = gas_limit(None, Some(U256::from(25_000_001)), &native).unwrap_err();
        assert_eq!(err.exit_code(), 4);
        assert_eq!(
            err.to_string(),
            "the node's gas estimate gives a gas limit of 30000002, above 30000000; set 'gas' in the request to sign anyway"
        );
        assert!(gas_limit(None, Some(U256::from(25_000_000)), &native).is_ok());
        // A value the request sets is the user's own.
        assert!(gas_limit(Some("40000000"), None, &native).is_ok());

        let gwei = |n: u64| U256::from(n) * U256::from(1_000_000_000_u64);
        let err = Fee::suggested(gwei(10_001)).capped("max_fee").unwrap_err();
        assert_eq!(err.exit_code(), 4);
        assert_eq!(
            err.to_string(),
            "the node suggests max_fee = 10001 gwei (suggested), above the 10000 gwei ceiling; set max_fee in the request to sign anyway"
        );
        assert!(Fee::suggested(gwei(10_000)).capped("max_fee").is_ok());
        assert!(Fee::set(gwei(20_000)).capped("max_fee").is_ok());
    }

    #[test]
    fn fees_show_in_gwei_and_say_when_they_are_suggested() {
        let fee = Fee::suggested(U256::from(31_500_000_000_u64));
        assert_eq!(fee.show(), "31.5 gwei (suggested)");
        assert_eq!(Fee::set(U256::from(2_000_000_000_u64)).show(), "2 gwei");
    }
}
