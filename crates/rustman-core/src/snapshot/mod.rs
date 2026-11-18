//! Snapshot testing (snapshot-testing spec): normalize an exchange, store it
//! under `__snapshots__/`, and compare later runs against it.
//!
//! Every request kind maps its response onto an [`Exchange`] (status,
//! headers, body bytes, and the event list of a subscription), so the
//! normalizer, the store and the differ are the same for `http`, `graphql`,
//! `jsonrpc` and `mcp`. [`check`] runs one snapshot assertion in a
//! [`SnapshotMode`] and says what happened.

mod diff;
mod file;
mod inventory;
mod normalize;
mod presets;
mod review;
mod run;
mod store;

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::SnapshotAssertion;
use crate::redact::Redactor;

pub use diff::{BinaryChange, Change, ChangeKind, MAX_CHANGES, Pair, SnapshotDiff, diff_documents};
pub use file::{
    SNAPSHOT_DIR, SNAPSHOT_SUFFIX, SNAPSHOT_VERSION, SnapshotMeta, display_path, document, parse,
    request_stem, snapshot_file_name, snapshot_path, to_yaml,
};
pub use inventory::{EntryState, SnapshotEntry, inventory, prune, snapshot_names};
pub use normalize::{
    IGNORED, MAX_STORED_BODY, Normalized, NormalizedBody, Normalizer, REDACTED_HEADER, canonical,
    is_textual, sha256_hex,
};
pub use presets::{DATE, EPOCH_MS, ETAG, JWT, TIMESTAMP, UUID};
pub use review::SnapshotReview;
pub use run::{
    CI_ENV, CONFLICTING_FLAGS, CheckedSnapshots, SnapshotCounts, SnapshotResult, assertion_result,
    check_all, is_ci_value,
};
pub use store::{SnapshotLock, WriteResult, read_snapshot, same_text, write_snapshot};

/// A response as snapshots see it, whatever the request kind.
#[derive(Clone, Copy, Debug, Default)]
pub struct Exchange<'a> {
    /// The request kind: `http`, `graphql`, `jsonrpc`, `mcp`...
    pub kind: &'a str,
    pub status: Option<u16>,
    /// Header names and values in the order received; names may repeat.
    pub headers: &'a [(String, String)],
    pub body: &'a [u8],
    /// The bounded event list of a subscription; when set it is the body.
    pub events: Option<&'a [Value]>,
}

impl Exchange<'_> {
    /// The first `Content-Type` header.
    pub fn content_type(&self) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case("content-type"))
            .map(|(_, v)| v.as_str())
    }
}

/// How a run treats snapshot files.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SnapshotMode {
    /// Record missing snapshots; compare existing ones.
    #[default]
    Record,
    /// Also overwrite differing snapshots (`--update-snapshots`).
    Update,
    /// Never write (`--ci`, `CI=true`); a missing snapshot fails.
    Ci,
}

/// How a snapshot assertion ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SnapshotState {
    Matched,
    /// Written on a first run; counts as passed.
    Recorded,
    /// Overwritten in update mode; counts as passed.
    Updated,
    Failed,
    /// No snapshot in CI mode.
    Missing,
}

impl SnapshotState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Matched => "matched",
            Self::Recorded => "recorded",
            Self::Updated => "updated",
            Self::Failed => "failed",
            Self::Missing => "missing",
        }
    }

    pub fn passed(self) -> bool {
        matches!(self, Self::Matched | Self::Recorded | Self::Updated)
    }
}

/// What [`check`] found.
#[derive(Clone, Debug, PartialEq)]
pub struct SnapshotOutcome {
    pub name: String,
    pub state: SnapshotState,
    /// The snapshot file.
    pub path: PathBuf,
    /// The snapshot path relative to the workspace, with `/`.
    pub display_path: String,
    /// The differences from the stored snapshot (failed or updated).
    pub diff: Option<SnapshotDiff>,
    /// The new snapshot's text, for an approve flow.
    pub received: String,
    /// The stored text it was compared against (`None`: there was no file).
    pub stored: Option<String>,
    /// Why it failed: the diff text, or `snapshot missing: ...`.
    pub message: String,
    pub warnings: Vec<String>,
}

/// Why a snapshot could not be checked.
#[derive(Debug)]
pub enum SnapshotError {
    /// A rule could not be compiled (checked again at load time).
    Rule(String),
    Io {
        path: String,
        error: std::io::Error,
    },
    /// The stored file is not a readable snapshot.
    Invalid(String),
    /// The normalized snapshot still held a secret value; nothing was written.
    Secret(String),
    /// Approving a snapshot in CI mode, which never writes.
    CiMode,
    /// Another writer changed the file after it was read; it was left as is.
    Changed(String),
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rule(message) | Self::Invalid(message) => f.write_str(message),
            Self::Io { path, error } => write!(f, "{path}: {error}"),
            Self::Secret(path) => write!(
                f,
                "{path}: the snapshot would contain a secret value; it was not written"
            ),
            Self::CiMode => f.write_str("snapshot approval is disabled in CI mode"),
            Self::Changed(path) => write!(
                f,
                "{path}: the snapshot file changed on disk since it was compared; it was not written (run again)"
            ),
        }
    }
}

