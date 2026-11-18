//! The audit log (spec "Audit log", design D9): one JSON line per tool call
//! in `.rustman/audit/<YYYY-MM-DD>.ndjson`, appended with a single write.
//! The log is only ever appended to, and a call whose line cannot be
//! written fails.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use rustman_core::run::now_rfc3339;
use serde::Serialize;
use serde_json::Value;

use super::error::{ErrorCode, ToolError};

/// The audit folder, under the workspace root.
pub const AUDIT_DIR: &str = ".rustman/audit";

/// How a tool call ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Ok,
    Error,
    Blocked,
    Declined,
}

impl Outcome {
    /// The outcome of a call that failed with `code`.
    pub fn of_error(code: ErrorCode) -> Self {
        match code {
            ErrorCode::Blocked => Self::Blocked,
            ErrorCode::Declined => Self::Declined,
            _ => Self::Error,
        }
    }
}

/// One line of the audit log. Every text in it must already be redacted.
#[derive(Clone, Debug, Serialize)]
pub struct AuditEntry {
    pub timestamp: String,
    pub session: String,
    pub client: String,
    pub tool: String,
    pub arguments: Value,
    pub outcome: Outcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// The audit log of one workspace.
#[derive(Clone, Debug)]
pub struct AuditLog {
    dir: PathBuf,
}

impl AuditLog {
    pub fn for_workspace(root: &Path) -> Self {
        Self {
            dir: root.join(AUDIT_DIR),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The file of the day `timestamp` (RFC 3339, UTC) falls on.
    pub fn file_for(&self, timestamp: &str) -> PathBuf {
        let day = timestamp.get(..10).unwrap_or("unknown-day");
        self.dir.join(format!("{day}.ndjson"))
    }

    /// Open today's file for appending, creating the folder; check this
    /// before running a call, so a call that could not be audited does
    /// not run.
    pub fn open(&self) -> Result<File, ToolError> {
        self.open_for(&now_rfc3339())
    }

    fn open_for(&self, timestamp: &str) -> Result<File, ToolError> {
        fs::create_dir_all(&self.dir).map_err(unavailable)?;
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.file_for(timestamp))
            .map_err(unavailable)
    }

    /// Append `entry` as one line, in one write.
    pub fn append(&self, entry: &AuditEntry) -> Result<(), ToolError> {
        let mut line = serde_json::to_vec(entry)
            .map_err(|e| ToolError::internal(format!("cannot write the audit log: {e}")))?;
        line.push(b'\n');
        let mut file = self.open_for(&entry.timestamp)?;
        file.write_all(&line).map_err(unavailable)
    }
}

fn unavailable(err: std::io::Error) -> ToolError {
    ToolError::internal(format!(
        "the audit log ({AUDIT_DIR}) cannot be written: {err}; the call was refused"
    ))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn entry(outcome: Outcome) -> AuditEntry {
        AuditEntry {
            timestamp: now_rfc3339(),
            session: "s1".into(),
            client: "test-client".into(),
            tool: "send_request".into(),
            arguments: json!({"url": "http://127.0.0.1/"}),
            outcome,
            reason: (outcome == Outcome::Blocked).then(|| "host not allowed".into()),
            method: Some("GET".into()),
            url: Some("http://127.0.0.1/".into()),
        }
    }

    fn lines(log: &AuditLog) -> Vec<Value> {
        let text = fs::read_to_string(log.file_for(&now_rfc3339())).unwrap();
        text.lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn every_outcome_appends_one_line() {
        let dir = tempfile::tempdir().unwrap();
        let log = AuditLog::for_workspace(dir.path());
        let outcomes = [
            Outcome::Ok,
            Outcome::Error,
            Outcome::Blocked,
            Outcome::Declined,
        ];
        for outcome in outcomes {
            log.append(&entry(outcome)).unwrap();
        }
        let lines = lines(&log);
        let written: Vec<&str> = lines
            .iter()
            .map(|l| l["outcome"].as_str().unwrap())
            .collect();
        assert_eq!(written, ["ok", "error", "blocked", "declined"]);
        assert_eq!(lines[2]["tool"], "send_request");
        assert_eq!(lines[2]["reason"], "host not allowed");
        assert_eq!(lines[0]["client"], "test-client");
        assert!(lines[0].get("reason").is_none());
    }

    #[test]
    fn earlier_lines_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let log = AuditLog::for_workspace(dir.path());
        log.append(&entry(Outcome::Ok)).unwrap();
        let again = AuditLog::for_workspace(dir.path());
        again.append(&entry(Outcome::Error)).unwrap();
        assert_eq!(lines(&log).len(), 2);
    }

    #[test]
    fn the_file_is_named_by_day() {
        let log = AuditLog::for_workspace(Path::new("ws"));
        let file = log.file_for("2026-11-18T10:00:00Z");
        assert!(file.ends_with(Path::new(".rustman/audit/2026-11-18.ndjson")));
    }

    #[test]
    fn an_unwritable_log_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        // A file where the folder should be makes it unwritable on every OS.
        fs::create_dir_all(dir.path().join(".rustman")).unwrap();
        fs::write(dir.path().join(AUDIT_DIR), "not a folder").unwrap();
        let log = AuditLog::for_workspace(dir.path());
        let err = log.append(&entry(Outcome::Ok)).unwrap_err();
        assert_eq!(err.code, ErrorCode::Internal);
        assert!(err.message.contains("audit log"), "{}", err.message);
        assert!(log.open().is_err());
    }

    #[test]
    fn outcomes_follow_error_codes() {
        assert_eq!(Outcome::of_error(ErrorCode::Blocked), Outcome::Blocked);
        assert_eq!(Outcome::of_error(ErrorCode::Declined), Outcome::Declined);
        assert_eq!(Outcome::of_error(ErrorCode::NotFound), Outcome::Error);
        assert_eq!(
            Outcome::of_error(ErrorCode::ConfirmationUnavailable),
            Outcome::Error
        );
    }
}
