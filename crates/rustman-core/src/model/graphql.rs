//! A `kind: graphql` request as the HTTP POST it sends, for exports that
//! only need the wire request (snippets). Execution belongs to the GraphQL
//! client; this reads only the fields a POST needs and ignores the rest.

use std::path::Path;

use serde::Deserialize;
use serde_json::{Map, Value};

use super::auth::Auth;
use super::body::{Body, JsonContent};
use super::common::NameValue;
use super::http::{HttpRequest, Method};
use super::kind::OpaqueRequest;
use crate::error::LoadError;
use crate::format::from_yaml;

/// The `Accept` header a GraphQL POST sends unless the request sets one.
pub const GRAPHQL_ACCEPT: &str = "application/graphql-response+json, application/json";

/// The fields of a `kind: graphql` file a POST is built from.
#[derive(Deserialize)]
struct GraphqlFields {
    endpoint: String,
    document: Option<String>,
    document_file: Option<String>,
    operation_name: Option<String>,
    variables: Option<Map<String, Value>>,
    headers: Option<Vec<NameValue>>,
    auth: Option<Auth>,
}

/// `request` (a `kind: graphql` file at `file`) as an HTTP POST of
/// `{"query", "operationName", "variables"}`. `read_document` reads a
/// `document_file` path. The document is never interpolated.
pub fn graphql_as_http(
    file: &Path,
    request: &OpaqueRequest,
    read_document: impl Fn(&str) -> Result<String, String>,
) -> Result<HttpRequest, LoadError> {
    let fields: GraphqlFields = from_yaml(file, &request.source)?;
    let invalid = |message: String| LoadError::new(file, None, format!("graphql: {message}"));
    let document = match (&fields.document, &fields.document_file) {
        (Some(document), None) => document.clone(),
        (None, Some(path)) => read_document(path).map_err(invalid)?,
        _ => {
            return Err(invalid(
                "exactly one of document or document_file is required".into(),
            ));
        }
    };
    let mut payload = Map::new();
    payload.insert("query".into(), Value::String(escape_template(&document)));
    if let Some(name) = fields.operation_name {
        payload.insert("operationName".into(), Value::String(name));
    }
    if let Some(variables) = fields.variables {
        payload.insert("variables".into(), Value::Object(variables));
    }
    Ok(HttpRequest {
        version: super::kind::REQUEST_VERSION,
        kind: "http".into(),
        name: request.name.clone(),
        description: request.description.clone(),
        tags: request.tags.clone(),
        method: Some(Method::parse("POST").expect("POST is a method")),
        url: fields.endpoint,
        query: None,
        headers: Some(with_accept(fields.headers.unwrap_or_default())),
        body: Some(Body::Json(JsonContent::Value(Value::Object(payload)))),
        auth: fields.auth,
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
    use crate::model::Request;
    use crate::model::kind::load_opaque;

    fn opaque(text: &str) -> OpaqueRequest {
        match load_opaque(Path::new("q.rman.yaml"), text).unwrap() {
            Request::Opaque(r) => r,
            Request::Http(_) => unreachable!(),
        }
    }

    fn no_file(path: &str) -> Result<String, String> {
        Err(format!("no file {path}"))
    }

    #[test]
    fn a_query_becomes_a_json_post() {
        let text = "version: 1\nkind: graphql\nname: users\nendpoint: '{{base}}/graphql'\ndocument: 'query Users { users { id } }'\noperation_name: Users\nvariables:\n  id: '{{id}}'\n";
        let http = graphql_as_http(Path::new("q.rman.yaml"), &opaque(text), no_file).unwrap();
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
    fn a_document_file_is_read_and_exactly_one_source_is_required() {
        let text =
            "version: 1\nkind: graphql\nname: q\nendpoint: http://x\ndocument_file: q.graphql\n";
        let read = |path: &str| Ok(format!("read {path}"));
        let http = graphql_as_http(Path::new("q.rman.yaml"), &opaque(text), read).unwrap();
        assert!(
            matches!(http.body, Some(Body::Json(JsonContent::Value(ref v))) if v["query"] == "read q.graphql")
        );
        let neither = "version: 1\nkind: graphql\nname: q\nendpoint: http://x\n";
        let err = graphql_as_http(Path::new("q.rman.yaml"), &opaque(neither), no_file).unwrap_err();
        assert_eq!(
            err.message,
            "graphql: exactly one of document or document_file is required"
        );
    }
}
