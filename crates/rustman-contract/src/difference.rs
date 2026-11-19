//! What a diff reports: one [`Difference`] per change, each naming a rule.

use serde::Serialize;

use crate::catalog::{CATALOG_VERSION, Classification, RuleId};

/// One change between the two documents.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Difference {
    pub rule: RuleId,
    pub classification: Classification,
    /// `METHOD /path`, as written in the new document when the operation
    /// exists there.
    pub operation: String,
    /// What changed, e.g. `response 200 (application/json) $.email: removed`.
    pub message: String,
    /// Where the change is in the old document (the closest enclosing
    /// element when it is not there).
    pub old_pointer: String,
    /// Where the change is in the new document (likewise).
    pub new_pointer: String,
}

/// The result of comparing two documents.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Diff {
    pub catalog_version: u32,
    /// How the old and new documents are named (path or URL).
    pub old: String,
    pub new: String,
    /// In a stable order: the old document's operations in document
    /// order, then the added ones in the new document's order.
    pub differences: Vec<Difference>,
}

impl Diff {
    pub fn new(old: &str, new: &str, differences: Vec<Difference>) -> Diff {
        Diff {
            catalog_version: CATALOG_VERSION,
            old: old.to_owned(),
            new: new.to_owned(),
            differences,
        }
    }

    /// How many differences have `classification`.
    pub fn count(&self, classification: Classification) -> usize {
        self.differences
            .iter()
            .filter(|d| d.classification == classification)
            .count()
    }

    /// `2 breaking, 1 non-breaking, 0 unknown`.
    pub fn summary(&self) -> String {
        format!(
            "{} breaking, {} non-breaking, {} unknown",
            self.count(Classification::Breaking),
            self.count(Classification::NonBreaking),
            self.count(Classification::Unknown)
        )
    }
}

/// Collects the differences of one operation.
pub struct Sink<'o> {
    operation: String,
    out: &'o mut Vec<Difference>,
}

impl<'o> Sink<'o> {
    pub fn new(operation: impl Into<String>, out: &'o mut Vec<Difference>) -> Sink<'o> {
        Sink {
            operation: operation.into(),
            out,
        }
    }

    pub fn push(&mut self, rule: RuleId, message: String, old_pointer: &str, new_pointer: &str) {
        self.out.push(Difference {
            rule,
            classification: rule.classification(),
            operation: self.operation.clone(),
            message,
            old_pointer: old_pointer.to_owned(),
            new_pointer: new_pointer.to_owned(),
        });
    }
}
