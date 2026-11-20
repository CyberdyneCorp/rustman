//! The `kind: graphql` request file, and the HTTP POST it sends for exports
//! that only need the wire request (snippets).
//!
//! Execution belongs to the `rustman-graphql` crate; this module only holds
//! the file model and its load-time checks.

use std::path::Path;
use std::time::Duration;

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::assertion::Assertion;
use super::auth::Auth;
use super::body::{Body, JsonContent};
use super::capture::Capture;
use super::common::Checked;
use super::common::NameValue;
use super::http::{HttpRequest, Method};
use crate::duration::parse_duration;
use crate::error::LoadError;
use crate::format::{Located, from_yaml};

/// The `Accept` header a GraphQL POST sends unless the request sets one.
pub const GRAPHQL_ACCEPT: &str = "application/graphql-response+json, application/json";

/// Events a headless subscription collects when `max_events` is not set.
pub const DEFAULT_MAX_EVENTS: u32 = 10;

/// How long a headless subscription runs when `duration` is not set.
pub const DEFAULT_DURATION: Duration = Duration::from_secs(30);

/// The load error for a file with neither or both document sources.
pub const DOCUMENT_SOURCE_ERROR: &str =
    "graphql: exactly one of document or document_file is required";

/// A `kind: graphql` request file. Optional fields stay `None` when absent;
/// the `effective_*` methods apply the defaults. Saving writes `source`
/// back unchanged.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphqlRequest {
    pub version: u64,
    pub kind: String,
    pub name: String,
    pub description: Option<String>,
    pub tags: Option<Vec<String>>,
    /// How the request appears in `rustman docs` output.
    pub docs: Option<super::docs::DocsMeta>,
    /// The GraphQL endpoint URL; `{{variables}}` are interpolated.
    pub endpoint: String,
    /// The inline document; never interpolated.
    pub document: Option<String>,
    /// A document file, relative to the request file's folder.
    pub document_file: Option<String>,
    pub operation_name: Option<String>,
    /// GraphQL variables sent in the payload. String values are interpolated.
    pub variables: Option<Map<String, Value>>,
    pub headers: Option<Vec<NameValue>>,
    pub auth: Option<Auth>,
    pub transport: Option<GraphqlTransport>,
    /// `GET` sends a query as URL parameters; mutations cannot use it.
    pub method: Option<GraphqlMethod>,
    /// Automatic Persisted Queries.
    pub apq: Option<bool>,
    /// An SDL (`.graphql`/`.graphqls`) or introspection JSON file, relative
    /// to the request file's folder; preferred over the schema cache.
    pub schema_file: Option<String>,
    pub ws_protocol: Option<WsProtocol>,
    /// The `connection_init` payload of WebSocket subscriptions.
    pub connection_params: Option<Map<String, Value>>,
    /// Headless subscriptions stop after this many events.
    pub max_events: Option<u32>,
    /// Headless subscriptions stop after this long (`500ms`, `30s`, `2m`).
    #[serde(default, deserialize_with = "duration")]
    pub duration: Option<Duration>,
    /// Checks of the result, HTTP and GraphQL ones (`no_errors`, `data`...).
    #[serde(default, deserialize_with = "super::snapshot::unique_snapshot_names")]
    pub assertions: Option<Vec<Assertion>>,
    /// Values taken from the result for later requests, such as
    /// `$.data.login.token`.
    pub captures: Option<Vec<Checked<Capture>>>,
    /// The file text, written back unchanged on save.
    #[serde(skip)]
    pub source: String,
}

/// How the operation travels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GraphqlTransport {
    #[default]
    Http,
    Ws,
    Sse,
}

/// The HTTP method of a `transport: http` operation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub enum GraphqlMethod {
    #[default]
    #[serde(rename = "POST")]
    Post,
    #[serde(rename = "GET")]
    Get,
}

