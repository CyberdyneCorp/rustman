//! Real examples (api-docs-generation "Real examples are opt-in"): the
//! latest successful exchange of an opted-in request from
//! `request-history`, or its stored snapshot with `--examples snapshot`.
//! Bodies pass `[docs].example_redact_paths`, then the redactor, and are
//! cut at [`MAX_EXAMPLE_BYTES`].

use std::collections::BTreeMap;
use std::path::Path;

use rustman_core::history::{Entry, HistoryStore};
use rustman_core::model::DEFAULT_SNAPSHOT_NAME;
use rustman_core::secret::placeholder;
use rustman_core::snapshot::{parse as parse_snapshot, snapshot_path};
use rustman_core::{RequestRef, Workspace};
use serde_json::Value;
use serde_json_path::JsonPath;

use crate::error::DocsError;
use crate::model::{Example, Row};
use crate::rendered::{DocsRedactor, Rendered};

/// Example bodies longer than this are cut, with a note.
pub const MAX_EXAMPLE_BYTES: usize = 64 * 1024;

/// Recorded response headers left out of examples: they say nothing about
/// the API.
const NOISY_HEADERS: [&str; 6] = [
    "connection",
    "content-length",
    "date",
    "keep-alive",
    "server",
    "transfer-encoding",
];

/// Where examples come from (`--examples`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExampleSource {
    #[default]
    History,
    Snapshot,
}

/// The examples a build may publish.
pub struct Examples {
    source: ExampleSource,
    /// Latest successful history entry per request file (`/` separators).
    latest: BTreeMap<String, Entry>,
    redact_paths: Vec<(String, JsonPath)>,
    opted_in: Vec<String>,
}

impl Examples {
    /// Read what `source` needs from `workspace`: its history, or nothing
    /// (snapshots are read per request).
    pub fn load(workspace: &Workspace, source: ExampleSource) -> Result<Self, DocsError> {
        let settings = &workspace.settings.docs;
        let redact_paths = settings
            .example_redact_paths
            .iter()
            .map(|p| parse_path(p).map(|path| (p.clone(), path)))
            .collect::<Result<_, _>>()?;
        let latest = match source {
            ExampleSource::History => latest_successful(workspace),
            ExampleSource::Snapshot => BTreeMap::new(),
        };
        Ok(Self {
            source,
            latest,
            redact_paths,
            opted_in: settings.examples.clone(),
        })
    }

    /// Whether `request` opted in: `docs: {example: true}` in its file, or
    /// its id or name in `[docs].examples`.
    pub fn opted_in(&self, request: &RequestRef<'_>) -> bool {
        let entry = request.entry;
        let flagged = entry.request.docs().is_some_and(|d| d.wants_example());
        let id = entry.id();
        flagged
            || self
                .opted_in
                .iter()
                .any(|o| *o == id || o == entry.request.name())
    }

    /// The example of `request`, when it opted in and one was recorded.
    pub fn example(
        &self,
        root: &Path,
        request: &RequestRef<'_>,
        redactor: &DocsRedactor,
    ) -> Option<Example> {
        if !self.opted_in(request) {
            return None;
        }
        match self.source {
            ExampleSource::History => {
                let entry = self.latest.get(&request_path(request))?;
                Some(self.history_example(entry, redactor))
            }
            ExampleSource::Snapshot => self.snapshot_example(root, request, redactor),
        }
    }

