//! Resolving a `kind: graphql` request before it is sent.
//!
//! `{{name}}` is interpolated in the endpoint, headers (inherited and own),
//! auth fields, and the string values of `variables` and
//! `connection_params`. The document is never interpolated: GraphQL
//! variables are the supported mechanism. Every missing name is reported at
//! once, and nothing can be sent unless resolution succeeds.

use std::fmt;

use serde_json::{Map, Value};

use super::request::{
    ResolvedBody, ResolvedRequest, TlsFiles, auth, contain_files, contain_jwt_key,
    inherit_client_identity, pairs,
};
use super::resolver::{Interpolator, ResolveError, Resolver};
use crate::model::{GraphqlRequest, Request, RequestSettings};
use crate::redact::Redactor;
use crate::workspace::RequestRef;

/// A graphql request with every placeholder replaced.
///
/// `http` carries the endpoint, headers, auth and workspace client identity
/// with no body; the transport adds the method and payload. Its `secrets`
/// hold every secret used anywhere in the request, `variables` included, so
/// [`Redactor::for_request`] on it covers the whole operation. The `Debug`
/// output is redacted.
#[derive(Clone, PartialEq)]
pub struct ResolvedGraphql {
    pub http: ResolvedRequest,
    pub operation_name: Option<String>,
    /// The GraphQL variables, string values interpolated.
    pub variables: Map<String, Value>,
    /// The `connection_init` payload, string values interpolated.
    pub connection_params: Option<Map<String, Value>>,
}

impl ResolvedGraphql {
    /// The redactor for everything shown or recorded about this request.
    pub fn redactor(&self) -> Redactor {
        Redactor::for_request(&self.http)
    }
}

impl fmt::Debug for ResolvedGraphql {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let raw = format!(
            "ResolvedGraphql {{ http: {:?}, operation_name: {:?}, variables: {:?}, connection_params: {:?} }}",
            self.http, self.operation_name, self.variables, self.connection_params
        );
        f.write_str(&self.redactor().text(&raw))
    }
}

/// Resolve the graphql request `request` through `resolver`.
pub fn resolve_graphql(
    request: &RequestRef<'_>,
    resolver: &Resolver<'_>,
) -> Result<ResolvedGraphql, ResolveError> {
    let Request::Graphql(graphql) = request.request() else {
        let kind = request.request().kind();
        return Err(ResolveError::Unsupported(format!(
            "request kind '{kind}' is not a graphql request"
        )));
    };
    let mut i = resolver.interpolator();
    let mut settings = RequestSettings::default();
    let mut tls = TlsFiles::default();
    inherit_client_identity(&mut i, request, &mut settings, &mut tls)?;
    let http = ResolvedRequest {
        method: "POST".to_owned(),
        url: i.text(&graphql.endpoint),
        query: Vec::new(),
        headers: pairs(&mut i, &request.effective_headers()),
        body: ResolvedBody::None,
        auth: request.effective_auth().map(|a| auth(&mut i, a)),
        settings,
        tls,
        secrets: Vec::new(),
    };
    let payload = Payload::of(&mut i, graphql);
    let ((mut http, payload), secrets) = i.finish((http, payload))?;
    http.secrets = secrets;
    contain_files(request, &mut http)?;
    contain_jwt_key(request, &mut http)?;
    Ok(ResolvedGraphql {
        http,
        operation_name: graphql.operation_name.clone(),
        variables: payload.variables,
        connection_params: payload.connection_params,
    })
}

/// The interpolated JSON parts of a graphql request.
struct Payload {
    variables: Map<String, Value>,
    connection_params: Option<Map<String, Value>>,
}

impl Payload {
    fn of(i: &mut Interpolator<'_, '_>, graphql: &GraphqlRequest) -> Self {
        Self {
            variables: graphql
                .variables
                .as_ref()
                .map(|v| object(i, v))
                .unwrap_or_default(),
            connection_params: graphql.connection_params.as_ref().map(|p| object(i, p)),
        }
    }
}

fn object(i: &mut Interpolator<'_, '_>, map: &Map<String, Value>) -> Map<String, Value> {
    map.iter()
        .map(|(key, value)| (key.clone(), string_values(i, value)))
        .collect()
}

/// `value` with every string interpolated; keys and other scalars are kept.
fn string_values(i: &mut Interpolator<'_, '_>, value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(i.text(text).text),
        Value::Array(items) => Value::Array(items.iter().map(|v| string_values(i, v)).collect()),
        Value::Object(map) => Value::Object(object(i, map)),
        other => other.clone(),
    }
}
