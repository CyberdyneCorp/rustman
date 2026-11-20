//! Walks the typed model in schema order and builds the [`Node`] tree.

use super::{MapBuilder, Node, render};
use crate::collection::{CollectionMeta, CollectionSource};
use crate::environment::Environment;
use crate::model::signed_auth::DEFAULT_JWT_TTL_SECS;
use crate::model::{
    ApiKeyLocation, Assertion, Auth, AwsCredentials, AwsSigV4, Body, Capture, CountExpectation,
    CountOp, GraphqlMethod, GraphqlRequest, GraphqlTransport, HeaderAssertion, HttpRequest,
    JsonAssertion, JsonContent, Jwt, McpRequest, NameValue, OAuth2, Part, PartSource, Request,
    RequestSettings, RpcErrorExpectation, SnapshotAssertion, StatusExpectation, VarValue,
    Variables, WsProtocol,
};

/// The YAML text Rustman writes for a request file.
///
/// Requests of kinds this build does not model are written back unchanged.
pub fn request_to_yaml(request: &Request) -> String {
    match request {
        Request::Http(http) => render(&http_node(http)),
        Request::Graphql(graphql) => graphql.source.clone(),
        Request::Jsonrpc(jsonrpc) => jsonrpc.source.clone(),
        Request::Mcp(mcp) if mcp.source.is_empty() => mcp_to_yaml(mcp),
        Request::Mcp(mcp) => mcp.source.clone(),
        Request::Grpc(grpc) => grpc.source.clone(),
        Request::Ws(ws) if ws.source.is_empty() => super::realtime::ws_to_yaml(ws),
        Request::Ws(ws) => ws.source.clone(),
        Request::Sse(sse) if sse.source.is_empty() => super::realtime::sse_to_yaml(sse),
        Request::Sse(sse) => sse.source.clone(),
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
        .opt("parallel", meta.parallel.map(Node::plain))
        .opt_str("data", meta.data.as_deref())
        .opt(
            "secret_columns",
            meta.secret_columns.as_deref().map(strings),
        )
        .opt("source", meta.source.as_deref().map(source))
        .build();
    render(&node)
}

fn source(s: &CollectionSource) -> Node {
    MapBuilder::new()
        .str("format", &s.format)
        .str("location", &s.location)
        .str("sha256", &s.sha256)
        .build()
}

/// The YAML text of an `assertions:` list (the TUI's Assertions editor).
pub fn assertions_to_yaml(assertions: &[Assertion]) -> String {
    if assertions.is_empty() {
        return String::new();
    }
    render(&Node::Seq(assertions.iter().map(assertion).collect()))
}

/// The YAML text Rustman writes for an environment file: its variables,
/// the names of its secrets (never their values) and `production: true`.
pub fn environment_to_yaml(env: &Environment) -> String {
    let secrets = (!env.secrets.is_empty()).then(|| strings(&env.secrets));
    let node = MapBuilder::new()
        .opt(
            "variables",
            (!env.variables.is_empty()).then(|| variables(&env.variables)),
        )
        .opt("secrets", secrets)
        .opt("production", env.production.then(|| Node::plain(true)))
        .build();
    render(&node)
}

/// Nodes for request kinds this crate does not model, such as the
/// `kind: graphql` files written by importers.
pub fn auth_node(a: &Auth) -> Node {
    auth(a)
}

/// A `{name, value}` list node (headers, query parameters, form fields).
pub fn name_values_node(pairs: &[NameValue]) -> Node {
    name_values(pairs)
}

/// A JSON value as a YAML node.
pub fn json_node(value: &serde_json::Value) -> Node {
    json(value)
}

/// The YAML text of a `kind: graphql` request, every field in schema
/// order. [`request_to_yaml`] keeps a loaded file's text as it is; this is
/// for a request that was edited (the TUI's GraphQL pane).
pub fn graphql_to_yaml(r: &GraphqlRequest) -> String {
    let node = MapBuilder::new()
        .node("version", Node::plain(r.version))
        .str("kind", &r.kind)
        .str("name", &r.name)
        .opt_str("description", r.description.as_deref())
        .opt("tags", r.tags.as_deref().map(strings))
        .str("endpoint", &r.endpoint)
        .opt_str("document", r.document.as_deref())
        .opt_str("document_file", r.document_file.as_deref())
        .opt_str("operation_name", r.operation_name.as_deref())
        .opt("variables", r.variables.as_ref().map(json_map))
        .opt("headers", r.headers.as_deref().map(name_values))
        .opt("auth", r.auth.as_ref().map(auth))
        .opt_str("transport", r.transport.map(transport_name))
        .opt_str("method", r.method.map(graphql_method_name))
        .opt_plain("apq", r.apq)
        .opt_str("schema_file", r.schema_file.as_deref())
        .opt_str("ws_protocol", r.ws_protocol.map(ws_protocol_name))
        .opt(
            "connection_params",
            r.connection_params.as_ref().map(json_map),
        )
        .opt_plain("max_events", r.max_events)
        .opt("duration", r.duration.map(duration_node))
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
        .build();
    render(&node)
}

/// The YAML text of a `kind: mcp` request, every field in schema order:
/// for a request built in the TUI (a loaded file keeps its text).
pub fn mcp_to_yaml(r: &McpRequest) -> String {
    let node = MapBuilder::new()
        .node("version", Node::plain(r.version))
        .str("kind", &r.kind)
        .str("name", &r.name)
        .opt_str("description", r.description.as_deref())
        .opt("tags", r.tags.as_deref().map(strings))
        .str("server", &r.server)
        .str("operation", r.operation.as_str())
        .opt("params", r.params.as_ref().map(json_map))
        .opt("timeout", r.timeout.map(duration_node))
        .opt_plain("validate", r.validate)
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
        .build();
    render(&node)
}

fn json_map(map: &serde_json::Map<String, serde_json::Value>) -> Node {
    Node::Map(map.iter().map(|(k, v)| (k.clone(), json(v))).collect())
}

fn transport_name(transport: GraphqlTransport) -> &'static str {
    match transport {
        GraphqlTransport::Http => "http",
        GraphqlTransport::Ws => "ws",
        GraphqlTransport::Sse => "sse",
    }
}

