//! The canonical report: a run or an exploration under `rustman.report/v1`.
//! It is the only input the renderers and `report diff` receive.

use crate::exploration::{ExplorationReport, KIND_EXPLORATION};
use crate::run::{KIND_RUN, RunReport};

/// The `schema` of every report this version writes.
pub const SCHEMA: &str = "rustman.report/v1";

/// The major schema versions this version reads.
const SUPPORTED_MAJORS: [u32; 1] = [1];

/// A report of either kind.
#[derive(Clone, Debug, PartialEq)]
pub enum Report {
    /// Boxed, like an exploration report, so the enum stays small.
    Run(Box<RunReport>),
    /// Boxed: an exploration report is several times larger than a run's.
    Exploration(Box<ExplorationReport>),
}

/// Why a report cannot be read.
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("not a JSON report: {0}")]
    Json(serde_json::Error),
    #[error("not a Rustman report: no `schema: rustman.report/v1` field")]
    NotAReport,
    #[error("unsupported report schema '{0}' (this version reads rustman.report/v1)")]
    UnsupportedVersion(String),
    #[error("unknown report kind '{0}' (expected run or exploration)")]
    UnknownKind(String),
    #[error("invalid {kind} report: {error}")]
    Invalid {
        kind: &'static str,
        error: serde_json::Error,
    },
}

impl Report {
    /// Read a report from its JSON text, refusing unknown major versions.
    pub fn parse(text: &str) -> Result<Self, ReadError> {
        let value: serde_json::Value = serde_json::from_str(text).map_err(ReadError::Json)?;
        let schema = value
            .get("schema")
            .and_then(serde_json::Value::as_str)
            .ok_or(ReadError::NotAReport)?;
        check_version(schema)?;
        match value.get("kind").and_then(serde_json::Value::as_str) {
            Some(KIND_RUN) => serde_json::from_value(value)
                .map(|r| Self::Run(Box::new(r)))
                .map_err(|error| ReadError::Invalid {
                    kind: KIND_RUN,
                    error,
                }),
            Some(KIND_EXPLORATION) => serde_json::from_value(value)
                .map(|r| Self::Exploration(Box::new(r)))
                .map_err(|error| ReadError::Invalid {
                    kind: KIND_EXPLORATION,
                    error,
                }),
            other => Err(ReadError::UnknownKind(other.unwrap_or("").to_owned())),
        }
    }

    /// `run` or `exploration`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Run(_) => KIND_RUN,
            Self::Exploration(_) => KIND_EXPLORATION,
        }
    }

    /// The report as pretty-printed JSON, ending in a newline.
    pub fn to_json(&self) -> String {
        let text = match self {
            Self::Run(r) => serde_json::to_string_pretty(r),
            Self::Exploration(r) => serde_json::to_string_pretty(r),
        };
        let mut text = text.expect("reports serialize");
        text.push('\n');
        text
    }
}

/// Accept `rustman.report/v<major>[.<minor>]` with a supported major.
fn check_version(schema: &str) -> Result<(), ReadError> {
    let unsupported = || ReadError::UnsupportedVersion(schema.to_owned());
    let version = schema
        .strip_prefix("rustman.report/v")
        .ok_or_else(unsupported)?;
    let major = version.split('.').next().unwrap_or_default();
    match major.parse::<u32>() {
        Ok(m) if SUPPORTED_MAJORS.contains(&m) => Ok(()),
        _ => Err(unsupported()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_checked_by_major() {
        assert!(check_version("rustman.report/v1").is_ok());
        assert!(check_version("rustman.report/v1.3").is_ok());
        for bad in [
            "rustman.report/v9",
            "rustman.report/v2.0",
            "other/v1",
            "rustman.report/vx",
        ] {
            assert!(
                matches!(check_version(bad), Err(ReadError::UnsupportedVersion(s)) if s == bad),
                "{bad}"
            );
        }
    }

    #[test]
    fn unknown_major_versions_and_kinds_are_refused() {
        let v9 = Report::parse(r#"{"schema":"rustman.report/v9","kind":"run"}"#).unwrap_err();
        assert!(v9.to_string().contains("rustman.report/v9"), "{v9}");
        let kind = Report::parse(r#"{"schema":"rustman.report/v1","kind":"bench"}"#).unwrap_err();
        assert!(matches!(kind, ReadError::UnknownKind(k) if k == "bench"));
        assert!(matches!(
            Report::parse(r#"{"kind":"run"}"#),
            Err(ReadError::NotAReport)
        ));
        assert!(matches!(Report::parse("nope"), Err(ReadError::Json(_))));
    }
}