/// The WebSocket subprotocol of `transport: ws`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub enum WsProtocol {
    /// `graphql-transport-ws`, falling back to the legacy protocol.
    #[default]
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "graphql-transport-ws")]
    GraphqlTransportWs,
    /// The legacy subscriptions-transport-ws protocol.
    #[serde(rename = "graphql-ws")]
    GraphqlWs,
}

/// Where the document comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentSource<'a> {
    Inline(&'a str),
    /// A path relative to the request file's folder.
    File(&'a str),
}

fn duration<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Duration>, D::Error> {
    let text = String::deserialize(deserializer)?;
    parse_duration(&text).map(Some).map_err(de::Error::custom)
}

impl GraphqlRequest {
    /// Parse and check the `kind: graphql` file `file` whose text is `text`.
    pub fn parse(file: &Path, text: &str) -> Result<Self, LoadError> {
        let mut request: Self = from_yaml(file, text)?;
        request.check(file)?;
        request.source = text.to_owned();
        Ok(request)
    }

    fn check(&self, file: &Path) -> Result<(), LoadError> {
        let invalid = |message: &str| Err(LoadError::new(file, None, message));
        if self.document.is_some() == self.document_file.is_some() {
            return invalid(DOCUMENT_SOURCE_ERROR);
        }
        if self.endpoint.trim().is_empty() {
            return invalid("graphql: endpoint must not be empty");
        }
        if self.max_events == Some(0) {
            return invalid("graphql: max_events must be at least 1");
        }
        Ok(())
    }

    /// The document source: exactly one exists once the file has loaded.
    pub fn document_source(&self) -> DocumentSource<'_> {
        match (&self.document, &self.document_file) {
            (Some(text), _) => DocumentSource::Inline(text),
            (None, Some(path)) => DocumentSource::File(path),
            (None, None) => DocumentSource::Inline(""),
        }
    }

    pub fn effective_transport(&self) -> GraphqlTransport {
        self.transport.unwrap_or_default()
    }

    pub fn effective_method(&self) -> GraphqlMethod {
        self.method.unwrap_or_default()
    }

    pub fn effective_ws_protocol(&self) -> WsProtocol {
        self.ws_protocol.unwrap_or_default()
    }

    pub fn apq_enabled(&self) -> bool {
        self.apq.unwrap_or(false)
    }

    pub fn effective_max_events(&self) -> u32 {
        self.max_events.unwrap_or(DEFAULT_MAX_EVENTS)
    }

    pub fn effective_duration(&self) -> Duration {
        self.duration.unwrap_or(DEFAULT_DURATION)
    }

    /// Every workspace file the assertions refer to (`schema: {file}`).
    pub fn file_paths(&self) -> Vec<&Located<String>> {
        self.assertions
            .iter()
            .flatten()
            .filter_map(|a| match a {
                Assertion::Schema(schema) => schema.file.as_ref(),
                _ => None,
            })
            .collect()
    }
}

/// `request` (the `kind: graphql` file at `file`) as an HTTP POST of
/// `{"query", "operationName", "variables"}`. `read_document` reads a
/// `document_file` path. The document is never interpolated.
pub fn graphql_as_http(
    file: &Path,
    request: &GraphqlRequest,
    read_document: impl Fn(&str) -> Result<String, String>,
) -> Result<HttpRequest, LoadError> {
    let document = match request.document_source() {
        DocumentSource::Inline(text) => text.to_owned(),
        DocumentSource::File(path) => read_document(path)
            .map_err(|message| LoadError::new(file, None, format!("graphql: {message}")))?,
    };
    let mut payload = Map::new();
    payload.insert("query".into(), Value::String(escape_template(&document)));
    if let Some(name) = &request.operation_name {
        payload.insert("operationName".into(), Value::String(name.clone()));
    }
    if let Some(variables) = &request.variables {
        payload.insert("variables".into(), Value::Object(variables.clone()));
    }
    Ok(HttpRequest {
        version: super::kind::REQUEST_VERSION,
        kind: "http".into(),
        name: request.name.clone(),
        description: request.description.clone(),
        tags: request.tags.clone(),
        docs: request.docs.clone(),
        method: Some(Method::parse("POST").expect("POST is a method")),
        url: request.endpoint.clone(),
        query: None,
        headers: Some(with_accept(request.headers.clone().unwrap_or_default())),
        body: Some(Body::Json(JsonContent::Value(Value::Object(payload)))),
        auth: request.auth.clone(),
        settings: None,
        // `variables` holds the GraphQL variables, sent in the payload; it
        // is not a layer of Rustman variables.
        variables: None,
        assertions: None,
        captures: None,
    })
}

