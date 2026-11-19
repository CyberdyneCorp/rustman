//! Recording proxied exchanges (spec: Record proxied exchanges). Each
//! exchange becomes a one-entry HAR document that goes through the
//! `collection-import` HAR pipeline: the same request names
//! (`get-legacy-report.rman.yaml`), header cleaning, deduplication and
//! secret extraction into the active environment, written under the
//! import lock. Repeated identical exchanges are recorded once.

use std::collections::HashSet;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use rustman_core::model::{KindRegistry, REQUEST_FILE_SUFFIX};
use rustman_core::{SecretBackend, SecretStore, Workspace};
use rustman_import::har::{self, HarFilter};
use rustman_import::ir::{Folder, Item};
use rustman_import::secrets::Existing;
use rustman_import::session::{Planned, import};
use rustman_import::{Format, ImportError, OnConflict, PlanOptions, lock_workspace};
use rustman_webhook::Capture;
use serde_json::{Value, json};

use crate::proxy::kept_headers;

/// Why `--record` is refused.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RecordSetupError {
    #[error("--record needs --proxy-unmatched: only proxied exchanges are recorded")]
    NoProxy,
    #[error("--record {0:?}: the directory must be inside the workspace")]
    Outside(String),
}

/// Where and how proxied exchanges are saved.
pub struct Recorder {
    root: PathBuf,
    /// The folder relative to `root`, with `/` separators.
    dir: String,
    /// The environment that receives extracted secrets; `None` for a new
    /// one named after the folder.
    environment: Option<String>,
    backend: SecretBackend,
    seen: Mutex<HashSet<u64>>,
}

impl std::fmt::Debug for Recorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recorder")
            .field("root", &self.root)
            .field("dir", &self.dir)
            .field("environment", &self.environment)
            .finish_non_exhaustive()
    }
}

impl Recorder {
    /// Record into `dir`, relative to the workspace `root` (with `/`
    /// separators); refused when it leaves the workspace.
    pub fn new(
        root: &Path,
        dir: &str,
        environment: Option<String>,
        backend: SecretBackend,
    ) -> Result<Self, RecordSetupError> {
        let dir = dir.replace('\\', "/").trim_matches('/').to_owned();
        let escapes = dir.split('/').any(|p| p == "..") || Path::new(&dir).is_absolute();
        if dir.is_empty() || dir == "." || escapes {
            return Err(RecordSetupError::Outside(dir));
        }
        Ok(Self {
            root: root.to_path_buf(),
            dir,
            environment,
            backend,
            seen: Mutex::new(HashSet::new()),
        })
    }

    /// Save the exchange of `capture`, sent to `url`, whose response had
    /// `content_type`. Returns the request file written (relative to the
    /// workspace root), or `None` for an exchange recorded before.
    pub fn record(
        &self,
        capture: &Capture,
        url: &str,
        content_type: Option<&str>,
    ) -> Result<Option<String>, ImportError> {
        if !self.first_time(capture, url) {
            return Ok(None);
        }
        let name = self.dir.rsplit('/').next().unwrap_or(&self.dir).to_owned();
        let doc = har_document(capture, url, content_type);
        let filter = HarFilter {
            include_static: true,
            ..HarFilter::default()
        };
        let mut collection = har::parse(&doc.to_string(), &name, &filter)?;
        flatten(&mut collection.root);
        let _lock = lock_workspace(&self.root)?;
        let registry = KindRegistry::default().with_opaque_fallback();
        let ws = Workspace::load_with(&self.root, &registry)
            .map_err(|e| ImportError::invalid(&self.root.display().to_string(), e.to_string()))?;
        let store = SecretStore::open(&ws.root, self.backend);
        let lookup = |env: &str, secret: &str| store.get(env, secret).ok().flatten();
        let options = PlanOptions {
            root: &ws.root,
            source: format!("{} {}", capture.method, capture.path),
            format: Format::Har,
            destination: Some(self.dir.clone()),
            on_conflict: OnConflict::Rename,
            environment: self.environment.clone(),
            existing: Existing {
                environments: &ws.environments,
                lookup: &lookup,
            },
            dry_run: false,
        };
        let Planned { plan, .. } = import(collection, &options, &store)?;
        Ok(plan
            .files
            .iter()
            .find(|f| f.path.ends_with(REQUEST_FILE_SUFFIX) && f.action.writes())
            .map(|f| f.path.clone()))
    }

    /// False for an exchange (method, URL, body) recorded before.
    fn first_time(&self, capture: &Capture, url: &str) -> bool {
        let mut hasher = DefaultHasher::new();
        (&capture.method, url, &capture.body[..]).hash(&mut hasher);
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(hasher.finish())
    }
}

/// A HAR 1.2 document with the one exchange.
fn har_document(capture: &Capture, url: &str, content_type: Option<&str>) -> Value {
    let headers: Vec<Value> = kept_headers(capture)
        .iter()
        .map(|h| json!({"name": h.name, "value": h.value_text()}))
        .collect();
    let mut request = json!({"method": capture.method, "url": url, "headers": headers});
    if !capture.body.is_empty() {
        request["postData"] = json!({
            "mimeType": capture.content_type().unwrap_or_default(),
            "text": String::from_utf8_lossy(&capture.body),
        });
    }
    json!({"log": {"entries": [{
        "request": request,
        "response": {"content": {"mimeType": content_type.unwrap_or_default()}},
    }]}})
}

/// The requests of the per-host folders, directly in `root`.
fn flatten(root: &mut Folder) {
    let items = std::mem::take(&mut root.items);
    for item in items {
        match item {
            Item::Folder(folder) => root.items.extend(folder.items),
            request @ Item::Request(_) => root.items.push(request),
        }
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use super::*;

    #[test]
    fn the_directory_stays_inside_the_workspace() {
        let root = Path::new("ws");
        let ok = Recorder::new(root, "recorded/", None, SecretBackend::File).unwrap();
        assert_eq!(ok.dir, "recorded");
        for bad in ["", ".", "../out", "a/../../b"] {
            assert!(
                Recorder::new(root, bad, None, SecretBackend::File).is_err(),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn the_har_document_holds_the_kept_headers_and_body() {
        let mut headers = http::HeaderMap::new();
        headers.insert("host", "mock".parse().unwrap());
        headers.insert("content-type", "application/json".parse().unwrap());
        let capture = crate::proxy::Outgoing {
            method: "POST",
            path: "/x",
            query: None,
            version: http::Version::HTTP_11,
            headers: &headers,
            body: Bytes::from_static(b"{\"a\":1}"),
        }
        .capture();
        let doc = har_document(&capture, "http://up/x", Some("application/json"));
        let request = &doc["log"]["entries"][0]["request"];
        assert_eq!(request["headers"].as_array().unwrap().len(), 1);
        assert_eq!(request["postData"]["text"], "{\"a\":1}");
    }
}
