//! Resolving a `kind: grpc` request before it is sent.
//!
//! `{{name}}` is interpolated in the target, metadata (names and values),
//! TLS settings and paths, schema paths and the string values of the
//! messages (keys are kept as written). Every missing name is reported at
//! once, and nothing can be sent unless resolution succeeds.

use std::fmt;

use serde_json::{Map, Value};

use super::request::{
    ResolvedBody, ResolvedFile, ResolvedRequest, contain_files, inherit_client_identity, pairs,
    settings, tls_files,
};
use super::resolver::{Interpolator, ResolveError, Resolver};
use crate::format::Located;
use crate::model::{GrpcSchema, GrpcTransport, Request, StreamLimits};
use crate::paths::PathEscape;
use crate::redact::Redactor;
use crate::run::{HeaderRecord, SentRequest, redact_value};
use crate::workspace::RequestRef;

/// The `method` a recorded gRPC exchange shows.
pub const GRPC_SENT_METHOD: &str = "GRPC";

/// The schema files of a request, contained in the workspace.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResolvedSchema {
    pub protos: Vec<ResolvedFile>,
    pub import_paths: Vec<ResolvedFile>,
    pub descriptor_set: Option<ResolvedFile>,
}

impl ResolvedSchema {
    /// No local schema: types come from server reflection.
    pub fn is_reflection(&self) -> bool {
        self.protos.is_empty() && self.descriptor_set.is_none()
    }
}

/// A grpc request with every placeholder replaced.
///
/// `request` carries the target as `url`, the metadata as `headers` and
/// the TLS options as `settings` and `tls` files, so the shared TLS builder
/// and [`Redactor::for_request`] apply unchanged. Its `secrets` hold every
/// secret used anywhere in the request, messages included. The `Debug`
/// output is redacted.
#[derive(Clone, PartialEq)]
pub struct ResolvedGrpc {
    pub request: ResolvedRequest,
    /// `package.Service/Method`.
    pub method: String,
    /// The messages to send, in order.
    pub messages: Vec<Value>,
    /// The file listed `messages` (client or bidi streaming).
    pub message_list: bool,
    pub deadline_ms: Option<u64>,
    /// `tls.enabled` as written; the target's scheme and port decide
    /// otherwise.
    pub tls_enabled: Option<bool>,
    pub transport: GrpcTransport,
    pub schema: ResolvedSchema,
    pub stream: StreamLimits,
}

impl ResolvedGrpc {
    /// The redactor for everything shown or recorded about this request.
    pub fn redactor(&self) -> Redactor {
        Redactor::for_request(&self.request)
    }

    /// The target, after interpolation.
    pub fn target(&self) -> &str {
        &self.request.url.text
    }

    /// The request as history and reports record it: the target and method
    /// as the URL, metadata values with each secret replaced by its
    /// placeholder (`Bearer «redacted:token»`), and the messages as JSON.
    pub fn sent(&self, redactor: &Redactor) -> SentRequest {
        let headers = self
            .request
            .headers
            .iter()
            .map(|p| HeaderRecord {
                name: redactor.resolved(&p.name),
                value: redactor.resolved(&p.value),
            })
            .collect();
        let messages: Vec<Value> = self
            .messages
            .iter()
            .map(|m| redact_value(m, redactor))
            .collect();
        let body = match (self.message_list, messages.as_slice()) {
            (false, [single]) => single.to_string(),
            _ => Value::Array(messages).to_string(),
        };
        let target = redactor.resolved(&self.request.url);
        SentRequest {
            method: GRPC_SENT_METHOD.to_owned(),
            url: format!("{}/{}", target.trim_end_matches('/'), self.method),
            headers,
            body: Some(body),
        }
    }
}

impl fmt::Debug for ResolvedGrpc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let raw = format!(
            "ResolvedGrpc {{ request: {:?}, method: {:?}, messages: {:?}, deadline_ms: {:?}, tls_enabled: {:?}, transport: {:?}, schema: {:?}, stream: {:?} }}",
            self.request,
            self.method,
            self.messages,
            self.deadline_ms,
            self.tls_enabled,
            self.transport,
            self.schema,
            self.stream
        );
        f.write_str(&self.redactor().text(&raw))
    }
}

