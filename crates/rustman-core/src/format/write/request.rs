//! Walks the typed model in schema order and builds the [`Node`] tree.

use super::{MapBuilder, Node, render};
use crate::collection::CollectionMeta;
use crate::model::{
    Assertion, Auth, Body, Capture, HeaderAssertion, HttpRequest, JsonAssertion, JsonContent,
    NameValue, OAuth2, Part, PartSource, Request, RequestSettings, StatusExpectation, VarValue,
    Variables,
};

/// The YAML text Rustman writes for a request file.
///
/// Requests of kinds this build does not model are written back unchanged.
pub fn request_to_yaml(request: &Request) -> String {
    match request {
        Request::Http(http) => render(&http_node(http)),
        Request::Opaque(opaque) => opaque.source.clone(),
    }
}

/// The YAML text Rustman writes for a `_collection.yaml` file.
pub fn collection_to_yaml(meta: &CollectionMeta) -> String {
    let node = MapBuilder::new()
        .opt_str("name", meta.name.as_deref())
        .opt_str("description", meta.description.as_deref())
        .opt("order", meta.order.as_deref().map(strings))
        .opt("headers", meta.headers.as_deref().map(name_values))
        .opt("auth", meta.auth.as_ref().map(auth))
        .opt("variables", meta.variables.as_ref().map(variables))
        .build();
    render(&node)
}

fn http_node(r: &HttpRequest) -> Node {
    MapBuilder::new()
        .node("version", Node::plain(r.version))
        .str("kind", &r.kind)
        .str("name", &r.name)
        .opt_str("description", r.description.as_deref())
        .opt("tags", r.tags.as_deref().map(strings))
        .opt_str("method", r.method.as_ref().map(|m| m.as_str()))
        .str("url", &r.url)
        .opt("query", r.query.as_deref().map(name_values))
        .opt("headers", r.headers.as_deref().map(name_values))
        .opt("body", r.body.as_ref().map(body))
        .opt("auth", r.auth.as_ref().map(auth))
        .opt("settings", r.settings.as_ref().map(settings))
        .opt("variables", r.variables.as_ref().map(variables))
        .opt(
            "assertions",
            r.assertions
                .as_deref()
                .map(|a| Node::Seq(a.iter().map(assertion).collect())),
        )
        .opt(
            "captures",
            r.captures
                .as_deref()
                .map(|c| Node::Seq(c.iter().map(|c| capture(c)).collect())),
        )
        .build()
}

fn strings(values: &[String]) -> Node {
    Node::Seq(values.iter().map(Node::str).collect())
}

fn name_values(pairs: &[NameValue]) -> Node {
    Node::Seq(
        pairs
            .iter()
            .map(|p| {
                MapBuilder::new()
                    .str("name", &p.name)
                    .str("value", &p.value)
                    .build()
            })
            .collect(),
    )
}

fn variables(vars: &Variables) -> Node {
    Node::Map(
        vars.iter()
            .map(|(name, value)| (name.to_owned(), var_value(value)))
            .collect(),
    )
}

fn var_value(value: &VarValue) -> Node {
    match value {
        VarValue::String(s) => Node::str(s),
        VarValue::Integer(n) => Node::plain(n),
        VarValue::Float(n) => Node::Plain(float(*n)),
        VarValue::Bool(b) => Node::plain(b),
    }
}

/// Floats keep a fractional part or exponent so they load back as floats.
fn float(n: f64) -> String {
    if n.is_nan() {
        ".nan".to_owned()
    } else if n.is_infinite() {
        if n > 0.0 { ".inf" } else { "-.inf" }.to_owned()
    } else {
        format!("{n:?}")
    }
}

fn json(value: &serde_json::Value) -> Node {
    use serde_json::Value;
    match value {
        Value::Null => Node::plain("null"),
        Value::Bool(b) => Node::plain(b),
        Value::Number(n) => Node::Plain(
            n.as_f64()
                .filter(|_| n.is_f64())
                .map_or_else(|| n.to_string(), float),
        ),
        Value::String(s) => Node::str(s),
        Value::Array(items) => Node::Seq(items.iter().map(json).collect()),
        Value::Object(map) => Node::Map(map.iter().map(|(k, v)| (k.clone(), json(v))).collect()),
    }
}

fn body(b: &Body) -> Node {
    let typed = |kind: &str| MapBuilder::new().str("type", kind);
    match b {
        Body::None => typed("none").build(),
        Body::Json(JsonContent::Value(value)) => typed("json").node("content", json(value)).build(),
        Body::Json(JsonContent::Text(text)) => typed("json").str("content", text).build(),
        Body::Text(text) => typed("text").str("content", text).build(),
        Body::Form(fields) => typed("form").node("fields", name_values(fields)).build(),
        Body::Multipart(parts) => typed("multipart")
            .node("parts", Node::Seq(parts.iter().map(part).collect()))
            .build(),
        Body::File(path) => typed("file").str("path", path).build(),
    }
}

fn part(p: &Part) -> Node {
    let builder = MapBuilder::new().str("name", &p.name);
    let builder = match &p.source {
        PartSource::Text(value) => builder.str("value", value),
        PartSource::File(path) => builder.str("file", path),
    };
    builder
        .opt_str("content_type", p.content_type.as_deref())
        .build()
}

