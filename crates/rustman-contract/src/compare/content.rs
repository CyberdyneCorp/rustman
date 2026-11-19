//! Request bodies and responses: presence, `required`, media types, headers
//! (see `headers`), and
//! their schemas walked in the request or response direction.

use std::collections::BTreeSet;

use serde_json::Value;

use super::Sides;
use super::view::OpView;
use crate::catalog::RuleId;
use crate::difference::Sink;
use crate::rules::{Direction, DocsRule};
use crate::spec::Node;
use crate::walk::compare_schemas;

pub(super) fn request_body(
    sides: Sides<'_>,
    old: &OpView<'_>,
    new: &OpView<'_>,
    sink: &mut Sink<'_>,
) {
    let old_body = old
        .node
        .child("requestBody")
        .map(|b| b.resolved(&sides.old.document));
    let new_body = new
        .node
        .child("requestBody")
        .map(|b| b.resolved(&sides.new.document));
    match (old_body, new_body) {
        (None, None) => {}
        (Some(o), None) => {
            let message = "request body: removed".to_owned();
            sink.push(
                RuleId::RequestBodyRemoved,
                message,
                &o.pointer,
                &new.node.pointer,
            );
        }
        (None, Some(n)) => {
            let (rule, what) = if required(&n) {
                (RuleId::RequestBodyBecameRequired, "added, required")
            } else {
                (RuleId::RequestBodyAdded, "added, optional")
            };
            let message = format!("request body: {what}");
            sink.push(rule, message, &old.node.pointer, &n.pointer);
        }
        (Some(o), Some(n)) => both_bodies(sides, &o, &n, sink),
    }
}

fn both_bodies(sides: Sides<'_>, old: &Node<'_>, new: &Node<'_>, sink: &mut Sink<'_>) {
    if required(old) != required(new) {
        let (rule, what) = if required(new) {
            (RuleId::RequestBodyBecameRequired, "now required")
        } else {
            (RuleId::RequestBodyBecameOptional, "no longer required")
        };
        let message = format!("request body: {what}");
        sink.push(rule, message, &old.pointer, &new.pointer);
    }
    sides.object("request body", (old, new), &DocsRule::SCHEMA, sink);
    media_types(sides, Direction::Request, "request body", (old, new), sink);
}

fn required(body: &Node<'_>) -> bool {
    body.get("required").and_then(Value::as_bool) == Some(true)
}

/// Responses by status. A status removed from the new document is still
/// described when the new document has a response that covers it
/// (OpenAPI's response ranges and `default`, see [`covering`]): then it is
/// compared with that response instead of being reported as removed.
pub(super) fn responses(sides: Sides<'_>, old: &OpView<'_>, new: &OpView<'_>, sink: &mut Sink<'_>) {
    let (Some(old_map), Some(new_map)) = (old.node.child("responses"), new.node.child("responses"))
    else {
        return;
    };
    let new_keys = keys(&new_map);
    for status in keys(&old_map).union(&new_keys) {
        let o = old_map
            .child(status)
            .map(|r| r.resolved(&sides.old.document));
        let n = new_map
            .child(status)
            .map(|r| r.resolved(&sides.new.document));
        let context = format!("response {status}");
        match (o, n) {
            (Some(o), None) => match covering(status, &new_keys) {
                Some(cover) => {
                    let n = new_map
                        .child(cover)
                        .map(|r| r.resolved(&sides.new.document));
                    let context = format!("{context} (now {cover})");
                    if let Some(n) = n {
                        response(sides, &context, (&o, &n), sink);
                    }
                }
                None => {
                    let message = format!("{context}: removed");
                    sink.push(
                        RuleId::ResponseStatusRemoved,
                        message,
                        &o.pointer,
                        &new_map.pointer,
                    );
                }
            },
            (None, Some(n)) => {
                let message = format!("{context}: added");
                sink.push(
                    RuleId::ResponseStatusAdded,
                    message,
                    &old_map.pointer,
                    &n.pointer,
                );
            }
            (Some(o), Some(n)) => response(sides, &context, (&o, &n), sink),
            (None, None) => {}
        }
    }
}

