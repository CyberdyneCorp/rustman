//! The direction-aware schema walker (design D2): applies the schema rules
//! to a pair of schemas, then to the pairs of their common members, array
//! items and additional properties (the members of an `allOf` count as the
//! object's). `$ref`s are followed on both sides, and
//! a pair already being compared further up the same branch (a recursive
//! schema) is not entered again.

use serde_json::Value;

use crate::difference::Sink;
use crate::rules::{Direction, Pair, SCHEMA_RULES, parts, property};
use crate::spec::Node;

/// The deepest schema nesting compared.
const MAX_DEPTH: usize = 64;

/// Compare the schemas `old` and `new` of one element (`context`, such as
/// `request body (application/json)`).
pub fn compare_schemas<'a>(
    direction: Direction,
    context: &str,
    (old, new): (Node<'a>, Node<'a>),
    (old_doc, new_doc): (&'a Value, &'a Value),
    sink: &mut Sink<'_>,
) {
    let mut walker = Walker {
        direction,
        context,
        old_doc,
        new_doc,
        stack: Vec::new(),
        sink,
    };
    walker.node("$", old, new);
}

struct Walker<'w, 'a, 's> {
    direction: Direction,
    context: &'w str,
    old_doc: &'a Value,
    new_doc: &'a Value,
    /// The (old, new) pointer pairs being compared on the current branch.
    stack: Vec<(String, String)>,
    sink: &'w mut Sink<'s>,
}

impl<'a> Walker<'_, 'a, '_> {
    fn node(&mut self, path: &str, old: Node<'a>, new: Node<'a>) {
        let old = old.resolved(self.old_doc);
        let new = new.resolved(self.new_doc);
        let key = (old.pointer.clone(), new.pointer.clone());
        if self.stack.contains(&key) || self.stack.len() >= MAX_DEPTH {
            return;
        }
        let pair = Pair {
            direction: self.direction,
            context: self.context,
            path,
            old: &old,
            new: &new,
            old_doc: self.old_doc,
            new_doc: self.new_doc,
        };
        for rule in SCHEMA_RULES {
            rule.check(&pair, self.sink);
        }
        self.stack.push(key);
        self.members(path, &old, &new);
        self.nested(path, &old, &new, "items", "[]");
        self.nested(path, &old, &new, "additionalProperties", ".*");
        self.stack.pop();
    }

    /// The properties declared on both sides, by name, in any part of the
    /// object.
    fn members(&mut self, path: &str, old: &Node<'a>, new: &Node<'a>) {
        let old_parts = parts(old, self.old_doc);
        let new_parts = parts(new, self.new_doc);
        let mut names: Vec<&str> = old_parts
            .iter()
            .filter_map(|part| part.get("properties").and_then(Value::as_object))
            .flat_map(|map| map.keys().map(String::as_str))
            .collect();
        names.sort_unstable();
        names.dedup();
        for name in names {
            if let (Some(o), Some(n)) = (property(&old_parts, name), property(&new_parts, name)) {
                self.node(&format!("{path}.{name}"), o, n);
            }
        }
    }

    /// A schema-valued keyword present as a schema on both sides. When it
    /// is added, removed or a boolean on either side, `SubschemaRule`
    /// reports the change instead.
    fn nested(&mut self, path: &str, old: &Node<'a>, new: &Node<'a>, key: &str, suffix: &str) {
        if let (Some(o), Some(n)) = (old.child(key), new.child(key))
            && o.value.is_object()
            && n.value.is_object()
        {
            self.node(&format!("{path}{suffix}"), o, n);
        }
    }
}
