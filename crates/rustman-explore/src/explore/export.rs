//! `explore export`: one regression request file per finding, written from
//! the report alone, for `rustman run`.

use std::path::{Path, PathBuf};

use super::report::{ExplorationReport, ReportFinding};
use crate::collection::names;

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("no finding '{0}' in this exploration (see report.json for the finding ids)")]
    UnknownFinding(String),
    #[error("finding {0} has no reproduction to export")]
    NoReproduction(String),
    #[error("cannot write {}: {source}", .path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// Which findings to export: `all`, or ids such as `f001,f003`.
pub fn select<'a>(
    report: &'a ExplorationReport,
    selection: &[String],
) -> Result<Vec<&'a ReportFinding>, ExportError> {
    if selection.iter().any(|s| s == "all") {
        return Ok(report.findings.iter().collect());
    }
    selection
        .iter()
        .map(|id| {
            report
                .findings
                .iter()
                .find(|f| f.id == *id)
                .ok_or_else(|| ExportError::UnknownFinding(id.clone()))
        })
        .collect()
}

/// Write each of `findings` as `<id>-<rule>-<operation>.rman.yaml` in
/// `into` (created if needed), replacing earlier exports of the same
/// finding. Returns the files written.
pub fn export(findings: &[&ReportFinding], into: &Path) -> Result<Vec<PathBuf>, ExportError> {
    let io = |path: &Path| {
        let path = path.to_path_buf();
        move |source| ExportError::Io { path, source }
    };
    std::fs::create_dir_all(into).map_err(io(into))?;
    let mut written = Vec::new();
    for finding in findings {
        let reproduction = finding
            .reproduction
            .as_ref()
            .ok_or_else(|| ExportError::NoReproduction(finding.id.clone()))?;
        let path = into.join(file_name(finding));
        std::fs::write(&path, &reproduction.request).map_err(io(&path))?;
        written.push(path);
    }
    Ok(written)
}

fn file_name(finding: &ReportFinding) -> String {
    let stem = names::kebab(&format!(
        "{} {} {}",
        finding.id, finding.rule, finding.operation
    ));
    format!("{stem}.rman.yaml")
}