impl std::error::Error for SnapshotError {}

/// What [`check`] needs besides the assertion and the exchange.
pub struct SnapshotContext<'a> {
    /// The request file on disk.
    pub request_file: &'a Path,
    /// The request file relative to the workspace (any separator).
    pub request: &'a str,
    pub mode: SnapshotMode,
    /// Redacts secrets before anything is stored or compared.
    pub redactor: &'a Redactor,
}

/// Check one snapshot assertion against an exchange: record, compare or
/// update its file according to the mode.
pub fn check(
    spec: &SnapshotAssertion,
    exchange: &Exchange<'_>,
    ctx: &SnapshotContext<'_>,
) -> Result<SnapshotOutcome, SnapshotError> {
    let name = spec.name();
    let normalized = Normalizer::new(spec, ctx.redactor)?.normalize(exchange);
    let compare = spec.compared();
    let meta = SnapshotMeta {
        request: ctx.request,
        name,
        kind: exchange.kind,
        compare: &compare,
    };
    let received = to_yaml(&document(&meta, &normalized));
    let shown = display_path(ctx.request, name);
    if ctx.redactor.finds_secret(&received) {
        return Err(SnapshotError::Secret(shown));
    }
    let path = snapshot_path(ctx.request_file, name);
    let outcome = SnapshotOutcome {
        name: name.to_owned(),
        state: SnapshotState::Matched,
        path,
        display_path: shown,
        diff: None,
        received,
        stored: None,
        message: String::new(),
        warnings: normalized.warnings,
    };
    let io = |error| SnapshotError::Io {
        path: display_path(ctx.request, name),
        error,
    };
    match read_snapshot(&outcome.path).map_err(io)? {
        None => record(outcome, ctx.mode),
        Some(stored) => compare_stored(outcome, stored, ctx.mode),
    }
}

/// No stored snapshot: write it, or fail in CI mode.
fn record(
    mut outcome: SnapshotOutcome,
    mode: SnapshotMode,
) -> Result<SnapshotOutcome, SnapshotError> {
    if mode == SnapshotMode::Ci {
        outcome.state = SnapshotState::Missing;
        outcome.message = format!(
            "snapshot missing: {} (run without --ci to record)",
            outcome.display_path
        );
        return Ok(outcome);
    }
    write(&outcome)?;
    outcome.state = SnapshotState::Recorded;
    Ok(outcome)
}

/// A stored snapshot: compare, and overwrite a differing one in update mode.
fn compare_stored(
    mut outcome: SnapshotOutcome,
    stored: String,
    mode: SnapshotMode,
) -> Result<SnapshotOutcome, SnapshotError> {
    let invalid = |e: crate::error::LoadError| SnapshotError::Invalid(e.to_string());
    // Errors name the workspace-relative path, like every other message.
    let shown = Path::new(&outcome.display_path);
    let new = parse(shown, &outcome.received).map_err(invalid)?;
    // An unreadable stored file is replaced in update mode.
    let parsed = parse(shown, &stored);
    outcome.stored = Some(stored);
    let old = match parsed {
        Ok(old) => Some(old),
        Err(_) if mode == SnapshotMode::Update => None,
        Err(e) => return Err(invalid(e)),
    };
    if old.as_ref() == Some(&new) {
        return Ok(outcome);
    }
    let diff = old
        .map(|old| diff_documents(&old, &new))
        .unwrap_or_default();
    if mode == SnapshotMode::Update {
        write(&outcome)?;
        outcome.state = SnapshotState::Updated;
    } else {
        outcome.state = SnapshotState::Failed;
        outcome.message = failure_message(&diff);
    }
    outcome.diff = Some(diff);
    Ok(outcome)
}

/// The failure text: the diff, or a note when only file details differ.
fn failure_message(diff: &SnapshotDiff) -> String {
    if diff.is_empty() {
        return "snapshot differs from the stored file".to_owned();
    }
    diff.to_string()
}

/// Write the received text, only over the file this check read (or where
/// there was none): a file another writer changed meanwhile is kept.
fn write(outcome: &SnapshotOutcome) -> Result<WriteResult, SnapshotError> {
    write_expecting(
        &outcome.path,
        &outcome.display_path,
        outcome.stored.as_deref(),
        &outcome.received,
    )
}

/// [`write_snapshot`] with its errors and conflicts as [`SnapshotError`]s.
pub(crate) fn write_expecting(
    path: &Path,
    shown: &str,
    expected: Option<&str>,
    text: &str,
) -> Result<WriteResult, SnapshotError> {
    let result = write_snapshot(path, expected, text).map_err(|error| SnapshotError::Io {
        path: shown.to_owned(),
        error,
    })?;
    if result == WriteResult::Conflict {
        return Err(SnapshotError::Changed(shown.to_owned()));
    }
    Ok(result)
}

#[cfg(test)]
mod tests;
