//! The canonical report: a run, an exploration, a contract check or a
//! bench under `rustman.report/v1`.
//! It is the only input the renderers and `report diff` receive.

use crate::bench::{BenchReport, KIND_BENCH};
use crate::contract::{ContractReport, KIND_CONTRACT};
use crate::exploration::{ExplorationReport, KIND_EXPLORATION};
use crate::run::{KIND_RUN, RunReport};
use crate::stress::{KIND_STRESS, StressReport};

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
    /// Boxed, like the others.
    Contract(Box<ContractReport>),
    /// Boxed, like the others.
    Bench(Box<BenchReport>),
    /// Boxed, like the others.
    Stress(Box<StressReport>),
}

/// Every kind this version reads, in the order errors list them.
pub const KINDS: [&str; 5] = [
    KIND_RUN,
    KIND_EXPLORATION,
    KIND_CONTRACT,
    KIND_BENCH,
    KIND_STRESS,
];

/// Why a report cannot be read.
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("not a JSON report: {0}")]
    Json(serde_json::Error),
    #[error("not a Rustman report: no `schema: rustman.report/v1` field")]
    NotAReport,
    #[error("unsupported report schema '{0}' (this version reads rustman.report/v1)")]
    UnsupportedVersion(String),
    #[error("unknown report kind '{0}' (expected {expected})", expected = expected(&KINDS))]
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
        Self::parse_known(text, &KINDS)
    }

    /// Read a report as a reader that knows only `known` kinds would: a
    /// newer kind is an [`ReadError::UnknownKind`], never a crash (how an
    /// older reader meets a `stress` report).
    pub fn parse_known(text: &str, known: &[&str]) -> Result<Self, ReadError> {
        let value: serde_json::Value = serde_json::from_str(text).map_err(ReadError::Json)?;
        let schema = value
            .get("schema")
            .and_then(serde_json::Value::as_str)
            .ok_or(ReadError::NotAReport)?;
        check_version(schema)?;
        let kind = value.get("kind").and_then(serde_json::Value::as_str);
        if let Some(kind) = kind.filter(|k| !known.contains(k)) {
            return Err(ReadError::UnknownKind(kind.to_owned()));
        }
        match kind {
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
            Some(KIND_CONTRACT) => serde_json::from_value(value)
                .map(|r| Self::Contract(Box::new(r)))
                .map_err(|error| ReadError::Invalid {
                    kind: KIND_CONTRACT,
                    error,
                }),
            Some(KIND_BENCH) => serde_json::from_value(value)
                .map(|r| Self::Bench(Box::new(r)))
                .map_err(|error| ReadError::Invalid {
                    kind: KIND_BENCH,
                    error,
                }),
            Some(KIND_STRESS) => serde_json::from_value(value)
                .map(|r| Self::Stress(Box::new(r)))
                .map_err(|error| ReadError::Invalid {
                    kind: KIND_STRESS,
                    error,
                }),
            other => Err(ReadError::UnknownKind(other.unwrap_or("").to_owned())),
        }
    }

    /// `run`, `exploration`, `contract`, `bench` or `stress`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Stress(_) => KIND_STRESS,
            Self::Run(_) => KIND_RUN,
            Self::Exploration(_) => KIND_EXPLORATION,
            Self::Contract(_) => KIND_CONTRACT,
            Self::Bench(_) => KIND_BENCH,
        }
    }

    /// The report as pretty-printed JSON, ending in a newline.
    pub fn to_json(&self) -> String {
        let text = match self {
            Self::Run(r) => serde_json::to_string_pretty(r),
            Self::Exploration(r) => serde_json::to_string_pretty(r),
            Self::Contract(r) => serde_json::to_string_pretty(r),
            Self::Bench(r) => serde_json::to_string_pretty(r),
            Self::Stress(r) => serde_json::to_string_pretty(r),
        };
        let mut text = text.expect("reports serialize");
        text.push('\n');
        text
    }
}

/// `run, exploration, contract, bench or stress`.
fn expected(kinds: &[&str]) -> String {
    match kinds.split_last() {
        Some((last, [])) => (*last).to_owned(),
        Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
        None => String::new(),
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
        let kind = Report::parse(r#"{"schema":"rustman.report/v1","kind":"chaos"}"#).unwrap_err();
        assert!(matches!(kind, ReadError::UnknownKind(k) if k == "chaos"));
        // A reader from before `kind: stress` refuses it without crashing.
        let older = ["run", "exploration", "contract", "bench"];
        let stress = r#"{"schema":"rustman.report/v1","kind":"stress"}"#;
        let err = Report::parse_known(stress, &older).unwrap_err();
        assert!(matches!(err, ReadError::UnknownKind(ref k) if k == "stress"));
        assert!(
            err.to_string().starts_with("unknown report kind 'stress'"),
            "{err}"
        );
        assert!(matches!(
            Report::parse(r#"{"kind":"run"}"#),
            Err(ReadError::NotAReport)
        ));
        assert!(matches!(Report::parse("nope"), Err(ReadError::Json(_))));
    }
}
