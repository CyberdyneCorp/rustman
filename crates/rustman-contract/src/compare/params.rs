//! Parameters: path parameters matched by their position in the template,
//! the others by name and location; a name found in another location has
//! moved (`parameter-location-changed`).

use rustman_explore::openapi::validate::template_names;

use super::Sides;
use super::view::{OpView, Param};
use crate::catalog::RuleId;
use crate::difference::Sink;
use crate::rules::{Direction, DocsRule};
use crate::spec::Node;
use crate::walk::compare_schemas;

pub(super) fn compare(sides: Sides<'_>, old: &OpView<'_>, new: &OpView<'_>, sink: &mut Sink<'_>) {
    let old_params = old.parameters(&sides.old.document);
    let new_params = new.parameters(&sides.new.document);
    path_parameters(sides, (old, new), (&old_params, &new_params), sink);
    let old_rest: Vec<&Param<'_>> = old_params.iter().filter(|p| !is_path(p)).collect();
    let new_rest: Vec<&Param<'_>> = new_params.iter().filter(|p| !is_path(p)).collect();
    let mut matched = vec![false; new_rest.len()];
    for p in &old_rest {
        let same = |q: &&Param<'_>| p.same_name(q) && p.location == q.location;
        let moved = |q: &&Param<'_>| p.same_name(q) && p.location != q.location;
        if let Some(i) = new_rest.iter().position(same) {
            matched[i] = true;
            parameter(sides, p, new_rest[i], sink);
        } else if let Some(i) = new_rest.iter().position(moved) {
            matched[i] = true;
            location_changed(p, new_rest[i], sink);
        } else {
            let message = format!("{}: removed", p.label());
            sink.push(
                RuleId::ParameterRemoved,
                message,
                &p.node.pointer,
                &new.node.pointer,
            );
        }
    }
    for (q, _) in new_rest.iter().zip(&matched).filter(|(_, m)| !**m) {
        added(q, &old.node, sink);
    }
}

fn is_path(param: &Param<'_>) -> bool {
    param.location == "path"
}

/// Path parameters, paired by position (a rename is reported separately).
fn path_parameters(
    sides: Sides<'_>,
    (old, new): (&OpView<'_>, &OpView<'_>),
    (old_params, new_params): (&[Param<'_>], &[Param<'_>]),
    sink: &mut Sink<'_>,
) {
    let names = template_names(&old.op.path)
        .into_iter()
        .zip(template_names(&new.op.path));
    for (old_name, new_name) in names {
        let find = |list: &'_ [Param<'_>], name: &str| {
            list.iter().position(|p| is_path(p) && p.name == name)
        };
        if let (Some(o), Some(n)) = (find(old_params, old_name), find(new_params, new_name)) {
            parameter(sides, &old_params[o], &new_params[n], sink);
        }
    }
}

fn location_changed(old: &Param<'_>, new: &Param<'_>, sink: &mut Sink<'_>) {
    let message = format!(
        "parameter '{}': moved from {} to {}",
        new.name, old.location, new.location
    );
    sink.push(
        RuleId::ParameterLocationChanged,
        message,
        &old.node.pointer,
        &new.node.pointer,
    );
}

fn added(param: &Param<'_>, old_op: &Node<'_>, sink: &mut Sink<'_>) {
    let (rule, what) = if param.required {
        (RuleId::RequestRequiredParameterAdded, "added, required")
    } else {
        (RuleId::RequestOptionalParameterAdded, "added, optional")
    };
    let message = format!("{}: {what}", param.label());
    sink.push(rule, message, &old_op.pointer, &param.node.pointer);
}

/// The same parameter in both documents.
fn parameter(sides: Sides<'_>, old: &Param<'_>, new: &Param<'_>, sink: &mut Sink<'_>) {
    let label = new.label();
    if old.required != new.required {
        let (rule, what) = if new.required {
            (RuleId::ParameterBecameRequired, "now required")
        } else {
            (RuleId::ParameterBecameOptional, "no longer required")
        };
        let message = format!("{label}: {what}");
        sink.push(rule, message, &old.node.pointer, &new.node.pointer);
    }
    sides.object(&label, (&old.node, &new.node), &DocsRule::SCHEMA, sink);
    if let (Some(o), Some(n)) = (schema(&old.node), schema(&new.node)) {
        compare_schemas(Direction::Request, &label, (o, n), sides.docs(), sink);
    }
}

/// A parameter's (or header's) schema: `schema`, or the one of its single `content` entry.
pub(super) fn schema<'a>(param: &Node<'a>) -> Option<Node<'a>> {
    if let Some(schema) = param.child("schema") {
        return Some(schema);
    }
    let content = param.child("content")?;
    let media = content.value.as_object()?.keys().next()?.clone();
    content.child(&media)?.child("schema")
}
