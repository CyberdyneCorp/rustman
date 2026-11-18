//! Why an import stopped.

use rustman_http::RequestError;

use super::diagnostic::Diagnostic;

/// The message for a Swagger 2.0 document (openapi-import spec).
pub const SWAGGER_2_MESSAGE: &str =
    "Swagger 2.0 is not supported; convert it to OpenAPI 3 first (e.g. with swagger2openapi)";

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("{SWAGGER_2_MESSAGE}")]
    Swagger2,
    #[error("{0}: not an OpenAPI document (no 'openapi' field)")]
    NotOpenApi(String),
    #[error("OpenAPI {0} is not supported (expected 3.0.x or 3.1.x)")]
    UnsupportedVersion(String),
    /// The text is neither JSON nor YAML.
    #[error("{source_name}: {message}")]
    Parse {
        source_name: String,
        message: String,
    },
    /// A file that cannot be read, or a URL that answered with an error.
    #[error("{0}")]
    Source(String),
    /// The request for a URL failed.
    #[error("cannot fetch {url}: {error}")]
    Fetch { url: String, error: RequestError },
    /// The safety policy refused the fetch of `url` or one of its
    /// redirect hops (`refused`); nothing was sent there.
    #[error("cannot fetch {url}: blocked: {reason} ({refused})")]
    Blocked {
        url: String,
        refused: String,
        reason: String,
    },
    #[error("{name} is larger than the {} limit", size(*.limit))]
    TooLarge { name: String, limit: u64 },
    #[error(
        "remote reference '{reference}' at {at} is not resolved; pass --allow-remote-refs to fetch it"
    )]
    RemoteRef { reference: String, at: String },
    /// A reference that cannot be followed.
    #[error("{0}")]
    Reference(String),
    /// Structural errors; the diagnostics also hold the warnings found.
    #[error("{}", errors(.0))]
    Invalid(Vec<Diagnostic>),
}

impl ImportError {
    /// The warnings found before the import stopped.
    pub fn warnings(&self) -> Vec<&Diagnostic> {
        match self {
            Self::Invalid(diagnostics) => diagnostics.iter().filter(|d| !d.is_error()).collect(),
            _ => Vec::new(),
        }
    }

    /// The request could not be completed (network, TLS, timeout), as
    /// opposed to an invalid source or document.
    pub fn is_request_failure(&self) -> bool {
        matches!(self, Self::Fetch { .. })
    }
}

/// `20 MiB`, or a byte count when the limit is not whole mebibytes.
fn size(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    if bytes >= MIB && bytes.is_multiple_of(MIB) {
        format!("{} MiB", bytes / MIB)
    } else {
        format!("{bytes}-byte")
    }
}

fn errors(diagnostics: &[Diagnostic]) -> String {
    diagnostics
        .iter()
        .filter(|d| d.is_error())
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}
