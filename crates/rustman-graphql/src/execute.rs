//! Queries and mutations over HTTP through `rustman-http`: POST (JSON or
//! multipart with uploads), GET for queries, and Automatic Persisted
//! Queries.

use std::fmt;
use std::path::PathBuf;

use rustman_core::RequestRef;
use rustman_core::model::{GRAPHQL_ACCEPT, GraphqlMethod, GraphqlRequest};
use rustman_core::vars::{
    Resolved, ResolvedBody, ResolvedFile, ResolvedGraphql, ResolvedPair, ResolvedPart,
    ResolvedPartSource, ResolvedRequest,
};
use rustman_http::{Exchange, HttpEngine, RequestError, SendOptions};
use serde_json::{Map, Value, json};

use crate::error::GraphqlError;
use crate::operation::{SelectedOperation, check_http_method, select_operation};
use crate::response::GraphqlResponse;
use crate::schema::sha256_hex;
use crate::upload::{Upload, extract_uploads, upload_map};

/// An operation ready to go over HTTP: selected, checked against its
/// method, with its uploads taken out of the variables.
#[derive(Clone, Debug)]
pub struct HttpOperation {
    /// Sent byte for byte as written.
    pub document: String,
    pub operation: SelectedOperation,
    /// `operationName` as the request sets it.
    pub operation_name: Option<String>,
    pub method: GraphqlMethod,
    pub apq: bool,
    /// The variables, with every upload replaced by `null`.
    pub variables: Map<String, Value>,
    pub uploads: Vec<Upload>,
}

impl HttpOperation {
    /// Prepare `document` for the graphql request `graphql` resolved as
    /// `target`. `resolve_file` turns an upload path into the file to read.
    /// Fails before anything is sent on an ambiguous document, a mutation
    /// over GET, uploads over GET and missing upload files.
    pub fn prepare(
        document: String,
        graphql: &GraphqlRequest,
        target: &ResolvedGraphql,
        resolve_file: impl Fn(&str) -> Result<PathBuf, GraphqlError>,
    ) -> Result<Self, GraphqlError> {
        let operation_name = target.operation_name.clone();
        let operation = select_operation(&document, operation_name.as_deref())?;
        let method = graphql.effective_method();
        check_http_method(&operation, graphql.effective_transport(), method)?;
        let mut variables = target.variables.clone();
        let uploads = extract_uploads(&mut variables, resolve_file)?;
        if !uploads.is_empty() && method == GraphqlMethod::Get {
            return Err(GraphqlError::UploadOverGet);
        }
        Ok(Self {
            document,
            operation,
            operation_name,
            method,
            apq: graphql.apq_enabled(),
            variables,
            uploads,
        })
    }

    /// [`HttpOperation::prepare`] for a workspace request: upload paths are
    /// relative to the request file and must stay inside the workspace.
    pub fn for_request(
        request: &RequestRef<'_>,
        graphql: &GraphqlRequest,
        document: String,
        target: &ResolvedGraphql,
    ) -> Result<Self, GraphqlError> {
        Self::prepare(document, graphql, target, |raw| {
            Ok(request.contain_file(raw)?)
        })
    }

    /// The SHA-256 of the document, as APQ sends it.
    pub fn document_hash(&self) -> String {
        sha256_hex(self.document.as_bytes())
    }

    /// The payload members: `query` (unless `with_query` is false),
    /// `operationName`, `variables` and the APQ `extensions`. Absent and
    /// empty members are left out.
    pub fn payload(&self, with_query: bool) -> Map<String, Value> {
        let mut payload = Map::new();
        if with_query {
            payload.insert("query".into(), Value::String(self.document.clone()));
        }
        if let Some(name) = &self.operation_name {
            payload.insert("operationName".into(), Value::String(name.clone()));
        }
        if !self.variables.is_empty() {
            payload.insert("variables".into(), Value::Object(self.variables.clone()));
        }
        if self.apq {
            let persisted = json!({"version": 1, "sha256Hash": self.document_hash()});
            payload.insert("extensions".into(), json!({ "persistedQuery": persisted }));
        }
        payload
    }

