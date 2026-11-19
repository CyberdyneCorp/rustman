//! Object members: fields added, removed, made required or optional. A
//! renamed field is a removal and an addition. The members of an `allOf`
//! add up to one object.

use std::collections::BTreeSet;

use serde_json::Value;

use super::object::{parts, property};
use super::{Direction, Pair, Rule};
use crate::catalog::RuleId;
use crate::difference::Sink;
use crate::spec::Node;

pub struct PropertiesRule;

/// The members of one object schema.
struct Members<'a> {
    properties: BTreeSet<&'a str>,
    required: BTreeSet<&'a str>,
    closed: bool,
}

impl<'a> Members<'a> {
    /// The members of every part of an object (see [`parts`]).
    fn of(parts: &[Node<'a>]) -> Members<'a> {
        let mut members = Members {
            properties: BTreeSet::new(),
            required: BTreeSet::new(),
            closed: false,
        };
        for part in parts {
            if let Some(map) = part.get("properties").and_then(Value::as_object) {
                members.properties.extend(map.keys().map(String::as_str));
            }
            if let Some(list) = part.get("required").and_then(Value::as_array) {
                members
                    .required
                    .extend(list.iter().filter_map(Value::as_str));
            }
            members.closed |= part.get("additionalProperties") == Some(&Value::Bool(false));
        }
        members
    }

    fn has(&self, name: &str) -> bool {
        self.properties.contains(name) || self.required.contains(name)
    }
}

impl Rule for PropertiesRule {
    fn check(&self, pair: &Pair<'_, '_>, sink: &mut Sink<'_>) {
        let old_parts = parts(pair.old, pair.old_doc);
        let new_parts = parts(pair.new, pair.new_doc);
        let old = Members::of(&old_parts);
        let new = Members::of(&new_parts);
        let names: BTreeSet<&str> = [
            &old.properties,
            &old.required,
            &new.properties,
            &new.required,
        ]
        .into_iter()
        .flatten()
        .copied()
        .collect();
        for name in names {
            let change = match (old.has(name), new.has(name)) {
                (true, false) => Some(removed(pair.direction, &new)),
                (false, true) => Some(added(pair.direction, &old, new.required.contains(name))),
                _ => requirement(pair.direction, &old, &new, name),
            };
            if let Some((rule, what)) = change {
                let pointers = (
                    member_pointer(&old_parts, name),
                    member_pointer(&new_parts, name),
                );
                report(pair, sink, (rule, what), name, pointers);
            }
        }
    }
}

type Change = (RuleId, &'static str);

/// A removed field. Old clients may still send it, which a schema that
/// allows no other properties now rejects.
fn removed(direction: Direction, new: &Members<'_>) -> Change {
    match direction {
        Direction::Request if new.closed => {
            (RuleId::RequestFieldRemovedFromClosedSchema, "removed")
        }
        Direction::Request => (RuleId::RequestFieldRemoved, "removed"),
        Direction::Response => (RuleId::ResponseFieldRemoved, "removed"),
    }
}

fn added(direction: Direction, old: &Members<'_>, required: bool) -> Change {
    match direction {
        Direction::Request if required => (RuleId::RequestRequiredFieldAdded, "added, required"),
        Direction::Request => (RuleId::RequestOptionalFieldAdded, "added, optional"),
        Direction::Response if old.closed => (RuleId::ResponseFieldAddedToClosedSchema, "added"),
        Direction::Response => (RuleId::ResponseFieldAdded, "added"),
    }
}

fn requirement(
    direction: Direction,
    old: &Members<'_>,
    new: &Members<'_>,
    name: &str,
) -> Option<Change> {
    let (was, is) = (old.required.contains(name), new.required.contains(name));
    match (direction, was, is) {
        (Direction::Request, false, true) => {
            Some((RuleId::RequestRequiredFieldAdded, "now required"))
        }
        (Direction::Request, true, false) => {
            Some((RuleId::RequestFieldBecameOptional, "no longer required"))
        }
        (Direction::Response, false, true) => {
            Some((RuleId::ResponseFieldBecameRequired, "now required"))
        }
        (Direction::Response, true, false) => {
            Some((RuleId::ResponseFieldBecameOptional, "no longer required"))
        }
        _ => None,
    }
}

/// Report at the member's pointer in each document.
fn report(
    pair: &Pair<'_, '_>,
    sink: &mut Sink<'_>,
    (rule, what): Change,
    name: &str,
    (old, new): (String, String),
) {
    let message = format!("{}: {what}", pair.place(Some(name)));
    sink.push(rule, message, &old, &new);
}

/// Where the member is declared, or else the first `properties` of the
/// object, or else the object itself.
fn member_pointer(parts: &[Node<'_>], name: &str) -> String {
    if let Some(member) = property(parts, name) {
        return member.pointer;
    }
    parts
        .iter()
        .find_map(|part| part.child("properties"))
        .or_else(|| parts.first().cloned())
        .map(|node| node.pointer)
        .unwrap_or_default()
}
