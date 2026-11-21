//! AsyncAPI 2.6 and 3.0 documents (messaging-requests "AsyncAPI import",
//! "Payload schema validation"): read from a file or URL (JSON or YAML),
//! bundled with the `openapi-import` reference resolver (local refs;
//! remote refs only when allowed), and normalized into one model.

mod import;
mod spec;
mod validate;

use std::path::Path;

use rustman_explore::openapi::bundle::{self, BundleOptions};
use rustman_explore::openapi::{
    FetchOptions, ImportError, Location, SpecVersion, document, source,
};
use serde_json::Value;

pub use import::{GeneratedFile, ImportPlan, WriteOutcome, plan, write};
pub use spec::{AsyncApi, Bindings, Message, Op, Server, Version, protocol_kind};
pub use validate::{
    ASSERTION_KIND, Validator, consumed_violations, schema_assertion, validate_produce,
};

/// Why a document cannot be used.
#[derive(Debug, thiserror::Error)]
pub enum AsyncApiError {
    /// The source cannot be read, parsed or bundled.
    #[error(transparent)]
    Import(#[from] ImportError),
    #[error("asyncapi: {0} is not an AsyncAPI document (no 'asyncapi' field)")]
    NotAsyncApi(String),
    #[error("asyncapi: unsupported version {0} (supported: 2.6, 3.0)")]
    UnsupportedVersion(String),
    #[error("asyncapi: {0}")]
    Invalid(String),
    #[error("asyncapi: no message '{0}' in the document")]
    UnknownMessage(String),
    #[error("asyncapi: cannot write {path}: {message}")]
    Write { path: String, message: String },
}

impl AsyncApiError {
    /// The `cli-runner` exit code: 3 when fetching the document failed,
    /// else 2.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Import(e) if e.is_request_failure() => 3,
            _ => 2,
        }
    }
}

/// How a document is read.
#[derive(Clone, Debug, Default)]
pub struct LoadOptions {
    pub allow_remote_refs: bool,
    pub fetch: FetchOptions,
}

/// A bundled document and its model.
#[derive(Clone, Debug)]
pub struct Document {
    pub location: String,
    /// The self-contained document: every `$ref` points into
    /// `#/components`.
    pub bundled: Value,
    pub model: AsyncApi,
}

impl Document {
    /// The message named `name`.
    pub fn message(&self, name: &str) -> Result<&Message, AsyncApiError> {
        self.model
            .messages
            .iter()
            .find(|m| m.name == name)
            .ok_or_else(|| AsyncApiError::UnknownMessage(name.to_owned()))
    }
}

/// Read `source` (a path or an `http(s)` URL).
pub async fn load(source: &str, options: &LoadOptions) -> Result<Document, AsyncApiError> {
    let location = Location::parse(source);
    let text = source::read(&location, &options.fetch).await?;
    from_text(&text, &location, options).await
}

/// Read the document file at `path`.
pub async fn load_file(path: &Path) -> Result<Document, AsyncApiError> {
    let location = Location::File(path.to_path_buf());
    let text = source::read(&location, &FetchOptions::default()).await?;
    from_text(&text, &location, &LoadOptions::default()).await
}

async fn from_text(
    text: &str,
    location: &Location,
    options: &LoadOptions,
) -> Result<Document, AsyncApiError> {
    let shown = location.to_string();
    let (_, mut raw) = document::parse(text, &shown)?;
    let version = spec::version(&raw, &shown)?;
    spec::mark_references(&mut raw);
    let bundle_options = BundleOptions {
        allow_remote_refs: options.allow_remote_refs,
        version: SpecVersion::V3_1,
        fetch: options.fetch.clone(),
    };
    let bundled = bundle::bundle(location, raw, &bundle_options).await?;
    let model = spec::normalize(&bundled, version)?;
    Ok(Document {
        location: shown,
        bundled,
        model,
    })
}
