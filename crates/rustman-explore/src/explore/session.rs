//! One exploration on disk: `.rustman/explorations/<id>/` holds
//! `state.json` (what `explore status` shows and `explore resume` needs),
//! `plan.json`, `journal.ndjson` and, once it completes, `report.json`.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use ring::rand::{SecureRandom, SystemRandom};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::graph::Graph;
use super::journal::JOURNAL_FILE;
use super::plan::Plan;
use super::rules::SeverityCounts;

/// Where explorations live, relative to the workspace root.
pub const EXPLORATIONS_DIR: &str = ".rustman/explorations";
pub const STATE_FILE: &str = "state.json";
pub const PLAN_FILE: &str = "plan.json";
pub const REPORT_FILE: &str = "report.json";

/// The four phases, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Discover,
    Plan,
    Execute,
    Report,
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Discover => "discover",
            Self::Plan => "plan",
            Self::Execute => "execute",
            Self::Report => "report",
        })
    }
}

/// Whether an exploration is still going.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunState {
    Running,
    /// Stopped by Ctrl+C; `explore resume` continues it.
    Interrupted,
    Completed,
}

impl fmt::Display for RunState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Running => "running",
            Self::Interrupted => "interrupted",
            Self::Completed => "completed",
        })
    }
}

pub use rustman_report::exploration::Settings;

/// How far execution got.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    /// Probes finished (passed, found something, blocked, skipped or not run).
    pub done: usize,
    pub total: usize,
    /// Distinct findings (per rule and operation) by severity.
    pub findings: SeverityCounts,
}

/// `state.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub id: String,
    pub phase: Phase,
    pub state: RunState,
    /// RFC 3339.
    pub started_at: String,
    pub started_unix_ms: u64,
    pub updated_at: String,
    /// Time spent executing, over every run and resume.
    #[serde(default)]
    pub executed_ms: u64,
    /// `--spec` as given.
    pub spec: String,
    /// SHA-256 of the document text; a resume refuses another document.
    pub document_sha256: String,
    pub target: String,
    #[serde(default)]
    pub environment: Option<String>,
    #[serde(default)]
    pub identity_b: Option<String>,
    pub settings: Settings,
    #[serde(default)]
    pub progress: Progress,
    /// The resource graph discovery found.
    #[serde(default)]
    pub graph: Graph,
}

/// Why an exploration cannot be read.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("no exploration '{0}' (see `rustman explore status`)")]
    NotFound(String),
    #[error("no exploration yet (start one with `rustman explore run`)")]
    None,
    #[error("cannot read or write {}: {source}", .path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("{} is not valid: {message}", .path.display())]
    Invalid { path: PathBuf, message: String },
}

/// An exploration's directory and state.
#[derive(Clone, Debug)]
pub struct Session {
    dir: PathBuf,
    pub state: State,
}

impl Session {
    /// Create the directory of `state.id` under `root` with its state, its
    /// plan and an empty journal.
    pub fn create(root: &Path, state: State, plan: &Plan) -> Result<Self, SessionError> {
        let dir = explorations_dir(root).join(&state.id);
        std::fs::create_dir_all(&dir).map_err(io_error(&dir))?;
        let session = Self { dir, state };
        session.write_json(PLAN_FILE, plan)?;
        let journal = session.journal_path();
        std::fs::write(&journal, "").map_err(io_error(&journal))?;
        session.save()?;
        Ok(session)
    }

    /// The exploration `id` under `root`.
    pub fn open(root: &Path, id: &str) -> Result<Self, SessionError> {
        let valid = id.starts_with("exp-") && !id.contains(['/', '\\', '.']);
        let dir = explorations_dir(root).join(id);
        if !valid || !dir.join(STATE_FILE).is_file() {
            return Err(SessionError::NotFound(id.to_owned()));
        }
        let state = read_json(&dir.join(STATE_FILE))?;
        Ok(Self { dir, state })
    }

    /// The most recently started exploration under `root`.
    pub fn latest(root: &Path) -> Result<Self, SessionError> {
        let dir = explorations_dir(root);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(SessionError::None),
            Err(e) => {
                return Err(SessionError::Io {
                    path: dir,
                    source: e,
                });
            }
        };
        entries
            .filter_map(Result::ok)
            .filter_map(|e| Self::open(root, &e.file_name().to_string_lossy()).ok())
            .max_by_key(|s| (s.state.started_unix_ms, s.state.id.clone()))
            .ok_or(SessionError::None)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn journal_path(&self) -> PathBuf {
        self.dir.join(JOURNAL_FILE)
    }

    pub fn report_path(&self) -> PathBuf {
        self.dir.join(REPORT_FILE)
    }

    pub fn plan(&self) -> Result<Plan, SessionError> {
        read_json(&self.dir.join(PLAN_FILE))
    }

