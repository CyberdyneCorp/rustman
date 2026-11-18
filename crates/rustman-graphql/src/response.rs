//! GraphQL results: `data`, `errors` and `extensions` kept apart, with the
//! overall status (`ok`, `partial` or `errors`).

use std::fmt;

use serde::Serialize;
use serde_json::{Map, Value};

/// The message shown, with the raw body, for a body that is not a GraphQL
/// result (an HTML error page from a proxy, say).
pub const NOT_GRAPHQL: &str = "graphql: response is not a GraphQL result";

/// The outcome of a GraphQL result.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ResultStatus {
    /// Data and no errors.
    Ok,
    /// Data and errors side by side.
    Partial,
    /// Errors and no data.
    Errors,
}

impl ResultStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Partial => "partial",
            Self::Errors => "errors",
        }
    }
}

impl fmt::Display for ResultStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A 1-based location in the document an error points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ErrorLocation {
    pub line: u64,
    pub column: u64,
}

/// One entry of `errors`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ResponseError {
    pub message: String,
    pub locations: Vec<ErrorLocation>,
    /// The response path, such as `["user", "email"]` or `["users", 0]`.
    pub path: Vec<Value>,
    pub extensions: Option<Map<String, Value>>,
}

impl ResponseError {
    /// The path joined with dots: `user.email`, `users.0.name`.
    pub fn path_text(&self) -> String {
        let parts: Vec<String> = self
            .path
            .iter()
            .map(|segment| match segment {
                Value::String(name) => name.clone(),
                other => other.to_string(),
            })
            .collect();
        parts.join(".")
    }

    /// `extensions.code`, when it is a string.
    pub fn code(&self) -> Option<&str> {
        self.extensions.as_ref()?.get("code")?.as_str()
    }
}

/// A parsed GraphQL result.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct GraphqlResponse {
    pub status: ResultStatus,
    /// `data`; `None` when absent or null.
    pub data: Option<Value>,
    pub errors: Vec<ResponseError>,
    pub extensions: Option<Map<String, Value>>,
}

impl GraphqlResponse {
    /// Parse `body`. `None` when it is not a JSON object with `data` or
    /// `errors`: the caller shows the raw body with [`NOT_GRAPHQL`].
    pub fn parse(body: &[u8]) -> Option<Self> {
        let Ok(Value::Object(mut object)) = serde_json::from_slice::<Value>(body) else {
            return None;
        };
        if !object.contains_key("data") && !object.contains_key("errors") {
            return None;
        }
        let data = object.remove("data").filter(|d| !d.is_null());
        let errors = match object.remove("errors") {
            Some(Value::Array(entries)) => entries.iter().map(response_error).collect(),
            Some(Value::Null) | None => Vec::new(),
            Some(_) => return None,
        };
        let extensions = match object.remove("extensions") {
            Some(Value::Object(map)) => Some(map),
            _ => None,
        };
        Some(Self {
            status: status(data.is_some(), !errors.is_empty()),
            data,
            errors,
            extensions,
        })
    }

    /// Whether an error says the server does not know a persisted query.
    pub fn persisted_query_not_found(&self) -> bool {
        self.has_error("PersistedQueryNotFound", "PERSISTED_QUERY_NOT_FOUND")
    }

    /// Whether an error says the server does not support persisted queries.
    pub fn persisted_query_not_supported(&self) -> bool {
        self.has_error(
            "PersistedQueryNotSupported",
            "PERSISTED_QUERY_NOT_SUPPORTED",
        )
    }

    /// Whether an error has `message`, or `code` as its `extensions.code`.
    fn has_error(&self, message: &str, code: &str) -> bool {
        self.errors
            .iter()
            .any(|e| e.message == message || e.code() == Some(code))
    }
}

fn status(has_data: bool, has_errors: bool) -> ResultStatus {
    match (has_data, has_errors) {
        (_, false) => ResultStatus::Ok,
        (true, true) => ResultStatus::Partial,
        (false, true) => ResultStatus::Errors,
    }
}