fn graphql_method_name(method: GraphqlMethod) -> &'static str {
    match method {
        GraphqlMethod::Post => "POST",
        GraphqlMethod::Get => "GET",
    }
}

fn ws_protocol_name(protocol: WsProtocol) -> &'static str {
    match protocol {
        WsProtocol::Auto => "auto",
        WsProtocol::GraphqlTransportWs => "graphql-transport-ws",
        WsProtocol::GraphqlWs => "graphql-ws",
    }
}

/// `2m`, `30s` or `1500ms`: the largest unit that writes it exactly.
fn duration_node(duration: std::time::Duration) -> Node {
    let ms = duration.as_millis();
    let text = if ms.is_multiple_of(60_000) {
        format!("{}m", ms / 60_000)
    } else if ms.is_multiple_of(1000) {
        format!("{}s", ms / 1000)
    } else {
        format!("{ms}ms")
    };
    Node::str(text)
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

pub(super) fn strings(values: &[String]) -> Node {
    Node::Seq(values.iter().map(Node::str).collect())
}

pub(super) fn name_values(pairs: &[NameValue]) -> Node {
    Node::Seq(
        pairs
            .iter()
            .map(|p| {
                MapBuilder::new()
                    .str("name", &p.name)
                    .str("value", &p.value)
                    .opt_plain("enabled", (!p.enabled).then_some(false))
                    .build()
            })
            .collect(),
    )
}

pub(super) fn variables(vars: &Variables) -> Node {
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

pub(super) fn json(value: &serde_json::Value) -> Node {
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

pub(super) fn body(b: &Body) -> Node {
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

pub(super) fn auth(a: &Auth) -> Node {
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
        Auth::Digest { username, password } => typed("digest")
            .str("username", username)
            .str("password", password)
            .build(),
        Auth::AwsSigV4(sigv4) => aws_sigv4(typed("aws_sigv4"), sigv4),
        Auth::Jwt(jwt) => jwt_node(typed("jwt"), jwt),
    }
}

fn aws_sigv4(builder: MapBuilder, s: &AwsSigV4) -> Node {
    let builder = builder.str("region", &s.region).str("service", &s.service);
    let builder = match &s.credentials {
        AwsCredentials::Fields {
            access_key,
            secret_key,
            session_token,
        } => builder
            .str("access_key", access_key)
            .str("secret_key", secret_key)
            .opt_str("session_token", session_token.as_deref()),
        AwsCredentials::Env => builder.str("from", "env"),
        AwsCredentials::Profile(name) => builder.str("from", "profile").str("profile", name),
    };
    builder
        .opt_plain("unsigned_payload", s.unsigned_payload.then_some(true))
        .build()
}

/// `ttl` in the largest whole unit: `1h`, `10m` or `90s`.
fn ttl_text(secs: u64) -> String {
    match secs {
        s if s % 3600 == 0 => format!("{}h", s / 3600),
        s if s % 60 == 0 => format!("{}m", s / 60),
        s => format!("{s}s"),
    }
}

fn jwt_node(builder: MapBuilder, j: &Jwt) -> Node {
    let object = |map: &serde_json::Map<String, serde_json::Value>| {
        (!map.is_empty()).then(|| json(&serde_json::Value::Object(map.clone())))
    };
    let ttl = (j.ttl_secs != DEFAULT_JWT_TTL_SECS).then(|| ttl_text(j.ttl_secs));
    let location = (j.location != ApiKeyLocation::Header).then(|| j.location.as_str());
    builder
        .str("algorithm", j.algorithm.as_str())
        .opt_str("secret", j.secret.as_deref())
        .opt_str("private_key", j.private_key.as_deref())
        .opt_str(
            "private_key_file",
            j.private_key_file.as_deref().map(String::as_str),
        )
        .opt("claims", object(&j.claims))
        .opt("header", object(&j.header))
        .opt_str("ttl", ttl.as_deref())
        .opt_str("in", location)
        .opt_str("name", j.name.as_deref())
        .build()
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

pub(super) fn assertion(a: &Assertion) -> Node {
    let (key, node) = match a {
        Assertion::Snapshot(s) if ***s == SnapshotAssertion::default() => {
            return Node::str("snapshot");
        }
        Assertion::Snapshot(s) => ("snapshot", snapshot_node(s)),
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
        Assertion::NoErrors(expected) => ("no_errors", Node::plain(expected)),
        Assertion::ErrorsCount(count) => ("errors_count", count_node(count)),
        Assertion::ErrorMessageContains(text) => ("error_message_contains", Node::str(text)),
        Assertion::ErrorCode(code) => ("error_code", Node::str(code)),
        Assertion::Data(j) => ("data", json_assertion(j)),
        Assertion::RpcError(e) => ("rpc_error", rpc_error_node(e)),
        Assertion::IsError(expected) => ("is_error", Node::plain(expected)),
        Assertion::ContentTextContains(text) => ("content_text_contains", Node::str(text)),
        Assertion::Structured(j) => ("structured", json_assertion(j)),
        Assertion::OutputSchema(expected) => ("output_schema", Node::plain(expected)),
        Assertion::ToolExists(name) => ("tool_exists", Node::str(name)),
        Assertion::GrpcStatus(e) => ("grpc_status", Node::str(e.name())),
        Assertion::Metadata(m) => ("metadata", header_assertion(m)),
        Assertion::Messages(m) => (
            "messages",
            MapBuilder::new()
                .opt_plain("count_gte", m.count_gte)
                .opt_plain("count_lte", m.count_lte)
                .opt_plain("count_eq", m.count_eq)
                .build(),
        ),
        Assertion::CloseCode(code) => ("close_code", Node::plain(code)),
    };
    Node::Map(vec![(key.to_owned(), node)])
}

fn rpc_error_node(expected: &RpcErrorExpectation) -> Node {
    match expected {
        RpcErrorExpectation::None => Node::str("none"),
        RpcErrorExpectation::Code {
            code,
            message_contains,
        } => MapBuilder::new()
            .opt_plain("code", Some(code))
            .opt_str("message_contains", message_contains.as_deref())
            .build(),
    }
}

fn snapshot_node(s: &SnapshotAssertion) -> Node {
    let names = |v: &[String]| strings(v);
    let located = |v: &[crate::format::Located<String>]| {
        Node::Seq(v.iter().map(|p| Node::str(p.get())).collect())
    };
    MapBuilder::new()
        .opt("name", s.name.as_ref().map(|n| Node::str(n.get())))
        .opt(
            "compare",
            s.compare
                .as_ref()
                .map(|c| Node::Seq(c.iter().map(|p| Node::str(p.as_str())).collect())),
        )
        .opt("ignore", s.ignore.as_deref().map(located))
        .opt(
            "headers",
            s.headers.as_ref().map(|h| {
                MapBuilder::new()
                    .opt("include", h.include.as_deref().map(names))
                    .opt("redact", h.redact.as_deref().map(names))
                    .build()
            }),
        )
        .opt(
            "replace",
            s.replace.as_ref().map(|rules| {
                Node::Seq(
                    rules
                        .iter()
                        .map(|r| {
                            MapBuilder::new()
                                .str("pattern", r.pattern.get())
                                .str("with", &r.with)
                                .build()
                        })
                        .collect(),
                )
            }),
        )
        .opt(
            "sort",
            s.sort.as_ref().map(|rules| {
                Node::Seq(
                    rules
                        .iter()
                        .map(|r| {
                            MapBuilder::new()
                                .str("path", r.path.get())
                                .opt_str("by", r.by.as_deref())
                                .build()
                        })
                        .collect(),
                )
            }),
        )
        .opt(
            "presets",
            s.presets
                .as_ref()
                .map(|p| Node::Seq(p.iter().map(|p| Node::str(p.as_str())).collect())),
        )
        .build()
}

fn status_node(status: &StatusExpectation) -> Node {
    match status {
        StatusExpectation::Code(code) => Node::plain(code),
        StatusExpectation::AnyOf(codes) => Node::Seq(codes.iter().map(Node::plain).collect()),
        StatusExpectation::Class(digit) => Node::str(format!("{digit}xx")),
    }
}

fn count_node(count: &CountExpectation) -> Node {
    match count.op {
        CountOp::Equals => Node::plain(count.value),
        op => MapBuilder::new()
            .opt_plain(op.as_str(), Some(count.value))
            .build(),
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

pub(super) fn json_assertion(j: &JsonAssertion) -> Node {
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

pub(super) fn capture(c: &Capture) -> Node {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environment::parse;

    #[test]
    fn environment_files_round_trip_without_secret_values() {
        let env = Environment {
            name: "staging".into(),
            variables: Variables::new(vec![
                ("base_url".into(), VarValue::String("https://x".into())),
                ("port".into(), VarValue::Integer(8080)),
            ]),
            secrets: vec!["api_key".into()],
            production: false,
            trace: Default::default(),
        };
        let text = environment_to_yaml(&env);
        assert_eq!(
            text,
            "variables:\n  base_url: https://x\n  port: 8080\nsecrets:\n  - api_key\n"
        );
        let loaded = parse(std::path::Path::new("staging.yaml"), "staging", &text).unwrap();
        assert_eq!(loaded, env);
        let empty = Environment {
            production: true,
            ..Environment::default()
        };
        assert_eq!(environment_to_yaml(&empty), "production: true\n");
    }
}
