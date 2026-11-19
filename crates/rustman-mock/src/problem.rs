//! Problem details (RFC 9457) for every error the mock generates itself
//! (spec: Problem details for mock errors): `type`, `title`, `status` and
//! `detail`, plus `errors` for validation failures.

use bytes::Bytes;
use http::header::{CONTENT_LENGTH, CONTENT_TYPE};
use http::{HeaderValue, Response, StatusCode};
use http_body_util::Full;
use serde::Serialize;

pub const PROBLEM_JSON: &str = "application/problem+json";

/// One violation, located in the request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProblemError {
    /// `path`, `query`, `header`, `cookie` or `body`.
    pub location: String,
    /// The JSON pointer of the offending value.
    pub pointer: String,
    pub message: String,
}

/// A problem+json body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Problem {
    #[serde(rename = "type")]
    pub kind: String,
    pub title: String,
    pub status: u16,
    pub detail: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<ProblemError>,
}

impl Problem {
    /// A problem of `status`; `slug` names its `type`
    /// (`urn:rustman:mock:<slug>`).
    pub fn new(status: StatusCode, slug: &str, title: &str, detail: impl Into<String>) -> Self {
        Self {
            kind: format!("urn:rustman:mock:{slug}"),
            title: title.to_owned(),
            status: status.as_u16(),
            detail: detail.into(),
            errors: Vec::new(),
        }
    }

    /// Add validation errors.
    #[must_use]
    pub fn with_errors(mut self, errors: Vec<ProblemError>) -> Self {
        self.errors = errors;
        self
    }

    /// No operation matches `path`.
    pub fn not_found(method: &str, path: &str) -> Self {
        let detail = format!("{method} {path} matches no operation of the document");
        Self::new(
            StatusCode::NOT_FOUND,
            "no-matching-operation",
            "No matching operation",
            detail,
        )
    }

    /// `path` exists but does not declare `method`.
    pub fn method_not_allowed(method: &str, path: &str, allowed: &str) -> Self {
        let detail = format!("{path} does not declare {method}; allowed: {allowed}");
        Self::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method-not-allowed",
            "Method not allowed",
            detail,
        )
    }

    pub fn status(&self) -> StatusCode {
        StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// The response carrying this problem.
    pub fn response(&self) -> Response<Full<Bytes>> {
        let body = self.to_json();
        let length = body.len();
        let mut response = Response::new(Full::new(Bytes::from(body)));
        *response.status_mut() = self.status();
        let headers = response.headers_mut();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static(PROBLEM_JSON));
        headers.insert(CONTENT_LENGTH, HeaderValue::from(length));
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_unknown_route_body() {
        let problem = Problem::not_found("GET", "/nope");
        let pretty = serde_json::to_string_pretty(&problem).unwrap();
        insta::assert_snapshot!(pretty, @r#"
        {
          "type": "urn:rustman:mock:no-matching-operation",
          "title": "No matching operation",
          "status": 404,
          "detail": "GET /nope matches no operation of the document"
        }
        "#);
        let response = problem.response();
        assert_eq!(response.status(), 404);
        assert_eq!(response.headers()[CONTENT_TYPE], PROBLEM_JSON);
    }

    #[test]
    fn validation_errors_are_listed() {
        let error = ProblemError {
            location: "query".into(),
            pointer: "/q".into(),
            message: "required".into(),
        };
        let problem = Problem::new(
            StatusCode::BAD_REQUEST,
            "invalid-request",
            "Invalid request",
            "1 error",
        )
        .with_errors(vec![error]);
        let json: serde_json::Value = serde_json::from_str(&problem.to_json()).unwrap();
        assert_eq!(
            json["errors"],
            serde_json::json!([{"location": "query", "pointer": "/q", "message": "required"}])
        );
    }
}