/// Two responses for the same status: documentation, headers and content.
fn response(sides: Sides<'_>, context: &str, (o, n): (&Node<'_>, &Node<'_>), sink: &mut Sink<'_>) {
    sides.object(context, (o, n), &DocsRule::SCHEMA, sink);
    super::headers::compare(sides, context, (o, n), sink);
    media_types(sides, Direction::Response, context, (o, n), sink);
}

/// The response among `keys` that describes `status` when `status` itself
/// is not declared. OpenAPI lets a range (`1XX` to `5XX`) stand for every
/// code it covers, an explicit code taking precedence, and `default` for
/// every status not declared otherwise. So a code is covered by its range,
/// else by `default`; a range only by `default`; `default` by nothing.
fn covering<'k>(status: &str, keys: &BTreeSet<&'k str>) -> Option<&'k str> {
    let bytes = status.as_bytes();
    let code = bytes.len() == 3 && bytes.iter().all(u8::is_ascii_digit);
    let range = code
        .then(|| format!("{}XX", &status[..1]))
        .and_then(|range| keys.iter().find(|k| k.eq_ignore_ascii_case(&range)));
    let default = (status != "default").then(|| keys.get("default")).flatten();
    range.or(default).copied()
}

/// The member names of an object, sorted.
fn keys<'a>(node: &Node<'a>) -> BTreeSet<&'a str> {
    node.value
        .as_object()
        .map(|map| map.keys().map(String::as_str).collect())
        .unwrap_or_default()
}

/// The `content` maps of a body or response: media types removed and
/// added, and the schemas of the common ones.
fn media_types(
    sides: Sides<'_>,
    direction: Direction,
    context: &str,
    (old, new): (&Node<'_>, &Node<'_>),
    sink: &mut Sink<'_>,
) {
    let old_content = old.child("content");
    let new_content = new.child("content");
    let old_types = old_content.as_ref().map(keys).unwrap_or_default();
    let new_types = new_content.as_ref().map(keys).unwrap_or_default();
    let (removed, added) = match direction {
        Direction::Request => (
            RuleId::RequestMediaTypeRemoved,
            RuleId::RequestMediaTypeAdded,
        ),
        Direction::Response => (
            RuleId::ResponseMediaTypeRemoved,
            RuleId::ResponseMediaTypeAdded,
        ),
    };
    for media in old_types.union(&new_types) {
        let o = old_content.as_ref().and_then(|c| c.child(media));
        let n = new_content.as_ref().and_then(|c| c.child(media));
        match (o, n) {
            (Some(o), None) => {
                let message = format!("{context} ({media}): removed");
                sink.push(removed, message, &o.pointer, &new.pointer);
            }
            (None, Some(n)) => {
                let message = format!("{context} ({media}): added");
                sink.push(added, message, &old.pointer, &n.pointer);
            }
            (Some(o), Some(n)) => {
                let context = format!("{context} ({media})");
                media_type(sides, direction, &context, (&o, &n), sink);
            }
            (None, None) => {}
        }
    }
}

fn media_type(
    sides: Sides<'_>,
    direction: Direction,
    context: &str,
    (old, new): (&Node<'_>, &Node<'_>),
    sink: &mut Sink<'_>,
) {
    sides.object(context, (old, new), &DocsRule::SCHEMA, sink);
    if let (Some(o), Some(n)) = (old.child("schema"), new.child("schema")) {
        compare_schemas(direction, context, (o, n), sides.docs(), sink);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_covered_by_their_range_then_by_default() {
        let keys: BTreeSet<&str> = ["2XX", "4xx", "default"].into();
        assert_eq!(covering("200", &keys), Some("2XX"));
        assert_eq!(covering("404", &keys), Some("4xx"));
        assert_eq!(covering("500", &keys), Some("default"));
        assert_eq!(covering("3XX", &keys), Some("default"));
        assert_eq!(covering("default", &keys), None);
        let only_range: BTreeSet<&str> = ["2XX"].into();
        assert_eq!(covering("201", &only_range), Some("2XX"));
        assert_eq!(covering("404", &only_range), None);
        assert_eq!(covering("2XX", &only_range), None);
    }
}
