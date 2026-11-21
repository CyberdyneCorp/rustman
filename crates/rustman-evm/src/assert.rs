//! `evm` assertions (evm-toolkit "EVM assertions"): output checks with
//! unit-aware values, `emits`, `reverts`, `reverts_with` and `gas_lt`,
//! evaluated against what a call returned. Integer arithmetic only.

use alloy_dyn_abi::DynSolValue;
use alloy_primitives::{I256, U256};
use rustman_core::assertions::{AssertionResult, Outcome};
use rustman_core::model::{EvmAssertion, OutputRef};
use serde_json::Value;

use crate::call::{output_key, parse_address};
use crate::codec::{DecodedLog, NamedValue, Revert, format_value};
use crate::quantity::{Denomination, Form, Quantity};
use crate::tx::TokenMeta;

/// What a call produced, as the assertions see it.
#[derive(Clone, Copy, Debug)]
pub struct EvmFacts<'a> {
    /// The decoded outputs of a call that returned.
    pub outputs: Option<&'a [NamedValue]>,
    pub revert: Option<&'a Revert>,
    /// Decoded events, reorged ones left out.
    pub events: &'a [DecodedLog],
    /// Gas used (a receipt) or the gas estimate (a call).
    pub gas: Option<U256>,
    /// The chain's native currency, for unit forms.
    pub native: &'a Denomination,
    /// The token of an ERC-20 amount output, whose symbol is a unit too
    /// (`250 USDC`).
    pub token: Option<&'a TokenMeta>,
}

/// Evaluate one `evm` assertion.
pub fn evaluate(assertion: &EvmAssertion, facts: &EvmFacts<'_>) -> AssertionResult {
    if let Some(output) = &assertion.output {
        return output_check(output, assertion, facts);
    }
    if let Some(event) = &assertion.emits {
        return emits(event, assertion, facts);
    }
    if let Some(expected) = assertion.reverts {
        return reverts(expected, facts);
    }
    if let Some(expected) = &assertion.reverts_with {
        return reverts_with(expected, facts);
    }
    match &assertion.gas_lt {
        Some(limit) => gas_lt(limit.as_str(), facts),
        None => result("evm", Outcome::Error, "evm assertion is empty".to_owned()),
    }
}

/// Evaluate every `evm` assertion of `assertions` in order; others are
/// left out.
pub fn evaluate_all(
    assertions: &[rustman_core::model::Assertion],
    facts: &EvmFacts<'_>,
) -> Vec<AssertionResult> {
    assertions
        .iter()
        .filter_map(|a| match a {
            rustman_core::model::Assertion::Evm(evm) => Some(evaluate(evm, facts)),
            _ => None,
        })
        .collect()
}

fn result(label: impl Into<String>, outcome: Outcome, message: String) -> AssertionResult {
    AssertionResult {
        kind: "evm",
        outcome,
        label: label.into(),
        expected: None,
        actual: None,
        message,
        snapshot: None,
    }
}

fn pass(label: String) -> AssertionResult {
    result(label, Outcome::Pass, String::new())
}

fn fail(label: String, prefix: &str, expected: String, actual: String) -> AssertionResult {
    let message = format!("{prefix}: expected {expected}, got {actual}");
    AssertionResult {
        expected: Some(expected),
        actual: Some(actual),
        ..result(label, Outcome::Fail, message)
    }
}

fn error(label: String, message: String) -> AssertionResult {
    result(label, Outcome::Error, message)
}

/// The text a JSON expected value stands for.
fn expected_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn output_check(output: &OutputRef, a: &EvmAssertion, facts: &EvmFacts<'_>) -> AssertionResult {
    let Some((op, raw)) = a.output_op() else {
        return error(format!("evm output '{output}'"), "no operator".to_owned());
    };
    let expected = expected_text(raw);
    let label = format!("evm output '{output}' {op} {expected}");
    let prefix = format!("evm output '{output}'");
    let Some(outputs) = facts.outputs else {
        let why = match facts.revert {
            Some(revert) => format!("{prefix}: the call {revert}"),
            None => format!("{prefix}: the call returned nothing decodable"),
        };
        return error(label, why);
    };
    let Some(value) = find_output(outputs, output) else {
        let names: Vec<String> = outputs
            .iter()
            .enumerate()
            .map(|(i, v)| output_key(v, i))
            .collect();
        let message = format!("{prefix}: no such output (outputs: {})", names.join(", "));
        return error(label, message);
    };
    let check = OpCheck {
        op,
        expected: &expected,
        raw,
        native: facts.native,
        token_output: facts.token.is_some(),
        token: token_denomination(facts.token),
    };
    match check.run(&value.value) {
        Ok(Verdict { ok: true, .. }) => pass(label),
        Ok(Verdict {
            ok: false,
            expected,
            actual,
        }) => fail(label, &prefix, expected, actual),
        Err(message) => error(label, format!("{prefix}: {message}")),
    }
}

