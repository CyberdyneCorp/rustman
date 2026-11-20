//! Writers for the realtime kinds (`kind: ws`, `kind: sse`), every field in
//! schema order, for requests built in code. A loaded file keeps its text.

use super::request::{
    assertion, auth, body, capture, docs_node, json, json_assertion, name_values, strings,
    variables,
};
use super::{MapBuilder, Node, render};
use crate::format::Located;
use crate::model::{
    Assertion, BinarySource, Capture, Checked, CloseStep, Frame, RealtimeLimits, RealtimeTls,
    SseRequest, Step, WaitFor, WsRequest,
};

/// The YAML text of a `kind: ws` request.
pub fn ws_to_yaml(r: &WsRequest) -> String {
    let node = MapBuilder::new()
        .node("version", Node::plain(r.version))
        .str("kind", &r.kind)
        .str("name", &r.name)
        .opt_str("description", r.description.as_deref())
        .opt("tags", r.tags.as_deref().map(strings))
        .opt("docs", r.docs.as_ref().map(docs_node))
        .str("url", r.url.get())
        .opt("headers", r.headers.as_deref().map(name_values))
        .opt("auth", r.auth.as_ref().map(auth))
        .opt("subprotocols", r.subprotocols.as_deref().map(strings))
        .opt(
            "steps",
            r.steps
                .as_deref()
                .map(|s| Node::Seq(s.iter().map(|s| step(s.get())).collect())),
        )
        .opt("limits", r.limits.as_deref().map(limits))
        .opt("tls", r.tls.as_ref().map(tls))
        .opt("variables", r.variables.as_ref().map(variables))
        .opt("assertions", assertions(r.assertions.as_deref()))
        .opt("captures", captures(r.captures.as_deref()))
        .build();
    render(&node)
}

/// The YAML text of a `kind: sse` request.
pub fn sse_to_yaml(r: &SseRequest) -> String {
    let node = MapBuilder::new()
        .node("version", Node::plain(r.version))
        .str("kind", &r.kind)
        .str("name", &r.name)
        .opt_str("description", r.description.as_deref())
        .opt("tags", r.tags.as_deref().map(strings))
        .opt("docs", r.docs.as_ref().map(docs_node))
        .opt_str("method", r.method.as_ref().map(|m| m.as_str()))
        .str("url", r.url.get())
        .opt("headers", r.headers.as_deref().map(name_values))
        .opt("auth", r.auth.as_ref().map(auth))
        .opt("body", r.body.as_ref().map(body))
        .opt_str("last_event_id", r.last_event_id.as_deref())
        .opt(
            "reconnect",
            r.reconnect.map(|c| {
                MapBuilder::new()
                    .opt_plain("max_attempts", c.max_attempts)
                    .build()
            }),
        )
        .opt("limits", r.limits.as_deref().map(limits))
        .opt("tls", r.tls.as_ref().map(tls))
        .opt("variables", r.variables.as_ref().map(variables))
        .opt("assertions", assertions(r.assertions.as_deref()))
        .opt("captures", captures(r.captures.as_deref()))
        .build();
    render(&node)
}

fn assertions(list: Option<&[Located<Assertion>]>) -> Option<Node> {
    list.map(|a| Node::Seq(a.iter().map(|a| assertion(a.get())).collect()))
}

fn captures(list: Option<&[Checked<Capture>]>) -> Option<Node> {
    list.map(|c| Node::Seq(c.iter().map(|c| capture(c)).collect()))
}

fn step(s: &Step) -> Node {
    let (key, node) = match s {
        Step::Send(f) => ("send", frame(f)),
        Step::WaitFor(w) => ("wait_for", wait_for(w)),
        Step::Sleep(ms) => ("sleep", Node::plain(ms)),
        Step::Close(c) => ("close", close(c)),
    };
    Node::Map(vec![(key.to_owned(), node)])
}

fn frame(f: &Frame) -> Node {
    let (key, node) = match f {
        Frame::Text(text) => ("text", Node::str(text)),
        Frame::Json(value) => ("json", json(value)),
        Frame::Binary(BinarySource::Base64(data)) => ("binary", Node::str(data)),
        Frame::Binary(BinarySource::File(path)) => {
            ("binary", MapBuilder::new().str("file", path.get()).build())
        }
        Frame::Ping(data) => ("ping", Node::str(data)),
    };
    Node::Map(vec![(key.to_owned(), node)])
}

fn wait_for(w: &WaitFor) -> Node {
    MapBuilder::new()
        .opt("json", w.json.as_deref().map(json_assertion))
        .opt_str("regex", w.regex.as_deref())
        .opt_plain("timeout_ms", w.timeout_ms)
        .build()
}

fn close(c: &CloseStep) -> Node {
    MapBuilder::new()
        .opt_plain("code", c.code)
        .opt_str("reason", c.reason.as_deref())
        .build()
}

fn limits(l: &RealtimeLimits) -> Node {
    MapBuilder::new()
        .opt_plain("max_messages", l.max_messages)
        .opt_plain("duration_ms", l.duration_ms)
        .opt_plain("max_message_bytes", l.max_message_bytes)
        .opt_plain("max_timeline", l.max_timeline)
        .build()
}

fn tls(t: &RealtimeTls) -> Node {
    let path = |p: &Option<Located<String>>| p.as_ref().map(|p| Node::str(p.get()));
    MapBuilder::new()
        .opt_plain("verify", t.verify)
        .opt("ca_bundle", path(&t.ca_bundle))
        .opt("client_cert", path(&t.client_cert))
        .opt("client_key", path(&t.client_key))
        .opt("client_pkcs12", path(&t.client_pkcs12))
        .opt_str(
            "client_pkcs12_password",
            t.client_pkcs12_password.as_deref(),
        )
        .build()
}
