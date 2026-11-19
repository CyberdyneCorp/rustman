//! Response headers: matched by name ignoring case, then compared like
//! parameters with their schemas walked in the response direction.

use serde_json::Value;

use super::Sides;
use super::params::schema;
use crate::catalog::RuleId;
use crate::difference::Sink;
use crate::rules::{Direction, DocsRule};
use crate::spec::Node;
use crate::walk::compare_schemas;

/// The headers of the same response (`context`) in both documents.
pub(super) fn compare(
    sides: Sides<'_>,
    context: &str,
    (old, new): (&Node<'_>, &Node<'_>),
    sink: &mut Sink<'_>,
) {
    let old_headers = list(old, &sides.old.document);
    let new_headers = list(new, &sides.new.document);
    for (name, o) in &old_headers {
        let label = format!("{context} header '{name}'");
        match find(&new_headers, name) {
            Some(n) => header(sides, &label, (o, n), sink),
            None => {
                let message = format!("{label}: removed");
                sink.push(
                    RuleId::ResponseHeaderRemoved,
                    message,
                    &o.pointer,
                    &new.pointer,
                );
            }
        }
    }
    for (name, n) in &new_headers {
        if find(&old_headers, name).is_none() {
            let message = format!("{context} header '{name}': added");
            sink.push(
                RuleId::ResponseHeaderAdded,
                message,
                &old.pointer,
                &n.pointer,
            );
        }
    }
}

/// A response's headers, resolved, in document order.
fn list<'a>(response: &Node<'a>, doc: &'a Value) -> Vec<(&'a str, Node<'a>)> {
    let Some(headers) = response.child("headers") else {
        return Vec::new();
    };
    let Some(map) = headers.value.as_object() else {
        return Vec::new();
    };
    map.keys()
        .filter_map(|name| Some((name.as_str(), headers.child(name)?.resolved(doc))))
        .collect()
}

fn find<'l, 'a>(headers: &'l [(&'a str, Node<'a>)], name: &str) -> Option<&'l Node<'a>> {
    headers
        .iter()
        .find(|(other, _)| other.eq_ignore_ascii_case(name))
        .map(|(_, node)| node)
}

/// The same header in both documents.
fn header(sides: Sides<'_>, label: &str, (old, new): (&Node<'_>, &Node<'_>), sink: &mut Sink<'_>) {
    let required = |node: &Node<'_>| node.get("required").and_then(Value::as_bool) == Some(true);
    if required(old) != required(new) {
        let (rule, what) = if required(new) {
            (RuleId::ResponseHeaderBecameRequired, "now required")
        } else {
            (RuleId::ResponseHeaderBecameOptional, "no longer required")
        };
        let message = format!("{label}: {what}");
        sink.push(rule, message, &old.pointer, &new.pointer);
    }
    sides.object(label, (old, new), &DocsRule::SCHEMA, sink);
    if let (Some(o), Some(n)) = (schema(old), schema(new)) {
        compare_schemas(Direction::Response, label, (o, n), sides.docs(), sink);
    }
}
