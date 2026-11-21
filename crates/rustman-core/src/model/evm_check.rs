//! The `evm` assertion of `evm-call` and `evm-tx` files (evm-toolkit "EVM
//! assertions"). Only the shape is checked here; `rustman-evm` evaluates
//! it against the decoded call, so this crate needs no EVM types.

use std::collections::BTreeMap;
use std::fmt;

use serde::Deserialize;
use serde_json::Value;

use super::common::{Validate, exactly_one, given};
use super::evm::Scalar;

/// An output of the called function: its name, or its index when the
/// output is unnamed (`output: 0`).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum OutputRef {
    Index(usize),
    Name(String),
}

impl fmt::Display for OutputRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Index(index) => write!(f, "{index}"),
            Self::Name(name) => f.write_str(name),
        }
    }
}

/// `evm: {output, <op>: value}`, `evm: {emits, where}`, `evm: {reverts:
/// true}`, `evm: {reverts_with}` or `evm: {gas_lt}`.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvmAssertion {
    pub output: Option<OutputRef>,
    pub equals: Option<Value>,
    pub gt: Option<Value>,
    pub gte: Option<Value>,
    pub lt: Option<Value>,
    pub lte: Option<Value>,
    pub contains: Option<Value>,
    /// An event signature such as `Transfer(address,address,uint256)`.
    pub emits: Option<String>,
    /// With `emits`: decoded fields the event must carry.
    #[serde(rename = "where")]
    pub where_fields: Option<BTreeMap<String, Value>>,
    pub reverts: Option<bool>,
    /// A revert reason or a custom error name.
    pub reverts_with: Option<String>,
    pub gas_lt: Option<Scalar>,
}

/// The output operators, in the order they are listed in messages.
pub const EVM_OUTPUT_OPS: [&str; 6] = ["equals", "gt", "gte", "lt", "lte", "contains"];

impl EvmAssertion {
    /// The output operator and its value, when there is one.
    pub fn output_op(&self) -> Option<(&'static str, &Value)> {
        let ops = [
            &self.equals,
            &self.gt,
            &self.gte,
            &self.lt,
            &self.lte,
            &self.contains,
        ];
        EVM_OUTPUT_OPS
            .iter()
            .zip(ops)
            .find_map(|(name, value)| value.as_ref().map(|v| (*name, v)))
    }

    fn op_count(&self) -> usize {
        [
            &self.equals,
            &self.gt,
            &self.gte,
            &self.lt,
            &self.lte,
            &self.contains,
        ]
        .iter()
        .filter(|v| v.is_some())
        .count()
    }
}

impl Validate for EvmAssertion {
    fn validate(&self) -> Result<(), String> {
        exactly_one(
            "evm assertion",
            &[
                ("output", given(self.output.is_some())),
                ("emits", given(self.emits.is_some())),
                ("reverts", given(self.reverts.is_some())),
                ("reverts_with", given(self.reverts_with.is_some())),
                ("gas_lt", given(self.gas_lt.is_some())),
            ],
        )?;
        let ops = self.op_count();
        if self.output.is_some() && ops != 1 {
            let message = format!(
                "evm output assertion needs exactly one of: {}",
                EVM_OUTPUT_OPS.join(", ")
            );
            return Err(message);
        }
        if self.output.is_none() && ops > 0 {
            return Err("evm assertion: operators go with 'output'".to_owned());
        }
        if self.where_fields.is_some() && self.emits.is_none() {
            return Err("evm assertion: 'where' goes with 'emits'".to_owned());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Checked;
    use std::path::Path;

    fn load(text: &str) -> Result<Checked<EvmAssertion>, String> {
        crate::format::from_yaml(Path::new("a.yaml"), text).map_err(|e| e.message)
    }

    #[test]
    fn each_form_loads() {
        let out = load("{output: balance, gte: \"1 ether\"}").unwrap();
        assert_eq!(out.output, Some(OutputRef::Name("balance".into())));
        assert_eq!(out.output_op(), Some(("gte", &Value::from("1 ether"))));
        let index = load("{output: 0, equals: 5}").unwrap();
        assert_eq!(index.output, Some(OutputRef::Index(0)));
        let emits = load("{emits: \"Transfer(address,address,uint256)\", where: {value: 10}}");
        assert!(emits.unwrap().where_fields.is_some());
        assert_eq!(load("{reverts: true}").unwrap().reverts, Some(true));
        assert!(load("{reverts_with: InsufficientBalance}").is_ok());
        assert_eq!(
            load("{gas_lt: 50000}").unwrap().gas_lt,
            Some(Scalar("50000".into()))
        );
    }

    #[test]
    fn bad_shapes_are_rejected() {
        let err = load("{output: x}").unwrap_err();
        assert_eq!(
            err,
            "evm output assertion needs exactly one of: equals, gt, gte, lt, lte, contains"
        );
        let err = load("{reverts: true, gas_lt: 1}").unwrap_err();
        assert!(err.contains("exactly one of: output, emits"), "{err}");
        assert!(load("{emits: X(), gt: 1}").is_err());
        assert!(load("{reverts: true, where: {a: 1}}").is_err());
        assert!(load("{output: x, gt: 1, lt: 2}").is_err());
        assert!(load("{output: x, between: 1}").is_err());
    }
}
