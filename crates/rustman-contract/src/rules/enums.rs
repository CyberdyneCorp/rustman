//! `enum` changes: removed values break both directions (spec "Narrowed
//! enum"); added values are fine in a request and `unknown` in a response.

use serde_json::Value;

use super::{Direction, Pair, Rule};
use crate::catalog::RuleId;
use crate::difference::Sink;

pub struct EnumRule;

impl Rule for EnumRule {
    fn check(&self, pair: &Pair<'_, '_>, sink: &mut Sink<'_>) {
        let old = pair.old.get("enum").and_then(Value::as_array);
        let new = pair.new.get("enum").and_then(Value::as_array);
        match (old, new) {
            (None, None) => {}
            (Some(_), None) => pair.report(sink, widened(pair), "enum removed".into()),
            (None, Some(new)) => {
                let what = format!("now limited to enum {}", list(new.iter()));
                pair.report(sink, narrowed(pair), what);
            }
            (Some(old), Some(new)) => compare(pair, old, new, sink),
        }
    }
}

fn compare(pair: &Pair<'_, '_>, old: &[Value], new: &[Value], sink: &mut Sink<'_>) {
    let removed: Vec<&Value> = old.iter().filter(|v| !new.contains(v)).collect();
    let added: Vec<&Value> = new.iter().filter(|v| !old.contains(v)).collect();
    if !removed.is_empty() {
        let what = format!("enum values removed: {}", list(removed.into_iter()));
        pair.report(sink, narrowed(pair), what);
    }
    if !added.is_empty() {
        let what = format!("enum values added: {}", list(added.into_iter()));
        pair.report(sink, widened(pair), what);
    }
}

fn narrowed(pair: &Pair<'_, '_>) -> RuleId {
    match pair.direction {
        Direction::Request => RuleId::RequestEnumNarrowed,
        Direction::Response => RuleId::ResponseEnumNarrowed,
    }
}

fn widened(pair: &Pair<'_, '_>) -> RuleId {
    match pair.direction {
        Direction::Request => RuleId::RequestEnumWidened,
        Direction::Response => RuleId::ResponseEnumWidened,
    }
}

fn list<'v>(values: impl Iterator<Item = &'v Value>) -> String {
    let shown: Vec<String> = values
        .map(|v| match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .collect();
    format!("[{}]", shown.join(", "))
}
