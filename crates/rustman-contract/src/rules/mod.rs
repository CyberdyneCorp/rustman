//! The rules that compare one pair of schema (or OpenAPI) objects (design
//! D2). Each is a small struct implementing [`Rule`]; the walker applies
//! [`SCHEMA_RULES`] to every pair of schemas it visits. A rule may report
//! one of several catalog ids, chosen by the walk's [`Direction`]: the same
//! change is breaking in a request and harmless in a response, or the other
//! way round.

mod composition;
mod constraints;
mod docs;
mod enums;
mod object;
mod properties;
mod subschema;
mod types;

use serde_json::Value;

use crate::difference::Sink;
use crate::spec::Node;

pub use composition::CompositionRule;
pub use constraints::ConstraintRule;
pub use docs::{DocsRule, VendorRule};
pub use enums::EnumRule;
pub use object::{parts, property};
pub use properties::PropertiesRule;
pub use subschema::SubschemaRule;
pub use types::TypeRule;

/// Which way data flows through a schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// Clients send it (parameters, request bodies): stricter breaks them.
    Request,
    /// Clients receive it (response bodies): fewer guarantees break them.
    Response,
}

/// The two versions of one element, with where they are.
pub struct Pair<'p, 'a> {
    pub direction: Direction,
    /// What the element belongs to, e.g. `request body (application/json)`.
    pub context: &'p str,
    /// The JSONPath-like place inside it, `$` for the element itself.
    pub path: &'p str,
    pub old: &'p Node<'a>,
    pub new: &'p Node<'a>,
    pub old_doc: &'a Value,
    pub new_doc: &'a Value,
}

impl Pair<'_, '_> {
    /// `request body (application/json) $.email`, or the context alone at
    /// the root.
    pub fn place(&self, member: Option<&str>) -> String {
        let path = match member {
            Some(name) => format!("{}.{name}", self.path),
            None => self.path.to_owned(),
        };
        if path == "$" {
            self.context.to_owned()
        } else {
            format!("{} {path}", self.context)
        }
    }

    /// Report `rule` at the element itself.
    pub fn report(&self, sink: &mut Sink<'_>, rule: crate::catalog::RuleId, what: String) {
        let message = format!("{}: {what}", self.place(None));
        sink.push(rule, message, &self.old.pointer, &self.new.pointer);
    }
}

/// A check on one pair of elements.
pub trait Rule: Sync {
    fn check(&self, pair: &Pair<'_, '_>, sink: &mut Sink<'_>);
}

/// Applied to every pair of schemas the walker visits.
pub static SCHEMA_RULES: &[&dyn Rule] = &[
    &TypeRule,
    &EnumRule,
    &ConstraintRule,
    &SubschemaRule,
    &PropertiesRule,
    &CompositionRule,
    &DocsRule::SCHEMA,
    &VendorRule,
];

/// A JSON value in a message: compact JSON, or `none` when absent.
pub(crate) fn shown(value: Option<&Value>) -> String {
    value.map_or_else(|| "none".to_owned(), Value::to_string)
}