fn auth(a: &Auth) -> Node {
    let typed = |kind: &str| MapBuilder::new().str("type", kind);
    match a {
        Auth::None => typed("none").build(),
        Auth::Basic { username, password } => typed("basic")
            .str("username", username)
            .str("password", password)
            .build(),
        Auth::Bearer { token } => typed("bearer").str("token", token).build(),
        Auth::ApiKey {
            name,
            value,
            location,
        } => typed("api_key")
            .str("name", name)
            .str("value", value)
            .opt_str("in", location.map(|l| l.as_str()))
            .build(),
        Auth::OAuth2(o) => oauth2(typed("oauth2"), o),
    }
}

fn oauth2(builder: MapBuilder, o: &OAuth2) -> Node {
    builder
        .str("grant", o.grant.as_str())
        .str("token_url", &o.token_url)
        .opt_str("auth_url", o.auth_url.as_deref())
        .str("client_id", &o.client_id)
        .opt_str("client_secret", o.client_secret.as_deref())
        .opt("scopes", o.scopes.as_deref().map(strings))
        .build()
}

fn settings(s: &RequestSettings) -> Node {
    let path = |p: &Option<crate::format::Located<String>>| p.as_ref().map(|p| Node::str(p.get()));
    MapBuilder::new()
        .opt_plain("timeout_ms", s.timeout_ms)
        .opt_plain("connect_timeout_ms", s.connect_timeout_ms)
        .opt_plain("follow_redirects", s.follow_redirects)
        .opt_plain("max_redirects", s.max_redirects)
        .opt_plain("verify_tls", s.verify_tls)
        .opt_plain("http2_prior_knowledge", s.http2_prior_knowledge)
        .opt_str("proxy", s.proxy.as_deref())
        .opt_plain("cookies", s.cookies)
        .opt_plain("decompress", s.decompress)
        .opt("ca_bundle", path(&s.ca_bundle))
        .opt("client_cert", path(&s.client_cert))
        .opt("client_key", path(&s.client_key))
        .opt("client_pkcs12", path(&s.client_pkcs12))
        .opt_str(
            "client_pkcs12_password",
            s.client_pkcs12_password.as_deref(),
        )
        .build()
}

fn assertion(a: &Assertion) -> Node {
    let (key, node) = match a {
        Assertion::Status(status) => ("status", status_node(status)),
        Assertion::Header(h) => ("header", header_assertion(h)),
        Assertion::Json(j) => ("json", json_assertion(j)),
        Assertion::Schema(s) => (
            "schema",
            MapBuilder::new()
                .opt("file", s.file.as_ref().map(|f| Node::str(f.get())))
                .opt("inline", s.inline.as_ref().map(json))
                .build(),
        ),
        Assertion::Body(b) => (
            "body",
            MapBuilder::new()
                .opt_str("contains", b.contains.as_deref())
                .opt_str("not_contains", b.not_contains.as_deref())
                .opt_str("matches", b.matches.as_deref())
                .opt_str("equals", b.equals.as_deref())
                .build(),
        ),
        Assertion::Latency(l) => (
            "latency",
            MapBuilder::new()
                .opt_plain("total_lt", l.total_lt)
                .opt_plain("ttfb_lt", l.ttfb_lt)
                .build(),
        ),
    };
    Node::Map(vec![(key.to_owned(), node)])
}

fn status_node(status: &StatusExpectation) -> Node {
    match status {
        StatusExpectation::Code(code) => Node::plain(code),
        StatusExpectation::AnyOf(codes) => Node::Seq(codes.iter().map(Node::plain).collect()),
        StatusExpectation::Class(digit) => Node::str(format!("{digit}xx")),
    }
}

fn header_assertion(h: &HeaderAssertion) -> Node {
    MapBuilder::new()
        .str("name", &h.name)
        .opt_str("equals", h.equals.as_deref())
        .opt_str("contains", h.contains.as_deref())
        .opt_str("matches", h.matches.as_deref())
        .opt_plain("exists", h.exists)
        .build()
}

fn json_assertion(j: &JsonAssertion) -> Node {
    MapBuilder::new()
        .str("path", &j.path)
        .opt("equals", j.equals.as_ref().map(json))
        .opt("not_equals", j.not_equals.as_ref().map(json))
        .opt("contains", j.contains.as_ref().map(json))
        .opt_str("matches", j.matches.as_deref())
        .opt("gt", j.gt.as_ref().map(json))
        .opt("gte", j.gte.as_ref().map(json))
        .opt("lt", j.lt.as_ref().map(json))
        .opt("lte", j.lte.as_ref().map(json))
        .opt_plain("exists", j.exists)
        .opt("length", j.length.as_ref().map(json))
        .opt_str("type", j.value_type.as_deref())
        .build()
}

fn capture(c: &Capture) -> Node {
    MapBuilder::new()
        .str("var", &c.var)
        .str("from", c.from.as_str())
        .opt_str("path", c.path.as_deref())
        .opt_str("name", c.name.as_deref())
        .opt_str("pattern", c.pattern.as_deref())
        .opt_plain("group", c.group)
        .opt_plain("required", c.required)
        .opt_plain("persist", c.persist)
        .build()
}
