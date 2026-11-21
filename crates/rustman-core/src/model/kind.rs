//! The request-kind registry and two-pass request loading (D4).
//!
//! Pass one reads only `version` and `kind`. Pass two hands the same text to
//! the loader registered for that kind, which deserializes its own struct with
//! `deny_unknown_fields`.

use std::path::Path;

use serde::Deserialize;

use super::common::Variables;
use super::docs::DocsMeta;
use super::graphql::GraphqlRequest;
use super::grpc::GrpcRequest;
use super::http::HttpRequest;
use super::jsonrpc::JsonrpcRequest;
use super::mcp::McpRequest;
use super::messaging::MessagingRequest;
use super::sse::SseRequest;
use super::ws::WsRequest;
use crate::error::{LoadError, Position};
use crate::format::{Located, from_yaml};

/// The only request-file version this build reads and writes.
pub const REQUEST_VERSION: u64 = 1;

/// The built-in request kinds. Plugins can never register these names
/// (plugins spec, "Request kind extension point").
pub const BUILTIN_KINDS: [&str; 10] = [
    "http", "graphql", "jsonrpc", "grpc", "mcp", "ws", "sse", "kafka", "mqtt", "amqp",
];

/// The keys every request file may have, whatever its kind.
pub const COMMON_FIELDS: [&str; 6] = [
    "version",
    "kind",
    "name",
    "description",
    "tags",
    "variables",
];

/// Turns the text of a request file into a [`Request`].
pub type KindLoader = fn(file: &Path, text: &str) -> Result<Request, LoadError>;

/// A loaded request file.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    Http(Box<HttpRequest>),
    Graphql(Box<GraphqlRequest>),
    Jsonrpc(Box<JsonrpcRequest>),
    Mcp(Box<McpRequest>),
    Grpc(Box<GrpcRequest>),
    Ws(Box<WsRequest>),
    Sse(Box<SseRequest>),
    /// `kind: kafka`, `kind: mqtt` or `kind: amqp`.
    Messaging(Box<MessagingRequest>),
    /// A registered kind this build can list but not execute.
    Opaque(OpaqueRequest),
}

impl Request {
    pub fn kind(&self) -> &str {
        match self {
            Self::Http(r) => &r.kind,
            Self::Graphql(r) => &r.kind,
            Self::Jsonrpc(r) => &r.kind,
            Self::Mcp(r) => &r.kind,
            Self::Grpc(r) => &r.kind,
            Self::Ws(r) => &r.kind,
            Self::Sse(r) => &r.kind,
            Self::Messaging(r) => &r.kind,
            Self::Opaque(r) => &r.kind,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Self::Http(r) => &r.name,
            Self::Graphql(r) => &r.name,
            Self::Jsonrpc(r) => &r.name,
            Self::Mcp(r) => &r.name,
            Self::Grpc(r) => &r.name,
            Self::Ws(r) => &r.name,
            Self::Sse(r) => &r.name,
            Self::Messaging(r) => &r.name,
            Self::Opaque(r) => &r.name,
        }
    }

    pub fn description(&self) -> Option<&str> {
        match self {
            Self::Http(r) => r.description.as_deref(),
            Self::Graphql(r) => r.description.as_deref(),
            Self::Jsonrpc(r) => r.description.as_deref(),
            Self::Mcp(r) => r.description.as_deref(),
            Self::Grpc(r) => r.description.as_deref(),
            Self::Ws(r) => r.description.as_deref(),
            Self::Sse(r) => r.description.as_deref(),
            Self::Messaging(r) => r.description.as_deref(),
            Self::Opaque(r) => r.description.as_deref(),
        }
    }

    pub fn tags(&self) -> &[String] {
        let tags = match self {
            Self::Http(r) => &r.tags,
            Self::Graphql(r) => &r.tags,
            Self::Jsonrpc(r) => &r.tags,
            Self::Mcp(r) => &r.tags,
            Self::Grpc(r) => &r.tags,
            Self::Ws(r) => &r.tags,
            Self::Sse(r) => &r.tags,
            Self::Messaging(r) => &r.tags,
            Self::Opaque(r) => &r.tags,
        };
        tags.as_deref().unwrap_or_default()
    }

    /// The request's `docs:` key, when the file has one.
    pub fn docs(&self) -> Option<&DocsMeta> {
        match self {
            Self::Http(r) => r.docs.as_ref(),
            Self::Graphql(r) => r.docs.as_ref(),
            Self::Jsonrpc(r) => r.docs.as_ref(),
            Self::Mcp(r) => r.docs.as_ref(),
            Self::Grpc(r) => r.docs.as_ref(),
            Self::Ws(r) => r.docs.as_ref(),
            Self::Sse(r) => r.docs.as_ref(),
            Self::Messaging(r) => r.docs.as_ref(),
            Self::Opaque(r) => r.docs.as_ref(),
        }
    }

