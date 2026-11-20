//! Stress reports on disk and `rustman stress export` (stress-testing
//! "Regression export"): every run keeps its report as
//! `.rustman/stress/<id>/report.json` (under the workspace's ignored
//! `.rustman/`), and export writes one file per finding from that report
//! alone: the regression request file built with the report (the
//! minimized input and the status a correct API answers), or, for a wire
//! fault, a file of comments, since request files cannot express it.
//! File names follow `explore export`.

use std::path::{Path, PathBuf};

use rustman_explore::collection::names;
use rustman_report::stress::{StressFinding, StressReport};

/// Where stress reports live under a workspace root.
pub const STRESS_DIR: &str = ".rustman/stress";
const REPORT_FILE: &str = "report.json";

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("no stress run '{0}' (its report is kept in .rustman/stress/<id>/report.json)")]
    NotFound(String),
    #[error("{} is not a valid stress report: {message}", .path.display())]
    Invalid { path: PathBuf, message: String },
    #[error("no finding '{0}' in this stress run (see its report for the finding ids)")]
    UnknownFinding(String),
    #[error("finding {0} has no input to export")]
    NoInput(String),
    #[error("cannot write {}: {source}", .path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// A new stress run id, `str-<YYYYMMDD>-<6 hex>`, dated by `now` (RFC 3339).
pub fn new_id(now: &str) -> String {
    let date: String = now.chars().take(10).filter(char::is_ascii_digit).collect();
    let bytes: [u8; 3] = rand::random();
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("str-{date}-{hex}")
}

fn report_path(root: &Path, id: &str) -> PathBuf {
    STRESS_DIR
        .split('/')
        .fold(root.to_path_buf(), |p, s| p.join(s))
        .join(id)
        .join(REPORT_FILE)
}

/// Keep `report` as `.rustman/stress/<id>/report.json` under `root`.
pub fn save(root: &Path, report: &StressReport) -> Result<PathBuf, ExportError> {
    let path = report_path(root, &report.id);
    let io = |source| ExportError::Io {
        path: path.clone(),
        source,
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    let mut text = serde_json::to_string_pretty(report).expect("reports serialize");
    text.push('\n');
    std::fs::write(&path, text).map_err(io)?;
    Ok(path)
}

/// The report of run `id` under `root`.
pub fn load(root: &Path, id: &str) -> Result<StressReport, ExportError> {
    let valid = id.starts_with("str-") && !id.contains(['/', '\\', '.']);
    let path = report_path(root, id);
    if !valid || !path.is_file() {
        return Err(ExportError::NotFound(id.to_owned()));
    }
    let invalid = |message: String| ExportError::Invalid {
        path: path.clone(),
        message,
    };
    let text = std::fs::read_to_string(&path).map_err(|e| invalid(e.to_string()))?;
    serde_json::from_str(&text).map_err(|e| invalid(e.to_string()))
}

/// Which findings to export: `all` (those with an input), or ids such as
/// `f001,f003` (each must have one).
pub fn select<'a>(
    report: &'a StressReport,
    selection: &[String],
) -> Result<Vec<&'a StressFinding>, ExportError> {
    if selection.iter().any(|s| s == "all") {
        return Ok(report
            .findings
            .iter()
            .filter(|f| f.finding.reproduction.is_some())
            .collect());
    }
    selection
        .iter()
        .map(|id| {
            let finding = report
                .findings
                .iter()
                .find(|f| f.finding.id == *id)
                .ok_or_else(|| ExportError::UnknownFinding(id.clone()))?;
            match finding.finding.reproduction {
                Some(_) => Ok(finding),
                None => Err(ExportError::NoInput(id.clone())),
            }
        })
        .collect()
}

/// Write each of `findings` into `into` (created if needed):
/// `<id>-<rule>-<operation>.rman.yaml`, or `.wire.txt` for a wire fault.
/// Returns the files written.
pub fn export(findings: &[&StressFinding], into: &Path) -> Result<Vec<PathBuf>, ExportError> {
    let io = |path: &Path| {
        let path = path.to_path_buf();
        move |source| ExportError::Io { path, source }
    };
    std::fs::create_dir_all(into).map_err(io(into))?;
    let mut written = Vec::new();
    for finding in findings {
        let f = &finding.finding;
        let Some(reproduction) = &f.reproduction else {
            return Err(ExportError::NoInput(f.id.clone()));
        };
        let stem = names::kebab(&format!("{} {} {}", f.id, f.rule, f.operation));
        let wire = f.operation.starts_with("wire ");
        let name = if wire {
            format!("{stem}.wire.txt")
        } else {
            format!("{stem}.rman.yaml")
        };
        let path = into.join(name);
        std::fs::write(&path, &reproduction.request).map_err(io(&path))?;
        written.push(path);
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_dated_and_random() {
        let id = new_id("2026-11-20T10:00:00Z");
        assert!(id.starts_with("str-20261120-"), "{id}");
        assert_eq!(id.len(), "str-20261120-".len() + 6);
    }

    #[test]
    fn ids_cannot_leave_the_stress_folder() {
        let root = tempfile::tempdir().unwrap();
        for bad in ["../x", "str-..", "exp-1", "str-a/b"] {
            assert!(
                matches!(load(root.path(), bad), Err(ExportError::NotFound(_))),
                "{bad}"
            );
        }
    }
}
