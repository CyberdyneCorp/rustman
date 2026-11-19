//! `type` changes: a request type that rejects values it accepted breaks
//! clients; a response type that allows new kinds of values does too. The
//! type of an `allOf` is what all its members allow.

use std::collections::BTreeSet;

use serde_json::Value;

use super::object::parts;
use super::{Direction, Pair, Rule};
use crate::catalog::RuleId;
use crate::difference::Sink;
use crate::spec::Node;

pub struct TypeRule;

/// The set of allowed types; `None` when the schema does not restrict it.
type Types = Option<BTreeSet<String>>;

impl Rule for TypeRule {
    fn check(&self, pair: &Pair<'_, '_>, sink: &mut Sink<'_>) {
        let old = types(&parts(pair.old, pair.old_doc));
        let new = types(&parts(pair.new, pair.new_doc));
        if old == new {
            return;
        }
        let rule = match pair.direction {
            Direction::Request if accepts_all(&new, &old) => RuleId::RequestTypeWidened,
            Direction::Request => RuleId::RequestTypeChanged,
            Direction::Response if accepts_all(&old, &new) => RuleId::ResponseTypeNarrowed,
            Direction::Response => RuleId::ResponseFieldTypeChanged,
        };
        let what = format!("type {} -> {}", show(&old), show(&new));
        pair.report(sink, rule, what);
    }
}

/// The types every part allows; `None` when no part restricts them.
fn types(parts: &[Node<'_>]) -> Types {
    parts
        .iter()
        .filter_map(|part| declared(part.value))
        .reduce(|a, b| intersect(&a, &b))
}

/// The types both sets allow (an `integer` is also a `number`).
fn intersect(a: &BTreeSet<String>, b: &BTreeSet<String>) -> BTreeSet<String> {
    let allows = |set: &BTreeSet<String>, t: &str| {
        set.contains(t) || (t == "integer" && set.contains("number"))
    };
    let one_way = a.iter().filter(|t| allows(b, t));
    let other_way = b.iter().filter(|t| allows(a, t));
    one_way.chain(other_way).cloned().collect()
}

/// The `type` keyword of one schema.
fn declared(schema: &Value) -> Types {
    match schema.get("type")? {
        Value::String(single) => Some(BTreeSet::from([single.clone()])),
        Value::Array(list) => Some(
            list.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
        ),
        _ => None,
    }
}

/// Whether every value of type `narrow` is also of type `wide`.
fn accepts_all(wide: &Types, narrow: &Types) -> bool {
    match (wide, narrow) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(wide), Some(narrow)) => narrow
            .iter()
            .all(|t| wide.contains(t) || (t == "integer" && wide.contains("number"))),
    }
}

fn show(types: &Types) -> String {
    match types {
        None => "any".to_owned(),
        Some(set) if set.len() == 1 => set.iter().next().cloned().unwrap_or_default(),
        Some(set) => format!("[{}]", set.iter().cloned().collect::<Vec<_>>().join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn set(list: &[&str]) -> Types {
        Some(list.iter().map(|s| (*s).to_owned()).collect())
    }

    #[test]
    fn number_accepts_integers() {
        assert!(accepts_all(&set(&["number"]), &set(&["integer"])));
        assert!(!accepts_all(&set(&["integer"]), &set(&["number"])));
        assert!(accepts_all(&None, &set(&["string"])));
        assert!(!accepts_all(&set(&["string"]), &None));
    }

    #[test]
    fn types_read_both_forms() {
        assert_eq!(declared(&json!({"type": "string"})), set(&["string"]));
        assert_eq!(
            declared(&json!({"type": ["string", "null"]})),
            set(&["null", "string"])
        );
        assert_eq!(declared(&json!({})), None);
        assert_eq!(show(&set(&["null", "string"])), "[null, string]");
    }

    #[test]
    fn an_all_of_allows_what_every_member_allows() {
        let doc = json!({"s": {"allOf": [
            {"type": ["number", "string"]},
            {"type": ["integer", "null"]},
            {"description": "no type"}
        ]}});
        let node = Node::at(&doc, "/s").unwrap();
        assert_eq!(types(&parts(&node, &doc)), set(&["integer"]));
        let doc = json!({"s": {"description": "no type"}});
        let node = Node::at(&doc, "/s").unwrap();
        assert_eq!(types(&parts(&node, &doc)), None);
    }
}