    /// The request's layer of Rustman variables. A graphql request's
    /// `variables` are GraphQL variables sent in the payload, not a layer.
    pub fn variables(&self) -> Option<&Variables> {
        match self {
            Self::Http(r) => r.variables.as_ref(),
            Self::Graphql(_) => None,
            Self::Jsonrpc(r) => r.variables.as_ref(),
            Self::Mcp(r) => r.variables.as_ref(),
            Self::Grpc(r) => r.variables.as_ref(),
            Self::Ws(r) => r.variables.as_ref(),
            Self::Sse(r) => r.variables.as_ref(),
            Self::Messaging(r) => r.variables.as_ref(),
            Self::Opaque(r) => r.variables.as_ref(),
        }
    }

    /// Why `rustman run` cannot execute the request, if it cannot.
    pub fn unsupported_reason(&self) -> Option<String> {
        let kind = match self {
            Self::Http(_) => return None,
            Self::Graphql(r) => &r.kind,
            Self::Jsonrpc(r) => &r.kind,
            Self::Mcp(r) => &r.kind,
            Self::Grpc(r) => &r.kind,
            Self::Ws(r) => &r.kind,
            Self::Sse(r) => &r.kind,
            Self::Messaging(r) => &r.kind,
            Self::Opaque(r) => &r.kind,
        };
        Some(format!(
            "request kind '{kind}' is not supported by this build"
        ))
    }
}

impl OpaqueRequest {
    /// The request file as JSON (YAML anchors and tags resolved).
    pub fn to_json(&self, file: &Path) -> Result<serde_json::Value, LoadError> {
        from_yaml(file, &self.source)
    }

    /// The fields of its kind: the request file without the common fields.
    pub fn kind_fields(&self, file: &Path) -> Result<serde_json::Value, LoadError> {
        let mut value = self.to_json(file)?;
        if let Some(map) = value.as_object_mut() {
            map.retain(|key, _| !COMMON_FIELDS.contains(&key.as_str()));
        }
        Ok(value)
    }

    /// Where `kind:` is written, for errors about it.
    pub fn kind_position(&self) -> Position {
        let line = self
            .source
            .lines()
            .position(|l| l.trim_start().starts_with("kind:"))
            .unwrap_or(0);
        Position {
            line: line + 1,
            column: 1,
        }
    }
}

/// A request of a kind whose fields this build does not model. Only the common
/// fields are read; saving writes the original text back unchanged.
#[derive(Clone, Debug, PartialEq)]
pub struct OpaqueRequest {
    pub kind: String,
    pub name: String,
    pub description: Option<String>,
    pub tags: Option<Vec<String>>,
    pub docs: Option<DocsMeta>,
    pub variables: Option<Variables>,
    pub source: String,
}

/// Registered request kinds, in registration order.
#[derive(Clone)]
pub struct KindRegistry {
    kinds: Vec<(&'static str, KindLoader)>,
    /// Loads kinds that are not registered, instead of failing.
    fallback: Option<KindLoader>,
}

impl Default for KindRegistry {
    /// The built-in kinds: `http`, `graphql`, `jsonrpc`, `grpc`, `mcp`,
    /// `ws`, `sse`, `kafka`, `mqtt` and `amqp`.
    fn default() -> Self {
        Self {
            kinds: vec![
                (BUILTIN_KINDS[0], load_http),
                ("graphql", load_graphql),
                ("jsonrpc", load_jsonrpc),
                ("grpc", load_grpc),
                ("mcp", load_mcp),
                ("ws", load_ws),
                ("sse", load_sse),
                ("kafka", load_messaging),
                ("mqtt", load_messaging),
                ("amqp", load_messaging),
            ],
            fallback: None,
        }
    }
}

#[derive(Deserialize)]
struct Header {
    version: Option<Located<Version>>,
    kind: Option<Located<String>>,
}

impl KindRegistry {
    /// Register a kind, replacing any loader already registered under `name`.
    pub fn register(&mut self, name: &'static str, loader: KindLoader) {
        match self.kinds.iter_mut().find(|(n, _)| *n == name) {
            Some(entry) => entry.1 = loader,
            None => self.kinds.push((name, loader)),
        }
    }

