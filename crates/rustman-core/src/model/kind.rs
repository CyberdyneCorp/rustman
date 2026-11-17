//! The request-kind registry and two-pass request loading (D4).
//!
//! Pass one reads only `version` and `kind`. Pass two hands the same text to
//! the loader registered for that kind, which deserializes its own struct with
//! `deny_unknown_fields`.

use std::path::Path;

use serde::Deserialize;

use super::common::Variables;
use super::http::HttpRequest;
use crate::error::{LoadError, Position};
use crate::format::{Located, from_yaml};

/// The only request-file version this build reads and writes.
pub const REQUEST_VERSION: u64 = 1;

/// Turns the text of a request file into a [`Request`].
pub type KindLoader = fn(file: &Path, text: &str) -> Result<Request, LoadError>;

/// A loaded request file.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    Http(Box<HttpRequest>),
    /// A registered kind this build can list but not execute (e.g. `graphql`, `mcp`).
    Opaque(OpaqueRequest),
}

impl Request {
    pub fn kind(&self) -> &str {
        match self {
            Self::Http(r) => &r.kind,
            Self::Opaque(r) => &r.kind,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Self::Http(r) => &r.name,
            Self::Opaque(r) => &r.name,
        }
    }

    pub fn description(&self) -> Option<&str> {
        match self {
            Self::Http(r) => r.description.as_deref(),
            Self::Opaque(r) => r.description.as_deref(),
        }
    }

    pub fn tags(&self) -> &[String] {
        let tags = match self {
            Self::Http(r) => &r.tags,
            Self::Opaque(r) => &r.tags,
        };
        tags.as_deref().unwrap_or_default()
    }

    pub fn variables(&self) -> Option<&Variables> {
        match self {
            Self::Http(r) => r.variables.as_ref(),
            Self::Opaque(r) => r.variables.as_ref(),
        }
    }

    /// Why this build cannot execute the request, if it cannot.
    pub fn unsupported_reason(&self) -> Option<String> {
        match self {
            Self::Http(_) => None,
            Self::Opaque(r) => Some(format!(
                "request kind '{}' is not supported by this build",
                r.kind
            )),
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
    pub variables: Option<Variables>,
    pub source: String,
}

/// Registered request kinds, in registration order.
#[derive(Clone)]
pub struct KindRegistry {
    kinds: Vec<(&'static str, KindLoader)>,
}

impl Default for KindRegistry {
    /// The kinds registered by this change: `http`, plus the reserved `graphql` and `mcp`.
    fn default() -> Self {
        Self {
            kinds: vec![
                ("http", load_http),
                ("graphql", load_opaque),
                ("mcp", load_opaque),
            ],
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
        let loader = self.loader(kind.get()).ok_or_else(|| {
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

#[derive(Deserialize)]
struct OpaqueHeader {
    kind: String,
    name: String,
    description: Option<String>,
    tags: Option<Vec<String>>,
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
        variables: header.variables,
        source: text.to_owned(),
    }))
}