fn find_output<'v>(outputs: &'v [NamedValue], output: &OutputRef) -> Option<&'v NamedValue> {
    match output {
        OutputRef::Index(index) => outputs.get(*index),
        OutputRef::Name(name) => outputs.iter().find(|v| &v.name == name).or_else(|| {
            let index: usize = name.parse().ok()?;
            outputs.get(index)
        }),
    }
}

/// How one comparison came out, with both sides as shown.
struct Verdict {
    ok: bool,
    expected: String,
    actual: String,
}

/// One output operator and its expected value.
struct OpCheck<'a> {
    op: &'static str,
    expected: &'a str,
    raw: &'a Value,
    native: &'a Denomination,
    /// Whether the output is a token amount (its denomination may still
    /// be unknown).
    token_output: bool,
    /// The token's denomination, when the output is a token amount.
    token: Option<Denomination>,
}

impl OpCheck<'_> {
    fn run(&self, value: &DynSolValue) -> Result<Verdict, String> {
        match value {
            DynSolValue::Uint(actual, _) => self.unsigned(*actual),
            DynSolValue::Int(actual, _) => self.signed(*actual),
            _ if self.is_ordering() => Err(format!(
                "'{}' needs an integer output, got {}",
                self.op,
                format_value(value)
            )),
            _ if self.op == "contains" => self.contains(value),
            _ => Ok(self.verdict(self.same(value), self.expected.to_owned(), value)),
        }
    }

    fn contains(&self, value: &DynSolValue) -> Result<Verdict, String> {
        let ok = match value {
            DynSolValue::Array(items) | DynSolValue::FixedArray(items) => {
                items.iter().any(|item| self.same(item))
            }
            DynSolValue::String(text) => text.contains(self.expected),
            DynSolValue::Bytes(_) | DynSolValue::FixedBytes(..) => {
                let hex = format_value(value).to_ascii_lowercase();
                let needle = self.expected.trim_start_matches("0x").to_ascii_lowercase();
                hex.contains(&needle)
            }
            _ => {
                return Err(format!(
                    "'contains' needs a string, bytes or array output, got {}",
                    format_value(value)
                ));
            }
        };
        Ok(self.verdict(ok, format!("to contain {}", self.expected), value))
    }

    fn is_ordering(&self) -> bool {
        matches!(self.op, "gt" | "gte" | "lt" | "lte")
    }

    fn verdict(&self, ok: bool, expected: String, actual: &DynSolValue) -> Verdict {
        Verdict {
            ok,
            expected,
            actual: format_value(actual),
        }
    }

    /// Whether `value` equals the expected value, by its type.
    fn same(&self, value: &DynSolValue) -> bool {
        let expected = self.expected.trim();
        match value {
            DynSolValue::Address(actual) => parse_address(expected).is_ok_and(|a| a == *actual),
            DynSolValue::Bool(actual) => {
                self.raw.as_bool() == Some(*actual)
                    || expected.eq_ignore_ascii_case(&actual.to_string())
            }
            DynSolValue::Uint(actual, _) => {
                Quantity::parse(expected, self.native).is_ok_and(|q| q.value == *actual)
            }
            DynSolValue::Int(actual, _) => parse_signed(expected).is_ok_and(|e| e == *actual),
            DynSolValue::String(actual) => actual == expected,
            DynSolValue::Bytes(_) | DynSolValue::FixedBytes(..) => {
                format_value(value).eq_ignore_ascii_case(expected)
            }
            other => format_value(other) == expected,
        }
    }

    /// The denomination the expected value's unit names: the token's
    /// symbol, else the native currency's units.
    fn denomination(&self) -> &Denomination {
        let unit = self.expected.split_whitespace().nth(1).unwrap_or_default();
        match &self.token {
            Some(token) if token.symbol.eq_ignore_ascii_case(unit) => token,
            _ => self.native,
        }
    }

    fn unsigned(&self, actual: U256) -> Result<Verdict, String> {
        let denomination = self.denomination();
        let expected = Quantity::parse(self.expected, denomination).map_err(|e| e.to_string())?;
        if self.op == "contains" {
            return Err("'contains' needs a string, bytes or array output".to_owned());
        }
        let ok = compare(self.op, actual.cmp(&expected.value));
        let (expected, actual) = self.shown(expected.value, actual, denomination);
        Ok(Verdict {
            ok,
            expected: format!("{}{expected}", symbol(self.op)),
            actual,
        })
    }

    /// Both sides of an integer comparison as a failure shows them: in
    /// the output's own units (a token amount in the token, else the
    /// native currency) when the expected value has a unit, else as plain
    /// integers. A token amount of unknown decimals shows the raw
    /// integers, with the unit as given.
    fn shown(&self, expected: U256, actual: U256, parsed_in: &Denomination) -> (String, String) {
        let plain = |v: U256| Quantity::from(v).show(Form::Decimal, parsed_in);
        if !has_unit(self.expected) {
            return (plain(expected), plain(actual));
        }
        let units = match (&self.token, self.token_output) {
            (Some(token), _) => token,
            (None, false) => parsed_in,
            (None, true) => {
                let given = self.expected.trim();
                return (format!("{} ({given})", plain(expected)), plain(actual));
            }
        };
        let show = |v: U256| Quantity::from(v).show(Form::Native, units);
        (show(expected), show(actual))
    }

    fn signed(&self, actual: I256) -> Result<Verdict, String> {
        let expected = parse_signed(self.expected)?;
        if self.op == "contains" {
            return Err("'contains' needs a string, bytes or array output".to_owned());
        }
        Ok(Verdict {
            ok: compare(self.op, actual.cmp(&expected)),
            expected: format!("{}{expected}", symbol(self.op)),
            actual: actual.to_string(),
        })
    }
}