    /// Load request files of unregistered kinds as [`OpaqueRequest`]s
    /// instead of rejecting them. Front-ends that can show any request
    /// (the TUI's read-only view) use this; the CLI keeps the strict default.
    #[must_use]
    pub fn with_opaque_fallback(mut self) -> Self {
        self.fallback = Some(load_opaque);
        self
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.kinds.iter().map(|(name, _)| *name).collect()
    }

    /// Load the request file `file` whose contents are `text`.
    pub fn load(&self, file: &Path, text: &str) -> Result<Request, LoadError> {
        let header: Header = from_yaml(file, text)?;
        check_version(file, header.version)?;
        let kind = header.kind.ok_or_else(|| {
            let message = format!(
                "missing request kind (expected one of: {})",
                self.names().join(", ")
            );
            LoadError::new(file, Some(Position { line: 1, column: 1 }), message)
        })?;
        let loader = self.loader(kind.get()).or(self.fallback).ok_or_else(|| {
            let message = format!(
                "unknown request kind '{}' (expected one of: {})",
                kind.get(),
                self.names().join(", ")
            );
            LoadError::new(file, kind.position(), message)
        })?;
        loader(file, text)
    }

    fn loader(&self, kind: &str) -> Option<KindLoader> {
        self.kinds
            .iter()
            .find(|(name, _)| *name == kind)
            .map(|(_, loader)| *loader)
    }
}

/// The `version` field: a plain integer. A quoted `'1'` is rejected rather
/// than coerced, so the header reads exactly what the emitter writes.
struct Version(u64);

impl<'de> Deserialize<'de> for Version {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl serde::de::Visitor<'_> for Visitor {
            type Value = Version;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an unquoted integer version")
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Version, E> {
                Ok(Version(v))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Version, E> {
                u64::try_from(v)
                    .map(Version)
                    .map_err(|_| E::invalid_value(serde::de::Unexpected::Signed(v), &self))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

fn check_version(file: &Path, version: Option<Located<Version>>) -> Result<(), LoadError> {
    match version {
        Some(v) if v.get().0 == REQUEST_VERSION => Ok(()),
        Some(v) => Err(LoadError::new(
            file,
            v.position(),
            format!(
                "unsupported request file version {} (expected {REQUEST_VERSION})",
                v.get()
            ),
        )),
        None => Err(LoadError::new(
            file,
            Some(Position { line: 1, column: 1 }),
            format!("missing 'version' (expected {REQUEST_VERSION})"),
        )),
    }
}

fn load_http(file: &Path, text: &str) -> Result<Request, LoadError> {
    let request: HttpRequest = from_yaml(file, text)?;
    Ok(Request::Http(Box::new(request)))
}

fn load_graphql(file: &Path, text: &str) -> Result<Request, LoadError> {
    let request = GraphqlRequest::parse(file, text)?;
    Ok(Request::Graphql(Box::new(request)))
}

fn load_jsonrpc(file: &Path, text: &str) -> Result<Request, LoadError> {
    let request = JsonrpcRequest::parse(file, text)?;
    Ok(Request::Jsonrpc(Box::new(request)))
}

fn load_mcp(file: &Path, text: &str) -> Result<Request, LoadError> {
    let request = McpRequest::parse(file, text)?;
    Ok(Request::Mcp(Box::new(request)))
}

fn load_grpc(file: &Path, text: &str) -> Result<Request, LoadError> {
    let request = GrpcRequest::parse(file, text)?;
    Ok(Request::Grpc(Box::new(request)))
}

fn load_ws(file: &Path, text: &str) -> Result<Request, LoadError> {
    let request = WsRequest::parse(file, text)?;
    Ok(Request::Ws(Box::new(request)))
}

fn load_sse(file: &Path, text: &str) -> Result<Request, LoadError> {
    let request = SseRequest::parse(file, text)?;
    Ok(Request::Sse(Box::new(request)))
}

fn load_messaging(file: &Path, text: &str) -> Result<Request, LoadError> {
    let request = MessagingRequest::parse(file, text)?;
    Ok(Request::Messaging(Box::new(request)))
}

#[derive(Deserialize)]
struct OpaqueHeader {
    kind: String,
    name: String,
    description: Option<String>,
    tags: Option<Vec<String>>,
    docs: Option<DocsMeta>,
    variables: Option<Variables>,
}

/// Loader for reserved kinds: reads the common fields and keeps the source.
pub fn load_opaque(file: &Path, text: &str) -> Result<Request, LoadError> {
    let header: OpaqueHeader = from_yaml(file, text)?;
    Ok(Request::Opaque(OpaqueRequest {
        kind: header.kind,
        name: header.name,
        description: header.description,
        tags: header.tags,
        docs: header.docs,
        variables: header.variables,
        source: text.to_owned(),
    }))
}