    fn history_example(&self, entry: &Entry, redactor: &DocsRedactor) -> Example {
        let response = entry.record.pointer("/exchange/response");
        let headers = response
            .and_then(|r| r.get("headers"))
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|h| Some((h.get("name")?.as_str()?, h.get("value")?.as_str()?)))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let ShownBody {
            body,
            json: body_json,
            truncated,
        } = self.body(entry.response_body(), redactor);
        let date = entry.timestamp.get(..10).unwrap_or(&entry.timestamp);
        Example {
            source: "history",
            status: entry.status,
            recorded: Some(redactor.render(date)),
            headers: header_rows(headers, redactor),
            body,
            body_json,
            truncated,
        }
    }

    fn snapshot_example(
        &self,
        root: &Path,
        request: &RequestRef<'_>,
        redactor: &DocsRedactor,
    ) -> Option<Example> {
        let path = snapshot_path(&root.join(&request.entry.path), DEFAULT_SNAPSHOT_NAME);
        let text = std::fs::read_to_string(&path).ok()?;
        let document = parse_snapshot(&path, &text).ok()?;
        let status = document.get("status").and_then(Value::as_u64);
        let headers: Vec<(&str, &str)> = document
            .get("headers")
            .and_then(Value::as_object)
            .map(|map| {
                map.iter()
                    .filter_map(|(k, v)| Some((k.as_str(), v.as_str()?)))
                    .collect()
            })
            .unwrap_or_default();
        let body = document.get("body");
        let text = match (
            body.and_then(|b| b.get("json")),
            body.and_then(|b| b.get("text")),
        ) {
            (Some(json), _) => Some(json.to_string()),
            (None, Some(Value::String(text))) => Some(text.clone()),
            _ => None,
        };
        let ShownBody {
            body,
            json: body_json,
            truncated,
        } = self.body(text.as_deref(), redactor);
        Some(Example {
            source: "snapshot",
            status: status.and_then(|s| u16::try_from(s).ok()),
            recorded: None,
            headers: header_rows(headers, redactor),
            body,
            body_json,
            truncated,
        })
    }

    /// The body as shown: JSON pretty-printed with the redact paths
    /// masked, then redacted and cut.
    fn body(&self, body: Option<&str>, redactor: &DocsRedactor) -> ShownBody {
        let Some(body) = body.filter(|b| !b.is_empty()) else {
            return ShownBody::default();
        };
        let (rendered, json) = match serde_json::from_str::<Value>(body) {
            Ok(mut json) => {
                self.mask_paths(&mut json);
                (redactor.json(&json), true)
            }
            Err(_) => (redactor.render(body), false),
        };
        let (cut, truncated) = rendered.truncated(MAX_EXAMPLE_BYTES);
        ShownBody {
            body: Some(cut),
            json,
            truncated,
        }
    }

    fn mask_paths(&self, json: &mut Value) {
        for (text, path) in &self.redact_paths {
            let pointers: Vec<String> = path
                .query_located(json)
                .locations()
                .map(|l| l.to_json_pointer())
                .collect();
            for pointer in pointers {
                if let Some(value) = json.pointer_mut(&pointer) {
                    *value = Value::String(placeholder(text));
                }
            }
        }
    }
}

fn parse_path(text: &str) -> Result<JsonPath, DocsError> {
    JsonPath::parse(text)
        .map_err(|e| DocsError::Usage(format!("[docs].example_redact_paths: '{text}': {e}")))
}

/// The request file of `request` as history records it.
fn request_path(request: &RequestRef<'_>) -> String {
    let parts: Vec<String> = request
        .entry
        .path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    parts.join("/")
}

/// The newest exchange per request file that got a 2xx response without
/// error. History that cannot be read means no examples.
fn latest_successful(workspace: &Workspace) -> BTreeMap<String, Entry> {
    let store = HistoryStore::for_workspace(&workspace.root, &workspace.settings.history);
    let mut latest = BTreeMap::new();
    let Ok(listing) = store.list() else {
        return latest;
    };
    for entry in listing.entries {
        let ok = entry.error.is_none() && entry.status.is_some_and(|s| (200..300).contains(&s));
        if ok && !latest.contains_key(&entry.request) {
            latest.insert(entry.request.clone(), entry);
        }
    }
    latest
}

fn header_rows(headers: Vec<(&str, &str)>, redactor: &DocsRedactor) -> Vec<Row> {
    headers
        .into_iter()
        .filter(|(name, _)| !NOISY_HEADERS.contains(&name.to_ascii_lowercase().as_str()))
        .map(|(name, value)| Row {
            name: redactor.render(name),
            value: redactor.recorded_header(name, value),
            description: None,
        })
        .collect()
}

/// An example body as published.
#[derive(Default)]
struct ShownBody {
    body: Option<Rendered>,
    /// The recorded body was JSON.
    json: bool,
    truncated: bool,
}
