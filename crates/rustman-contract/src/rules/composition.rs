//! `oneOf`, `anyOf`, `allOf` and `not`: whether a changed composition is
//! compatible is not decided (design non-goal), so any change is `unknown`.
//! Alternatives are compared inlined and without documentation, so moving a
//! schema into `components` or rewording it is not a change.
//!
//! An `allOf` is flattened into one object (see [`super::object`]): its
//! properties, `required` lists and types get the usual rules, and only
//! what its members say besides that is compared as a composition.

use serde_json::Value;

use super::object::{parts, residual};
use super::{Pair, Rule};
use crate::catalog::RuleId;
use crate::difference::Sink;
use crate::spec::inline;

pub struct CompositionRule;

const KEYWORDS: [&str; 3] = ["oneOf", "anyOf", "not"];

impl Rule for CompositionRule {
    fn check(&self, pair: &Pair<'_, '_>, sink: &mut Sink<'_>) {
        for keyword in KEYWORDS {
            let old = pair.old.get(keyword).map(|v| inline(pair.old_doc, v, true));
            let new = pair.new.get(keyword).map(|v| inline(pair.new_doc, v, true));
            if old != new {
                let what = changed(keyword, count(old.as_ref()), count(new.as_ref()));
                pair.report(sink, RuleId::CompositionChanged, what);
            }
        }
        let old = residual(&parts(pair.old, pair.old_doc), pair.old_doc);
        let new = residual(&parts(pair.new, pair.new_doc), pair.new_doc);
        if old != new {
            let (from, to) = (pair.old.get("allOf"), pair.new.get("allOf"));
            let what = changed("allOf", count(from), count(to));
            pair.report(sink, RuleId::CompositionChanged, what);
        }
    }
}

/// `oneOf changed (2 schemas)`, or `allOf 1 schema -> 2 schemas`.
fn changed(keyword: &str, from: String, to: String) -> String {
    if from == to {
        format!("{keyword} changed ({to})")
    } else {
        format!("{keyword} {from} -> {to}")
    }
}

/// `none`, `1 schema` or `3 schemas`.
fn count(value: Option<&Value>) -> String {
    match value {
        None => "none".to_owned(),
        Some(Value::Array(list)) if list.len() != 1 => format!("{} schemas", list.len()),
        Some(_) => "1 schema".to_owned(),
    }
}