    /// The HTTP request for `target` carrying [`HttpOperation::payload`].
    pub fn http_request(&self, target: &ResolvedGraphql, with_query: bool) -> ResolvedRequest {
        let mut request = target.http.clone();
        let payload = self.payload(with_query);
        match self.method {
            GraphqlMethod::Get => {
                request.method = "GET".to_owned();
                request.query.extend(payload.iter().map(get_parameter));
            }
            GraphqlMethod::Post => {
                request.method = "POST".to_owned();
                request.body = self.post_body(payload);
            }
        }
        with_graphql_accept(&mut request);
        request
    }

    fn post_body(&self, payload: Map<String, Value>) -> ResolvedBody {
        if self.uploads.is_empty() {
            return ResolvedBody::Json(Value::Object(payload));
        }
        let text = |name: &str, value: String| ResolvedPart {
            name: name.to_owned(),
            source: ResolvedPartSource::Text(Resolved::literal(value)),
            content_type: None,
        };
        let mut parts = vec![
            text("operations", Value::Object(payload).to_string()),
            text("map", upload_map(&self.uploads).to_string()),
        ];
        parts.extend(self.uploads.iter().map(|u| ResolvedPart {
            name: u.key.clone(),
            source: ResolvedPartSource::File(ResolvedFile {
                raw: u.raw.clone(),
                path: u.file.clone(),
            }),
            content_type: Some(u.content_type.to_owned()),
        }));
        ResolvedBody::Multipart(parts)
    }
}

/// A GET URL parameter: `query` and `operationName` as text, the others
/// JSON-encoded.
fn get_parameter((name, value): (&String, &Value)) -> ResolvedPair {
    let text = match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    ResolvedPair {
        name: Resolved::literal(name.clone()),
        value: Resolved::literal(text),
    }
}

/// Add the GraphQL `Accept` header unless the request sets one.
pub(crate) fn with_graphql_accept(request: &mut ResolvedRequest) {
    if request
        .headers
        .iter()
        .any(|h| h.name.text.eq_ignore_ascii_case("accept"))
    {
        return;
    }
    request.headers.insert(
        0,
        ResolvedPair {
            name: Resolved::literal("Accept"),
            value: Resolved::literal(GRAPHQL_ACCEPT),
        },
    );
}

/// Whether the server knew the persisted query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApqOutcome {
    Hit,
    /// The hash was unknown; the full document was sent and registered.
    MissRegistered,
    /// The hash was unknown, and the server did not accept the full
    /// document either: it answered `PersistedQueryNotFound` again or
    /// something that is not a GraphQL result. Also the outcome when the
    /// server does not support persisted queries.
    MissNotRegistered,
}

impl ApqOutcome {
    /// The outcome after the full document was sent with the hash.
    fn after_retry(retry: Option<&GraphqlResponse>) -> Self {
        match retry {
            Some(response) if !response.persisted_query_not_found() => Self::MissRegistered,
            _ => Self::MissNotRegistered,
        }
    }
}

impl fmt::Display for ApqOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Hit => "APQ hit",
            Self::MissRegistered => "APQ miss (registered)",
            Self::MissNotRegistered => "APQ miss (not registered)",
        })
    }
}

/// What an HTTP operation produced.
#[derive(Clone, Debug)]
pub struct HttpResult {
    /// The last request sent, for redaction and history.
    pub request: ResolvedRequest,
    /// The last exchange: status, headers, timings and the raw body.
    pub exchange: Exchange,
    /// The parsed result; `None` when the body is not a GraphQL result
    /// (show the raw body with [`crate::response::NOT_GRAPHQL`]).
    pub response: Option<GraphqlResponse>,
    pub apq: Option<ApqOutcome>,
    /// How many HTTP requests were sent.
    pub requests: usize,
}