/// Resolve the grpc request `request` through `resolver`.
pub fn resolve_grpc(
    request: &RequestRef<'_>,
    resolver: &Resolver<'_>,
) -> Result<ResolvedGrpc, ResolveError> {
    let Request::Grpc(grpc) = request.request() else {
        let kind = request.request().kind();
        return Err(ResolveError::Unsupported(format!(
            "request kind '{kind}' is not a grpc request"
        )));
    };
    let mut i = resolver.interpolator();
    let mut settings = settings(&mut i, grpc.http_settings().as_ref());
    let mut tls = tls_files(&settings);
    inherit_client_identity(&mut i, request, &mut settings, &mut tls)?;
    let resolved = ResolvedRequest {
        method: "POST".to_owned(),
        url: i.text(&grpc.target),
        query: Vec::new(),
        headers: pairs(&mut i, grpc.metadata.as_deref().unwrap_or_default()),
        body: ResolvedBody::None,
        auth: None,
        settings,
        tls,
        secrets: Vec::new(),
    };
    let messages: Vec<Value> = grpc
        .message_list()
        .into_iter()
        .map(|m| string_values(&mut i, m))
        .collect();
    let schema = schema_paths(&mut i, grpc.schema.as_ref());
    let ((mut resolved, messages, schema), secrets) = i.finish((resolved, messages, schema))?;
    resolved.secrets = secrets;
    contain_files(request, &mut resolved)?;
    let schema = contain_schema(request, &resolved, schema)?;
    Ok(ResolvedGrpc {
        request: resolved,
        method: grpc.method.clone(),
        messages,
        message_list: grpc.has_message_list(),
        deadline_ms: grpc.deadline_ms,
        tls_enabled: grpc.tls.as_ref().and_then(|t| t.enabled),
        transport: grpc.effective_transport(),
        schema,
        stream: grpc.effective_stream(),
    })
}

/// The schema paths, interpolated but not yet contained.
fn schema_paths(i: &mut Interpolator<'_, '_>, schema: Option<&GrpcSchema>) -> SchemaPaths {
    let Some(schema) = schema else {
        return SchemaPaths::default();
    };
    let mut list = |paths: &Option<Vec<Located<String>>>| -> Vec<String> {
        paths
            .iter()
            .flatten()
            .map(|p| i.text(p.get()).text)
            .collect()
    };
    let protos = list(&schema.protos);
    let import_paths = list(&schema.import_paths);
    let descriptor_set = schema.descriptor_set.as_ref().map(|p| i.text(p.get()).text);
    SchemaPaths {
        protos,
        import_paths,
        descriptor_set,
    }
}

#[derive(Default)]
struct SchemaPaths {
    protos: Vec<String>,
    import_paths: Vec<String>,
    descriptor_set: Option<String>,
}

/// Resolve the schema paths against the request file's folder, keeping
/// them inside the workspace. The error shows the path redacted.
fn contain_schema(
    request: &RequestRef<'_>,
    resolved: &ResolvedRequest,
    paths: SchemaPaths,
) -> Result<ResolvedSchema, ResolveError> {
    let contain = |raw: String| -> Result<ResolvedFile, ResolveError> {
        match request.contain_file(&raw) {
            Ok(path) => Ok(ResolvedFile { raw, path }),
            Err(PathEscape(path)) => {
                let shown = Redactor::for_request(resolved).text(&path).into_owned();
                Err(PathEscape(shown).into())
            }
        }
    };
    let all = |raws: Vec<String>| raws.into_iter().map(contain).collect::<Result<Vec<_>, _>>();
    Ok(ResolvedSchema {
        protos: all(paths.protos)?,
        import_paths: all(paths.import_paths)?,
        descriptor_set: paths.descriptor_set.map(contain).transpose()?,
    })
}

/// `value` with every string interpolated; keys and other scalars are
/// kept as written.
fn string_values(i: &mut Interpolator<'_, '_>, value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(i.text(text).text),
        Value::Array(items) => Value::Array(items.iter().map(|v| string_values(i, v)).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), string_values(i, v)))
                .collect::<Map<String, Value>>(),
        ),
        other => other.clone(),
    }
}