fn token_denomination(token: Option<&TokenMeta>) -> Option<Denomination> {
    let token = token?;
    Some(Denomination {
        symbol: token.symbol.clone()?,
        decimals: token.decimals?,
    })
}

fn parse_signed(text: &str) -> Result<I256, String> {
    I256::from_dec_str(text.trim()).map_err(|_| format!("'{text}' is not a signed integer"))
}

/// Whether `text` names a unit (`1 ether`, `30 gwei`).
fn has_unit(text: &str) -> bool {
    text.split_whitespace().nth(1).is_some()
}

fn compare(op: &str, ordering: std::cmp::Ordering) -> bool {
    use std::cmp::Ordering::{Equal, Greater, Less};
    match op {
        "gt" => ordering == Greater,
        "gte" => ordering != Less,
        "lt" => ordering == Less,
        "lte" => ordering != Greater,
        _ => ordering == Equal,
    }
}

/// How an operator reads in a failure: `>= 1 ETH`.
fn symbol(op: &str) -> &'static str {
    match op {
        "gt" => "> ",
        "gte" => ">= ",
        "lt" => "< ",
        "lte" => "<= ",
        _ => "",
    }
}

fn emits(event: &str, a: &EvmAssertion, facts: &EvmFacts<'_>) -> AssertionResult {
    let wanted = event.replace(' ', "");
    let label = format!("evm emits {wanted}");
    let fields = a.where_fields.clone().unwrap_or_default();
    let matching: Vec<&DecodedLog> = facts
        .events
        .iter()
        .filter(|log| log.event.signature() == wanted)
        .collect();
    let found = matching.iter().any(|log| {
        fields.iter().all(|(name, raw)| {
            let check = OpCheck {
                op: "equals",
                expected: &expected_text(raw),
                raw,
                native: facts.native,
                token_output: false,
                token: None,
            };
            log.field(name).is_some_and(|f| check.same(&f.value))
        })
    });
    if found {
        return pass(label);
    }
    let expected = match fields.is_empty() {
        true => wanted.clone(),
        false => {
            let shown: Vec<String> = fields
                .iter()
                .map(|(k, v)| format!("{k}={}", expected_text(v)))
                .collect();
            format!("{wanted} where {}", shown.join(", "))
        }
    };
    let actual = match facts.events.is_empty() {
        true => "no events".to_owned(),
        false => {
            let shown: Vec<String> = facts.events.iter().map(ToString::to_string).collect();
            shown.join("; ")
        }
    };
    fail(label, "evm emits", expected, actual)
}

fn reverts(expected: bool, facts: &EvmFacts<'_>) -> AssertionResult {
    let label = format!("evm reverts {expected}");
    let actual = match facts.revert {
        Some(revert) => revert.to_string(),
        None => "no revert".to_owned(),
    };
    let wanted = if expected { "a revert" } else { "no revert" };
    if facts.revert.is_some() == expected {
        pass(label)
    } else {
        fail(label, "evm reverts", wanted.to_owned(), actual)
    }
}

fn reverts_with(expected: &str, facts: &EvmFacts<'_>) -> AssertionResult {
    let label = format!("evm reverts_with {expected}");
    let Some(revert) = facts.revert else {
        return fail(
            label,
            "evm reverts_with",
            format!("a revert with {expected}"),
            "no revert".to_owned(),
        );
    };
    let shown = revert.to_string();
    let detail = shown.strip_prefix("reverted: ").unwrap_or(&shown);
    let ok = revert.name() == Some(expected) || detail.starts_with(expected);
    if ok {
        pass(label)
    } else {
        fail(label, "evm reverts_with", expected.to_owned(), shown)
    }
}

fn gas_lt(limit: &str, facts: &EvmFacts<'_>) -> AssertionResult {
    let label = format!("evm gas_lt {limit}");
    let limit = match Quantity::parse(limit, &Denomination::default()) {
        Ok(limit) => limit.value,
        Err(e) => return error(label, format!("evm gas_lt: {e}")),
    };
    let Some(gas) = facts.gas else {
        let message = "evm gas_lt: no gas figure (the estimate failed or the call reverted)";
        return error(label, message.to_owned());
    };
    if gas < limit {
        pass(label)
    } else {
        fail(label, "evm gas_lt", format!("< {limit}"), gas.to_string())
    }
}

#[cfg(test)]
mod tests;
