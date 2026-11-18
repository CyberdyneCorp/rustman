//! Where documents come from: a local file or an `http(s)` URL, read with a
//! size cap. URLs go through the `rustman-http` engine with TLS verification
//! on, or through a [`GovernedSender`] when the caller must hold every
//! fetch (and every redirect hop) to a safety policy.

use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rustman_core::model::RequestSettings;
use rustman_core::vars::{Resolved, ResolvedBody, ResolvedRequest, TlsFiles};
use rustman_http::{GovernedError, GovernedSender, HttpEngine, RequestError, SendOptions};
use url::Url;

use super::error::ImportError;

/// The largest document (or referenced file) read: 20 MiB.
pub const MAX_DOCUMENT_BYTES: u64 = 20 * 1024 * 1024;

/// How long fetching one URL may take.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// A document's location; relative references are resolved against it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Location {
    File(PathBuf),
    Url(Url),
}

impl Location {
    /// A source argument: an `http(s)` URL, otherwise a path.
    pub fn parse(source: &str) -> Self {
        match Url::parse(source) {
            Ok(url) if is_http(&url) => Self::Url(url),
            _ => Self::File(PathBuf::from(source)),
        }
    }

    /// The location of `reference` (the part of a `$ref` before `#`) relative
    /// to this one.
    pub fn join(&self, reference: &str) -> Result<Self, ImportError> {
        if let Ok(url) = Url::parse(reference) {
            return if is_http(&url) {
                Ok(Self::Url(url))
            } else if url.scheme().len() > 1 {
                Err(ImportError::Reference(format!(
                    "unsupported reference '{reference}' (only relative paths and http(s) URLs)"
                )))
            } else {
                Ok(Self::File(PathBuf::from(reference)))
            };
        }
        match self {
            Self::Url(base) => base.join(reference).map(Self::Url).map_err(|e| {
                ImportError::Reference(format!("invalid reference '{reference}': {e}"))
            }),
            Self::File(base) => {
                let dir = base.parent().unwrap_or(Path::new(""));
                Ok(Self::File(normalize(&dir.join(reference))))
            }
        }
    }

    pub fn is_remote(&self) -> bool {
        matches!(self, Self::Url(_))
    }

    /// The same location in a canonical form, so one file read through two
    /// relative paths is loaded once.
    pub fn canonical(&self) -> Self {
        match self {
            Self::File(path) => {
                Self::File(dunce::canonicalize(path).unwrap_or_else(|_| path.clone()))
            }
            Self::Url(url) => Self::Url(url.clone()),
        }
    }

    /// The last path segment, for naming things after the document.
    pub fn file_name(&self) -> String {
        let name = match self {
            Self::File(path) => path.file_name().map(|n| n.to_string_lossy().into_owned()),
            Self::Url(url) => url
                .path_segments()
                .and_then(|mut s| s.next_back().map(str::to_owned)),
        };
        name.filter(|n| !n.is_empty())
            .unwrap_or_else(|| "document".to_owned())
    }
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File(path) => write!(f, "{}", path.display()),
            Self::Url(url) => f.write_str(&shown(url)),
        }
    }
}

/// `url` as shown in messages: without any `user:password@`.
pub fn shown(url: &Url) -> String {
    let mut url = url.clone();
    let _ = url.set_password(None);
    let _ = url.set_username("");
    url.to_string()
}

fn is_http(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
}

/// `path` with `.` and `a/..` segments removed, without touching the disk.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir if out.file_name().is_some() => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// How a document is fetched.
#[derive(Clone, Debug)]
pub struct FetchOptions {
    pub timeout: Duration,
    pub max_bytes: u64,
    /// Fetch through this sender: its policy checks each URL and every
    /// redirect hop, and its governor paces them. Without one, the plain
    /// engine follows redirects anywhere.
    pub sender: Option<Arc<GovernedSender>>,
}

impl Default for FetchOptions {
    fn default() -> Self {
        Self {
            timeout: FETCH_TIMEOUT,
            max_bytes: MAX_DOCUMENT_BYTES,
            sender: None,
        }
    }
}

/// The text at `location`.
pub async fn read(location: &Location, options: &FetchOptions) -> Result<String, ImportError> {
    match location {
        Location::File(path) => read_file(path, options.max_bytes),
        Location::Url(url) => fetch(url, options).await,
    }
}

fn read_file(path: &Path, max_bytes: u64) -> Result<String, ImportError> {
    let shown = path.display();
    let file = std::fs::File::open(path)
        .map_err(|e| ImportError::Source(format!("cannot read {shown}: {e}")))?;
    let mut bytes = Vec::new();
    file.take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| ImportError::Source(format!("cannot read {shown}: {e}")))?;
    if bytes.len() as u64 > max_bytes {
        return Err(ImportError::TooLarge {
            name: shown.to_string(),
            limit: max_bytes,
        });
    }
    utf8(bytes, &shown.to_string())
}