/// An `errors` entry; a malformed one keeps its JSON text as the message.
fn response_error(entry: &Value) -> ResponseError {
    let message = match entry.get("message").and_then(Value::as_str) {
        Some(message) => message.to_owned(),
        None => entry.to_string(),
    };
    let locations = entry
        .get("locations")
        .and_then(Value::as_array)
        .map(|all| all.iter().filter_map(location).collect())
        .unwrap_or_default();
    let path = entry
        .get("path")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let extensions = entry.get("extensions").and_then(Value::as_object).cloned();
    ResponseError {
        message,
        locations,
        path,
        extensions,
    }
}

fn location(value: &Value) -> Option<ErrorLocation> {
    Some(ErrorLocation {
        line: value.get("line")?.as_u64()?,
        column: value.get("column")?.as_u64()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Option<GraphqlResponse> {
        GraphqlResponse::parse(text.as_bytes())
    }

    #[test]
    fn data_and_errors_together_are_partial() {
        let r = parse(
            r#"{"data":{"user":{"name":"Ada","email":null}},
                "errors":[{"message":"forbidden","locations":[{"line":1,"column":22}],
                           "path":["user","email"],"extensions":{"code":"FORBIDDEN"}}]}"#,
        )
        .unwrap();
        assert_eq!(r.status, ResultStatus::Partial);
        assert_eq!(r.data.as_ref().unwrap()["user"]["name"], "Ada");
        let error = &r.errors[0];
        assert_eq!(error.path_text(), "user.email");
        assert_eq!(error.code(), Some("FORBIDDEN"));
        assert_eq!(
            error.locations,
            [ErrorLocation {
                line: 1,
                column: 22
            }]
        );
    }

    #[test]
    fn statuses_follow_data_and_errors() {
        assert_eq!(
            parse(r#"{"data":{"a":1}}"#).unwrap().status,
            ResultStatus::Ok
        );
        let errors = parse(r#"{"data":null,"errors":[{"message":"x"}]}"#).unwrap();
        assert_eq!(errors.status, ResultStatus::Errors);
        assert_eq!(errors.data, None);
        let ext = parse(r#"{"data":{},"extensions":{"cost":3}}"#).unwrap();
        assert_eq!(ext.extensions.unwrap()["cost"], 3);
        assert_eq!(ResultStatus::Partial.to_string(), "partial");
    }

    #[test]
    fn list_indices_appear_in_paths_and_odd_errors_keep_their_json() {
        let r = parse(r#"{"errors":[{"path":["users",0,"name"]}]}"#).unwrap();
        assert_eq!(r.errors[0].path_text(), "users.0.name");
        assert_eq!(r.errors[0].message, r#"{"path":["users",0,"name"]}"#);
    }

    #[test]
    fn other_bodies_are_not_graphql_results() {
        for body in [
            "<html><body>502 Bad Gateway</body></html>",
            r#"{"message":"hello"}"#,
            r#"[{"data":{}}]"#,
            r#"{"errors":"boom"}"#,
            "",
        ] {
            assert!(parse(body).is_none(), "{body}");
        }
        assert_eq!(NOT_GRAPHQL, "graphql: response is not a GraphQL result");
    }

    #[test]
    fn persisted_query_misses_are_recognised_by_message_or_code() {
        let by_message = parse(r#"{"errors":[{"message":"PersistedQueryNotFound"}]}"#).unwrap();
        assert!(by_message.persisted_query_not_found());
        let by_code = parse(
            r#"{"errors":[{"message":"x","extensions":{"code":"PERSISTED_QUERY_NOT_FOUND"}}]}"#,
        )
        .unwrap();
        assert!(by_code.persisted_query_not_found());
        assert!(!parse(r#"{"data":{}}"#).unwrap().persisted_query_not_found());
    }
}
