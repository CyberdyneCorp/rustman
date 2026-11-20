//! The diff engine (contract-testing "OpenAPI diff command"): operations
//! matched by method and path template, then compared part by part.

mod content;
mod headers;
mod params;
mod security;
mod view;

use std::collections::HashMap;

use rustman_explore::openapi::validate::template_names;

use crate::catalog::RuleId;
use crate::difference::{Diff, Difference, Sink};
use crate::rules::{Direction, DocsRule, Pair, Rule, VendorRule};
use crate::spec::{Node, Spec, template_key};

use view::OpView;

/// Every difference between `old` and `new`, in a stable order.
pub fn diff(old: &Spec, new: &Spec) -> Diff {
    let old_ops = OpView::all(&old.document, &old.model.operations);
    let new_ops = OpView::all(&new.document, &new.model.operations);
    let new_by_key: HashMap<&str, &OpView<'_>> =
        new_ops.iter().map(|op| (op.key.as_str(), op)).collect();
    let old_keys: HashMap<&str, &OpView<'_>> =
        old_ops.iter().map(|op| (op.key.as_str(), op)).collect();
    let sides = Sides { old, new };
    let mut out = Vec::new();
    for op in &old_ops {
        match new_by_key.get(op.key.as_str()) {
            Some(counterpart) => sides.operation(op, counterpart, &mut out),
            None => removed(op, &new_ops, &mut out),
        }
    }
    for op in new_ops
        .iter()
        .filter(|op| !old_keys.contains_key(op.key.as_str()))
    {
        added(op, &old_ops, &mut out);
    }
    Diff::new(&old.name, &new.name, out)
}

fn removed(op: &OpView<'_>, others: &[OpView<'_>], out: &mut Vec<Difference>) {
    let message = "operation removed".to_owned();
    let other = enclosing(op, others);
    Sink::new(&op.display, out).push(RuleId::OperationRemoved, message, &op.node.pointer, &other);
}

fn added(op: &OpView<'_>, others: &[OpView<'_>], out: &mut Vec<Difference>) {
    let message = "operation added".to_owned();
    let other = enclosing(op, others);
    Sink::new(&op.display, out).push(RuleId::OperationAdded, message, &other, &op.node.pointer);
}

/// The path item of the same template in the other document, or `/paths`.
fn enclosing(op: &OpView<'_>, others: &[OpView<'_>]) -> String {
    let template = template_key(&op.op.path);
    others
        .iter()
        .find(|other| template_key(&other.op.path) == template)
        .map_or_else(|| "/paths".to_owned(), |other| other.item.pointer.clone())
}

/// The two documents being compared.
#[derive(Clone, Copy)]
pub(crate) struct Sides<'a> {
    pub old: &'a Spec,
    pub new: &'a Spec,
}

impl<'a> Sides<'a> {
    fn operation(&self, old: &OpView<'a>, new: &OpView<'a>, out: &mut Vec<Difference>) {
        let mut sink = Sink::new(&new.display, out);
        let sink = &mut sink;
        renamed_path_parameters(old, new, sink);
        if !old.op.deprecated && new.op.deprecated {
            let message = "operation: deprecated".to_owned();
            sink.push(
                RuleId::OperationDeprecated,
                message,
                &old.node.pointer,
                &new.node.pointer,
            );
        }
        self.object(
            "operation",
            (&old.node, &new.node),
            &DocsRule::OPERATION,
            sink,
        );
        params::compare(*self, old, new, sink);
        content::request_body(*self, old, new, sink);
        content::responses(*self, old, new, sink);
        security::compare(*self, old, new, sink);
    }

    /// Documentation and vendor extensions of two OpenAPI objects.
    pub(crate) fn object(
        &self,
        context: &str,
        (old, new): (&Node<'a>, &Node<'a>),
        docs: &DocsRule,
        sink: &mut Sink<'_>,
    ) {
        let pair = Pair {
            direction: Direction::Request,
            context,
            path: "$",
            old,
            new,
            old_doc: &self.old.document,
            new_doc: &self.new.document,
        };
        docs.check(&pair, sink);
        VendorRule.check(&pair, sink);
    }

    pub(crate) fn docs(&self) -> (&'a serde_json::Value, &'a serde_json::Value) {
        (&self.old.document, &self.new.document)
    }
}

fn renamed_path_parameters(old: &OpView<'_>, new: &OpView<'_>, sink: &mut Sink<'_>) {
    let renames: Vec<String> = template_names(&old.op.path)
        .into_iter()
        .zip(template_names(&new.op.path))
        .filter(|(a, b)| a != b)
        .map(|(a, b)| format!("{{{a}}} -> {{{b}}}"))
        .collect();
    if !renames.is_empty() {
        let message = format!("path parameter renamed: {}", renames.join(", "));
        let (o, n) = (&old.item.pointer, &new.item.pointer);
        sink.push(RuleId::PathParameterRenamed, message, o, n);
    }
}