/// Send `operation` to the endpoint of `target`. With APQ, the hash goes
/// first and the full document follows once on `PersistedQueryNotFound`,
/// or on `PersistedQueryNotSupported`, which registers nothing.
/// A first answer that is not a GraphQL result tells neither hit nor miss:
/// its `apq` is `None`. A response body larger than `[http] max_body_bytes`
/// fails with [`GraphqlError::BodyTooLarge`] as soon as the limit is passed.
pub async fn execute_http(
    engine: &HttpEngine,
    options: &SendOptions,
    target: &ResolvedGraphql,
    operation: &HttpOperation,
) -> Result<HttpResult, GraphqlError> {
    if !operation.apq {
        return send(engine, options, target, operation, true, 1).await;
    }
    let first = send(engine, options, target, operation, false, 1).await?;
    let Some(response) = &first.response else {
        return Ok(first);
    };
    let unsupported = response.persisted_query_not_supported();
    if !unsupported && !response.persisted_query_not_found() {
        return Ok(HttpResult {
            apq: Some(ApqOutcome::Hit),
            ..first
        });
    }
    let second = send(engine, options, target, operation, true, 2).await?;
    let outcome = if unsupported {
        ApqOutcome::MissNotRegistered
    } else {
        ApqOutcome::after_retry(second.response.as_ref())
    };
    Ok(HttpResult {
        apq: Some(outcome),
        ..second
    })
}

async fn send(
    engine: &HttpEngine,
    options: &SendOptions,
    target: &ResolvedGraphql,
    operation: &HttpOperation,
    with_query: bool,
    requests: usize,
) -> Result<HttpResult, GraphqlError> {
    let request = operation.http_request(target, with_query);
    let limit = options.body.max_in_memory;
    let exchange = match engine.send(&request, &capped(options)).await {
        Ok(exchange) => exchange,
        Err(e) if over_limit(&e, limit) => return Err(GraphqlError::BodyTooLarge(limit)),
        Err(e) => return Err(e.into()),
    };
    if let Some(spill) = &exchange.spill {
        // Not expected under the cap; never read it back.
        let _ = std::fs::remove_file(spill);
        return Err(GraphqlError::BodyTooLarge(limit));
    }
    let response = GraphqlResponse::parse(&exchange.body);
    Ok(HttpResult {
        request,
        exchange,
        response,
        apq: None,
        requests,
    })
}

/// `options` with the body download stopped at the in-memory cap
/// (`[http] max_body_bytes`): a result is parsed whole and recorded, so it
/// is never spilled to disk, read back or stored past that cap.
fn capped(options: &SendOptions) -> SendOptions {
    let mut capped = options.clone();
    let limit = options.body.max_in_memory;
    capped.body.max_download = limit;
    capped.body.max_decoded = limit;
    capped
}

/// Whether `error` is the body reader stopping at `limit` bytes (as
/// received or decompressed).
fn over_limit(error: &RequestError, limit: u64) -> bool {
    let RequestError::Network(message) = error else {
        return false;
    };
    message.starts_with("network: the ")
        && message.ends_with(&format!("response body is larger than {limit} bytes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_body_limit_of_the_cap_is_over_limit() {
        let raw =
            RequestError::Network("network: the response body is larger than 64 bytes".into());
        let decoded = RequestError::Network(
            "network: the decompressed response body is larger than 64 bytes".into(),
        );
        assert!(over_limit(&raw, 64) && over_limit(&decoded, 64));
        assert!(!over_limit(&raw, 65));
        assert!(!over_limit(
            &RequestError::Network("network: reset".into()),
            64
        ));
        assert!(!over_limit(
            &RequestError::Io("the response body is larger than 64 bytes".into()),
            64
        ));
        let options = capped(&SendOptions::default());
        assert_eq!(options.body.max_download, options.body.max_in_memory);
        assert_eq!(options.body.max_decoded, options.body.max_in_memory);
    }
}