fn utf8(bytes: Vec<u8>, name: &str) -> Result<String, ImportError> {
    String::from_utf8(bytes)
        .map_err(|_| ImportError::Source(format!("{name} is not valid UTF-8 text")))
}

async fn fetch(url: &Url, options: &FetchOptions) -> Result<String, ImportError> {
    let request = ResolvedRequest {
        method: "GET".into(),
        url: Resolved::literal(url.as_str()),
        query: Vec::new(),
        headers: Vec::new(),
        body: ResolvedBody::None,
        auth: None,
        settings: RequestSettings::default(),
        tls: TlsFiles::default(),
        secrets: Vec::new(),
    };
    let send = send_options(options);
    let exchange = match &options.sender {
        Some(sender) => sender
            .send(&request, &send)
            .await
            .map_err(|error| governed_error(url, options, error))?,
        None => HttpEngine::new()
            .send(&request, &send)
            .await
            .map_err(|error| fetch_error(url, options, error))?,
    };
    if !(200..300).contains(&exchange.status) {
        return Err(ImportError::Source(format!(
            "cannot fetch {}: the server answered {}",
            shown(url),
            exchange.status
        )));
    }
    utf8(exchange.body.to_vec(), &shown(url))
}

/// The engine's options: TLS verification on, redirects followed, the
/// timeout, and a body cap that stops the download (never spilling it).
fn send_options(options: &FetchOptions) -> SendOptions {
    let mut send = SendOptions {
        timeout: options.timeout,
        ..SendOptions::default()
    };
    send.tls.verify = true;
    send.body.max_download = options.max_bytes;
    send.body.max_decoded = options.max_bytes;
    send.body.max_in_memory = options.max_bytes + 1;
    send
}

fn governed_error(url: &Url, options: &FetchOptions, error: GovernedError) -> ImportError {
    match error {
        GovernedError::Blocked {
            reason,
            url: refused,
        } => ImportError::Blocked {
            url: shown(url),
            refused,
            reason: reason.reason().to_owned(),
        },
        GovernedError::NotRun(budget) => {
            ImportError::Source(format!("cannot fetch {}: {budget}", shown(url)))
        }
        GovernedError::Request(error) => fetch_error(url, options, error),
    }
}

fn fetch_error(url: &Url, options: &FetchOptions, error: RequestError) -> ImportError {
    let limit = format!("larger than {} bytes", options.max_bytes);
    match &error {
        RequestError::Network(message) if message.contains(&limit) => ImportError::TooLarge {
            name: shown(url),
            limit: options.max_bytes,
        },
        _ => ImportError::Fetch {
            url: shown(url),
            error,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_are_urls_or_paths() {
        assert!(Location::parse("https://api.example.com/openapi.json").is_remote());
        assert!(!Location::parse("petstore.yaml").is_remote());
        assert!(!Location::parse("C:\\specs\\api.yaml").is_remote());
    }

    #[test]
    fn references_join_relative_to_their_document() {
        let base = Location::File(PathBuf::from("specs/api/root.yaml"));
        assert_eq!(
            base.join("../common/pet.yaml").unwrap(),
            Location::File(PathBuf::from("specs/common/pet.yaml"))
        );
        let base = Location::parse("https://h.example/specs/root.yaml");
        assert_eq!(
            base.join("pet.yaml").unwrap(),
            Location::parse("https://h.example/specs/pet.yaml")
        );
        assert!(base.join("ftp://x/y").is_err());
        assert!(
            Location::File(PathBuf::new())
                .join("https://h.example/a.json")
                .unwrap()
                .is_remote()
        );
    }

    #[test]
    fn credentials_in_urls_are_never_shown() {
        let location = Location::parse("https://ana:canary-pw@h.example/api.yaml?v=1");
        assert_eq!(location.to_string(), "https://h.example/api.yaml?v=1");
    }

    #[test]
    fn the_limits_are_the_spec_values() {
        assert_eq!(MAX_DOCUMENT_BYTES, 20 * 1024 * 1024);
        assert_eq!(FETCH_TIMEOUT, Duration::from_secs(30));
        let send = send_options(&FetchOptions::default());
        assert!(send.tls.verify);
        assert_eq!(send.timeout, FETCH_TIMEOUT);
        assert!(send.body.max_in_memory > send.body.max_download);
    }

    #[test]
    fn oversized_files_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.yaml");
        std::fs::write(&path, "a".repeat(11)).unwrap();
        let err = read_file(&path, 10).unwrap_err();
        assert!(
            err.to_string()
                .ends_with("is larger than the 10-byte limit"),
            "{err}"
        );
        let err = ImportError::TooLarge {
            name: "x".into(),
            limit: MAX_DOCUMENT_BYTES,
        };
        assert_eq!(err.to_string(), "x is larger than the 20 MiB limit");
        assert_eq!(read_file(&path, 11).unwrap().len(), 11);
    }
}
