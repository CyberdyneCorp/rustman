//! Reading an OpenAPI document into the [`ApiModel`](model::ApiModel) IR:
//! load (file or URL), detect format and version, resolve references,
//! rewrite 3.0 schemas to 3.1, validate, lower.

pub mod bundle;
pub mod diagnostic;
pub mod document;
pub mod error;
pub mod lower;
pub mod model;
pub mod normalize;
pub mod pointer;
pub mod schema;
pub mod source;
pub mod validate;
pub mod walk;

use serde_json::Value;

pub use diagnostic::{Diagnostic, Severity};
pub use document::{Format, SpecVersion};
pub use error::ImportError;
pub use source::{FetchOptions, Location};

use bundle::BundleOptions;
use model::ApiModel;

/// How a document is read.
#[derive(Clone, Debug, Default)]
pub struct ImportOptions {
    /// Fetch `$ref`s to other `http(s)` documents instead of refusing them.
    pub allow_remote_refs: bool,
    pub fetch: FetchOptions,
}

/// A document read, validated and lowered.
#[derive(Clone, Debug)]
pub struct Imported {
    pub location: Location,
    pub format: Format,
    pub version: SpecVersion,
    /// The exact text read, for hashing and snapshots.
    pub text: String,
    /// The self-contained, 3.1-shaped document the model was lowered from.
    pub document: Value,
    pub model: ApiModel,
    pub warnings: Vec<Diagnostic>,
}

/// Read `source` (a path or an `http(s)` URL) into an [`Imported`].
pub async fn import(source: &str, options: &ImportOptions) -> Result<Imported, ImportError> {
    let location = Location::parse(source);
    let text = source::read(&location, &options.fetch).await?;
    let (format, raw) = document::parse(&text, &location.to_string())?;
    let version = document::detect_version(&raw, &location.to_string())?;
    let bundle_options = BundleOptions {
        allow_remote_refs: options.allow_remote_refs,
        version,
        fetch: options.fetch.clone(),
    };
    let document = bundle::bundle(&location, raw, &bundle_options).await?;
    let (model, warnings) = check_and_lower(&document, version)?;
    Ok(Imported {
        location,
        format,
        version,
        text,
        document,
        model,
        warnings,
    })
}

/// Validate a bundled document and lower it. Structural errors (from either
/// step) stop the import; warnings are returned with the model.
pub fn check_and_lower(
    document: &Value,
    version: SpecVersion,
) -> Result<(ApiModel, Vec<Diagnostic>), ImportError> {
    let mut diagnostics = validate::validate(document, version);
    if diagnostics.iter().any(Diagnostic::is_error) {
        return Err(ImportError::Invalid(diagnostics));
    }
    match lower::lower(document) {
        Ok(model) => Ok((model, diagnostics)),
        Err(errors) => {
            diagnostics.extend(errors);
            Err(ImportError::Invalid(diagnostics))
        }
    }
}
