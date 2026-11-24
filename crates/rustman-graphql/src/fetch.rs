//! Fetching a schema by introspection over HTTP, through `rustman-http`
//! (TLS, proxies, auth and redaction come from the transport).
//!
//! Only `rustman graphql introspect` and the TUI refresh action call this:
//! sending or validating a request never introspects implicitly.

use rustman_core::vars::{ResolvedBody, ResolvedGraphql, ResolvedRequest};
use rustman_http::{HttpEngine, SendOptions};
use serde_json::{Value, json};

use crate::cache::{CachedSchema, SchemaCache, normalize_endpoint};
use crate::error::GraphqlError;
use crate::execute::{capped_at, over_limit, with_graphql_accept};
use crate::introspection::{
    self, INTROSPECTION_QUERY, LEGACY_INTROSPECTION_QUERY, OPERATION_NAME, needs_legacy_query,
};
use crate::schema::GraphqlSchema;

/// A schema fetched by introspection.
#[derive(Clone, Debug)]
pub struct Introspected {
    /// The response body: `{"data": {"__schema": ...}}`.
    pub result: Value,
    pub schema: GraphqlSchema,
    /// The server rejected the full query and answered the legacy one.
    pub legacy: bool,
}

/// What one introspection request returned.
enum Attempt {
    Schema(Value),
    /// GraphQL errors and no schema; messages redacted.
    Rejected(Vec<String>),
}

/// Introspect the endpoint of `target`. A server that rejects the fields
/// newer than the June 2018 specification is asked again with the legacy
/// query. Every failure is a [`GraphqlError::IntrospectionFailed`] with a
/// redacted reason.
///
/// The response is parsed whole, so it is kept in memory and may be at
/// most `max_schema_bytes` (`[graphql] max_schema_bytes`): a larger one
/// stops downloading as soon as the limit is passed and is refused,
/// never spilled to disk or read back.
pub async fn introspect(
    engine: &HttpEngine,
    options: &SendOptions,
    target: &ResolvedGraphql,
    max_schema_bytes: u64,
) -> Result<Introspected, GraphqlError> {
    let options = &capped_at(options, max_schema_bytes);
    let (result, legacy) = match attempt(engine, options, target, INTROSPECTION_QUERY).await? {
        Attempt::Schema(result) => (result, false),
        Attempt::Rejected(errors) if needs_legacy_query(&errors) => {
            match attempt(engine, options, target, LEGACY_INTROSPECTION_QUERY).await? {
                Attempt::Schema(result) => (result, true),
                Attempt::Rejected(errors) => return Err(rejected(&errors)),
            }
        }
        Attempt::Rejected(errors) => return Err(rejected(&errors)),
    };
    let origin = target
        .redactor()
        .text(&normalize_endpoint(&target.http.url.text)?)
        .into_owned();
    let schema = GraphqlSchema::from_introspection(&result, &origin)?;
    Ok(Introspected {
        result,
        schema,
        legacy,
    })
}

/// Introspect the endpoint of `target` and replace its cached schema. The
/// response may be at most `max_schema_bytes` (see [`introspect`]).
pub async fn refresh_schema(
    engine: &HttpEngine,
    options: &SendOptions,
    target: &ResolvedGraphql,
    cache: &SchemaCache,
    max_schema_bytes: u64,
) -> Result<CachedSchema, GraphqlError> {
    let fetched = introspect(engine, options, target, max_schema_bytes).await?;
    cache.store(
        &target.http.url.text,
        &fetched.result,
        fetched.schema,
        &target.redactor(),
    )
}

fn rejected(errors: &[String]) -> GraphqlError {
    GraphqlError::IntrospectionFailed(errors.join("; "))
}

fn too_large(limit: u64) -> GraphqlError {
    GraphqlError::IntrospectionFailed(format!(
        "the response is larger than the {limit} byte limit ([graphql] max_schema_bytes)"
    ))
}

async fn attempt(
    engine: &HttpEngine,
    options: &SendOptions,
    target: &ResolvedGraphql,
    query: &str,
) -> Result<Attempt, GraphqlError> {
    let request = introspection_request(target, query);
    // `options` comes from `introspect`, capped at the schema limit.
    let limit = options.body.max_in_memory;
    let exchange = match engine.send(&request, options).await {
        Ok(exchange) => exchange,
        Err(e) if over_limit(&e, limit) => return Err(too_large(limit)),
        Err(e) => return Err(GraphqlError::IntrospectionFailed(e.to_string())),
    };
    if let Some(spill) = &exchange.spill {
        // Not expected under the cap; never read it back.
        let _ = std::fs::remove_file(spill);
        return Err(too_large(limit));
    }
    let redactor = exchange.redactor(&request);
    let failed =
        |reason: String| GraphqlError::IntrospectionFailed(redactor.text(&reason).into_owned());
    let status = exchange.status;
    let body: Value = match serde_json::from_slice(&exchange.body) {
        Ok(body @ Value::Object(_)) => body,
        _ => {
            return Err(failed(format!(
                "HTTP {status}: response is not a GraphQL result"
            )));
        }
    };
    if introspection::schema_object(&body).is_some() {
        return Ok(Attempt::Schema(body));
    }
    let errors = error_messages(&body);
    if errors.is_empty() {
        return Err(failed(format!("HTTP {status}: the response has no schema")));
    }
    Ok(Attempt::Rejected(
        errors
            .iter()
            .map(|m| redactor.text(m).into_owned())
            .collect(),
    ))
}

/// The `message` of every entry of `errors`.
fn error_messages(body: &Value) -> Vec<String> {
    let Some(errors) = body.get("errors").and_then(Value::as_array) else {
        return Vec::new();
    };
    errors
        .iter()
        .map(|e| match e.get("message").and_then(Value::as_str) {
            Some(message) => message.to_owned(),
            None => e.to_string(),
        })
        .collect()
}

/// The POST of `query` to the endpoint of `target`, with the GraphQL
/// `Accept` header unless the request sets one.
fn introspection_request(target: &ResolvedGraphql, query: &str) -> ResolvedRequest {
    let mut request = target.http.clone();
    request.method = "POST".to_owned();
    request.body = ResolvedBody::Json(json!({
        "query": query,
        "operationName": OPERATION_NAME,
    }));
    with_graphql_accept(&mut request);
    request
}