    /// Write `state.json` atomically.
    pub fn save(&self) -> Result<(), SessionError> {
        self.write_json(STATE_FILE, &self.state)
    }

    /// Move to `phase` and save.
    pub fn enter(&mut self, phase: Phase) -> Result<(), SessionError> {
        self.state.phase = phase;
        self.touch();
        self.save()
    }

    /// Stamp `updated_at` with the current time.
    pub fn touch(&mut self) {
        self.state.updated_at = rustman_core::run::now_rfc3339();
    }

    /// Write `value` as `name` in the directory, replacing it atomically.
    pub fn write_json<T: Serialize>(&self, name: &str, value: &T) -> Result<(), SessionError> {
        let path = self.dir.join(name);
        let mut text = serde_json::to_string_pretty(value).map_err(|e| SessionError::Invalid {
            path: path.clone(),
            message: e.to_string(),
        })?;
        text.push('\n');
        let staging = self.dir.join(format!(".{name}.tmp"));
        std::fs::write(&staging, text).map_err(io_error(&staging))?;
        std::fs::rename(&staging, &path).map_err(io_error(&path))
    }
}

/// `.rustman/explorations` under `root`.
pub fn explorations_dir(root: &Path) -> PathBuf {
    EXPLORATIONS_DIR
        .split('/')
        .fold(root.to_path_buf(), |p, s| p.join(s))
}

/// A new exploration id, `exp-<YYYYMMDD>-<6 hex>`, dated by `now` (RFC 3339).
pub fn new_id(now: &str) -> String {
    let date: String = now.chars().take(10).filter(char::is_ascii_digit).collect();
    let mut bytes = [0u8; 3];
    if SystemRandom::new().fill(&mut bytes).is_err() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        bytes.copy_from_slice(&nanos.to_le_bytes()[..3]);
    }
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("exp-{date}-{hex}")
}

/// Milliseconds since the Unix epoch.
pub fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, SessionError> {
    let text = std::fs::read_to_string(path).map_err(io_error(path))?;
    serde_json::from_str(&text).map_err(|e| SessionError::Invalid {
        path: path.to_path_buf(),
        message: e.to_string(),
    })
}

fn io_error(path: &Path) -> impl FnOnce(io::Error) -> SessionError {
    let path = path.to_path_buf();
    move |source| SessionError::Io { path, source }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explore::plan::Family;
    use crate::explore::rules::Severity;

    fn state(id: &str, started: u64) -> State {
        State {
            id: id.into(),
            phase: Phase::Discover,
            state: RunState::Running,
            started_at: "2026-11-18T10:00:00Z".into(),
            started_unix_ms: started,
            updated_at: "2026-11-18T10:00:00Z".into(),
            executed_ms: 0,
            spec: "api.yaml".into(),
            document_sha256: "abc".into(),
            target: "http://127.0.0.1:1".into(),
            environment: None,
            identity_b: None,
            settings: Settings {
                probes: vec![Family::Conformance],
                operations: Vec::new(),
                seed: 1,
                fuzz_cases: 2,
                allow_mutations: false,
                allow_destructive: false,
                allow_hosts: Vec::new(),
                production: None,
                allow_remote_refs: false,
                rate: 10,
                concurrency: 4,
                max_probes: 2000,
                max_duration_ms: 900_000,
                fail_on: Severity::High,
            },
            progress: Progress::default(),
            graph: Graph::default(),
        }
    }

    #[test]
    fn ids_have_the_date_and_six_hex_digits() {
        let id = new_id("2026-11-18T10:00:00.000Z");
        assert!(id.starts_with("exp-20261118-"), "{id}");
        let hex = id.rsplit('-').next().unwrap();
        assert_eq!(hex.len(), 6);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn sessions_are_created_opened_and_the_latest_found() {
        let root = tempfile::tempdir().unwrap();
        assert!(matches!(
            Session::latest(root.path()),
            Err(SessionError::None)
        ));
        let plan = Plan {
            seed: 1,
            probes: Vec::new(),
            scans: Vec::new(),
        };
        Session::create(root.path(), state("exp-20261118-000001", 1), &plan).unwrap();
        let mut second =
            Session::create(root.path(), state("exp-20261118-000000", 2), &plan).unwrap();
        second.enter(Phase::Execute).unwrap();
        assert!(second.journal_path().is_file());
        let latest = Session::latest(root.path()).unwrap();
        assert_eq!(latest.state.id, "exp-20261118-000000");
        assert_eq!(latest.state.phase, Phase::Execute);
        assert_eq!(latest.plan().unwrap(), plan);
        assert!(matches!(
            Session::open(root.path(), "exp-nope"),
            Err(SessionError::NotFound(_))
        ));
        assert!(matches!(
            Session::open(root.path(), "../x"),
            Err(SessionError::NotFound(_))
        ));
    }
}