/// `headers` with the GraphQL `Accept` added unless one is set.
fn with_accept(mut headers: Vec<NameValue>) -> Vec<NameValue> {
    if !headers
        .iter()
        .any(|h| h.name.eq_ignore_ascii_case("Accept"))
    {
        headers.insert(0, NameValue::new("Accept", GRAPHQL_ACCEPT));
    }
    headers
}

/// `text` with every `{{` written as `\{{`, so interpolation leaves it as is.
fn escape_template(text: &str) -> String {
    text.replace("{{", "\\{{")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<GraphqlRequest, LoadError> {
        GraphqlRequest::parse(Path::new("q.rman.yaml"), text)
    }

    fn no_file(path: &str) -> Result<String, String> {
        Err(format!("no file {path}"))
    }

    #[test]
    fn a_query_becomes_a_json_post() {
        let text = "version: 1\nkind: graphql\nname: users\nendpoint: '{{base}}/graphql'\ndocument: 'query Users { users { id } }'\noperation_name: Users\nvariables:\n  id: '{{id}}'\n";
        let request = parse(text).unwrap();
        let http = graphql_as_http(Path::new("q.rman.yaml"), &request, no_file).unwrap();
        assert_eq!(http.effective_method(), "POST");
        assert_eq!(http.variables, None);
        assert_eq!(http.url, "{{base}}/graphql");
        assert_eq!(
            http.headers.unwrap(),
            [NameValue::new("Accept", GRAPHQL_ACCEPT)]
        );
        let Some(Body::Json(JsonContent::Value(body))) = http.body else {
            panic!("a JSON body");
        };
        assert_eq!(
            body,
            serde_json::json!({
                "query": "query Users { users { id } }",
                "operationName": "Users",
                "variables": {"id": "{{id}}"},
            })
        );
    }

    #[test]
    fn the_document_is_never_interpolated() {
        assert_eq!(
            escape_template("{ a(x: \"{{x}}\") }"),
            "{ a(x: \"\\{{x}}\") }"
        );
    }

    #[test]
    fn a_document_file_is_read() {
        let text =
            "version: 1\nkind: graphql\nname: q\nendpoint: http://x\ndocument_file: q.graphql\n";
        let read = |path: &str| Ok(format!("read {path}"));
        let http = graphql_as_http(Path::new("q.rman.yaml"), &parse(text).unwrap(), read).unwrap();
        assert!(
            matches!(http.body, Some(Body::Json(JsonContent::Value(ref v))) if v["query"] == "read q.graphql")
        );
        let err =
            graphql_as_http(Path::new("q.rman.yaml"), &parse(text).unwrap(), no_file).unwrap_err();
        assert_eq!(err.message, "graphql: no file q.graphql");
    }

    #[test]
    fn every_field_loads() {
        let text = "\
version: 1
kind: graphql
name: On message
endpoint: wss://api.example.com/graphql
document_file: on-message.graphql
operation_name: OnMessage
variables:
  room: '{{room}}'
  filter:
    tags: [a, b]
headers:
  - name: X-Client
    value: rustman
auth:
  type: bearer
  token: '{{token}}'
transport: ws
method: GET
apq: true
schema_file: schema.graphql
ws_protocol: graphql-ws
connection_params:
  authToken: '{{token}}'
max_events: 3
duration: 5s
assertions:
  - no_errors: true
  - schema: {file: schemas/event.json}
captures:
  - {var: first, from: json, path: '$.data[0].onMessage.id'}
";
        let r = parse(text).unwrap();
        assert_transport_fields(&r);
        assert_content_fields(&r);
        assert_eq!(r.source, text);
    }

    fn assert_transport_fields(r: &GraphqlRequest) {
        assert_eq!(
            r.document_source(),
            DocumentSource::File("on-message.graphql")
        );
        assert_eq!(r.effective_transport(), GraphqlTransport::Ws);
        assert_eq!(r.effective_method(), GraphqlMethod::Get);
        assert_eq!(r.effective_ws_protocol(), WsProtocol::GraphqlWs);
        assert!(r.apq_enabled());
        assert_eq!(r.schema_file.as_deref(), Some("schema.graphql"));
        assert_eq!(r.effective_max_events(), 3);
        assert_eq!(r.effective_duration(), Duration::from_secs(5));
    }

    fn assert_content_fields(r: &GraphqlRequest) {
        assert_eq!(r.variables.as_ref().unwrap()["filter"]["tags"][1], "b");
        assert_eq!(
            r.connection_params.as_ref().unwrap()["authToken"],
            "{{token}}"
        );
        assert!(matches!(r.auth, Some(Auth::Bearer { .. })));
        assert_eq!(r.assertions.as_ref().unwrap()[0], Assertion::NoErrors(true));
        assert_eq!(r.captures.as_ref().unwrap()[0].var, "first");
        let files: Vec<&str> = r.file_paths().iter().map(|p| p.get().as_str()).collect();
        assert_eq!(files, ["schemas/event.json"]);
    }

    #[test]
    fn defaults_apply_when_fields_are_absent() {
        let r =
            parse("version: 1\nkind: graphql\nname: q\nendpoint: http://x\ndocument: '{ a }'\n")
                .unwrap();
        assert_eq!(r.document_source(), DocumentSource::Inline("{ a }"));
        assert_eq!(r.effective_transport(), GraphqlTransport::Http);
        assert_eq!(r.effective_method(), GraphqlMethod::Post);
        assert_eq!(r.effective_ws_protocol(), WsProtocol::Auto);
        assert!(!r.apq_enabled());
        assert_eq!(r.effective_max_events(), 10);
        assert_eq!(r.effective_duration(), Duration::from_secs(30));
    }

    #[test]
    fn exactly_one_document_source_is_required() {
        let base = "version: 1\nkind: graphql\nname: q\nendpoint: http://x\n";
        for extra in ["", "document: '{ a }'\ndocument_file: q.graphql\n"] {
            let err = parse(&format!("{base}{extra}")).unwrap_err();
            assert_eq!(err.message, DOCUMENT_SOURCE_ERROR);
            assert_eq!(
                err.to_string(),
                format!("q.rman.yaml: {DOCUMENT_SOURCE_ERROR}")
            );
        }
    }

    #[test]
    fn invalid_values_are_rejected() {
        let base = "version: 1\nkind: graphql\nname: q\nendpoint: http://x\ndocument: '{ a }'\n";
        for (extra, expected) in [
            ("transport: grpc\n", "grpc"),
            ("method: PUT\n", "PUT"),
            ("ws_protocol: graphql-sse\n", "graphql-sse"),
            ("duration: 5h\n", "invalid duration '5h'"),
            ("max_events: 0\n", "max_events must be at least 1"),
            ("url: http://y\n", "url"),
        ] {
            let err = parse(&format!("{base}{extra}")).unwrap_err();
            assert!(err.to_string().contains(expected), "{extra}: {err}");
        }
        let empty = "version: 1\nkind: graphql\nname: q\nendpoint: ''\ndocument: '{ a }'\n";
        assert!(parse(empty).unwrap_err().message.contains("endpoint"));
    }
}
