//! Snapshot assertions in a run (snapshot-testing "Recording on first run",
//! "Update mode", "CI mode refuses writes", "Snapshot results in reports").
//!
//! [`crate::assertions::evaluate`] cannot read or write files, so it leaves
//! a placeholder for every `snapshot` assertion; the runner then calls
//! [`check_all`], which replaces each placeholder with the result of
//! [`super::check`] in the run's [`SnapshotMode`].

use serde::{Deserialize, Serialize};

use super::{
    Exchange, SnapshotContext, SnapshotDiff, SnapshotError, SnapshotMode, SnapshotOutcome,
    SnapshotReview, SnapshotState, check,
};
use crate::assertions::{AssertionResult, Outcome};
use crate::model::Assertion;
use crate::redact::Redactor;
use crate::run::redact_value;

/// The usage error of `--ci` (or `CI=true`) with `--update-snapshots`.
pub const CONFLICTING_FLAGS: &str = "--update-snapshots cannot be used in CI mode";

/// The environment variable that turns CI mode on (`true` or `1`).
pub const CI_ENV: &str = "CI";

impl SnapshotMode {
    /// The mode of a run from `--update-snapshots`, `--ci` and the value of
    /// the `CI` variable. Updating in CI mode is [`CONFLICTING_FLAGS`].
    pub fn detect(update: bool, ci_flag: bool, ci_env: Option<&str>) -> Result<Self, &'static str> {
        let ci = ci_flag || is_ci_value(ci_env);
        match (update, ci) {
            (true, true) => Err(CONFLICTING_FLAGS),
            (true, false) => Ok(Self::Update),
            (false, true) => Ok(Self::Ci),
            (false, false) => Ok(Self::Record),
        }
    }
}

/// Whether a `CI` variable value turns CI mode on: `true` (any case) or `1`.
pub fn is_ci_value(value: Option<&str>) -> bool {
    value.is_some_and(|v| {
        let v = v.trim();
        v == "1" || v.eq_ignore_ascii_case("true")
    })
}

/// A snapshot assertion's outcome in reports (an additive field of
/// `rustman.report/v1`): every text is redacted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotResult {
    pub name: String,
    pub state: SnapshotState,
    /// The snapshot file, relative to the workspace, with `/`.
    pub path: String,
    /// The differences from the stored snapshot (failed or updated).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<SnapshotDiff>,
}

impl SnapshotResult {
    /// One line for reports, such as
    /// `snapshot default recorded: users/__snapshots__/get-user.default.snap.yaml`.
    pub fn note(&self) -> String {
        format!(
            "snapshot {} {}: {}",
            self.name,
            self.state.as_str(),
            self.path
        )
    }

    /// Whether reports mention it beside the failures: a file was written.
    pub fn wrote(&self) -> bool {
        matches!(self.state, SnapshotState::Recorded | SnapshotState::Updated)
    }
}

/// How many snapshot assertions of a run ended in each state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotCounts {
    pub matched: usize,
    pub recorded: usize,
    pub updated: usize,
    pub failed: usize,
    pub missing: usize,
}

impl SnapshotCounts {
    /// The counts of `results`.
    pub fn of<'a>(results: impl IntoIterator<Item = &'a SnapshotResult>) -> Self {
        let mut counts = Self::default();
        for result in results {
            *counts.slot(result.state) += 1;
        }
        counts
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    fn slot(&mut self, state: SnapshotState) -> &mut usize {
        match state {
            SnapshotState::Matched => &mut self.matched,
            SnapshotState::Recorded => &mut self.recorded,
            SnapshotState::Updated => &mut self.updated,
            SnapshotState::Failed => &mut self.failed,
            SnapshotState::Missing => &mut self.missing,
        }
    }

    /// The summary line of a run, such as `2 snapshots matched, 1 snapshot
    /// recorded`; `None` when the run had no snapshot assertion.
    pub fn line(&self) -> Option<String> {
        let states = [
            (self.matched, SnapshotState::Matched),
            (self.recorded, SnapshotState::Recorded),
            (self.updated, SnapshotState::Updated),
            (self.failed, SnapshotState::Failed),
            (self.missing, SnapshotState::Missing),
        ];
        let parts: Vec<String> = states
            .iter()
            .filter(|(count, _)| *count > 0)
            .map(|(count, state)| {
                let noun = if *count == 1 { "snapshot" } else { "snapshots" };
                format!("{count} {noun} {}", state.as_str())
            })
            .collect();
        (!parts.is_empty()).then(|| parts.join(", "))
    }
}

/// What [`check_all`] found besides the assertion results.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CheckedSnapshots {
    /// The warnings to report, such as a body too large to store (redacted).
    pub warnings: Vec<String>,
    /// The failed, missing and new snapshots, for a review after the send.
    pub reviews: Vec<SnapshotReview>,
}

/// Check every `snapshot` assertion of `assertions` and store its result at
/// the same index of `results` (the output of
/// [`crate::assertions::evaluate`]). Every text is redacted with
/// `ctx.redactor`.
pub fn check_all(
    assertions: &[Assertion],
    results: &mut [AssertionResult],
    exchange: &Exchange<'_>,
    ctx: &SnapshotContext<'_>,
) -> CheckedSnapshots {
    let mut checked_all = CheckedSnapshots::default();
    for (assertion, slot) in assertions.iter().zip(results.iter_mut()) {
        let Assertion::Snapshot(spec) = assertion else {
            continue;
        };
        let checked = check(spec, exchange, ctx);
        if let Ok(outcome) = &checked {
            let warnings = outcome.warnings.iter().map(|w| redact(w, ctx.redactor));
            checked_all.warnings.extend(warnings);
            checked_all
                .reviews
                .extend(SnapshotReview::of(outcome, ctx.redactor));
        }
        *slot = assertion_result(spec.name(), checked, ctx.redactor);
    }
    checked_all
}

/// The assertion result of one snapshot check.
pub fn assertion_result(
    name: &str,
    checked: Result<SnapshotOutcome, SnapshotError>,
    redactor: &Redactor,
) -> AssertionResult {
    let label = format!("snapshot {name}");
    let outcome = match checked {
        Ok(outcome) => outcome,
        Err(error) => {
            return AssertionResult {
                kind: "snapshot",
                outcome: Outcome::Error,
                label,
                expected: None,
                actual: None,
                message: redact(&error.to_string(), redactor),
                snapshot: None,
            };
        }
    };
    let message = match outcome.state {
        SnapshotState::Failed => format!(
            "snapshot mismatch: {}\n{}",
            outcome.display_path, outcome.message
        ),
        SnapshotState::Missing => outcome.message.clone(),
        _ => String::new(),
    };
    AssertionResult {
        kind: "snapshot",
        outcome: if outcome.state.passed() {
            Outcome::Pass
        } else {
            Outcome::Fail
        },
        label,
        expected: None,
        actual: None,
        message: redact(&message, redactor),
        snapshot: Some(result(outcome, redactor)),
    }
}

fn result(outcome: SnapshotOutcome, redactor: &Redactor) -> SnapshotResult {
    SnapshotResult {
        name: outcome.name,
        state: outcome.state,
        path: outcome.display_path,
        diff: outcome.diff.map(|diff| redact_diff(diff, redactor)),
    }
}

/// The diff with every value redacted: a stored file edited by hand could
/// hold a secret the normalizer never saw.
pub(super) fn redact_diff(diff: SnapshotDiff, redactor: &Redactor) -> SnapshotDiff {
    let Ok(value) = serde_json::to_value(&diff) else {
        return diff;
    };
    serde_json::from_value(redact_value(&value, redactor)).unwrap_or(diff)
}

fn redact(text: &str, redactor: &Redactor) -> String {
    redactor.text(text).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::Secret;

    #[test]
    fn the_mode_follows_the_flags_and_the_ci_variable() {
        let detect = SnapshotMode::detect;
        assert_eq!(detect(false, false, None), Ok(SnapshotMode::Record));
        assert_eq!(detect(true, false, None), Ok(SnapshotMode::Update));
        assert_eq!(detect(false, true, None), Ok(SnapshotMode::Ci));
        assert_eq!(detect(false, false, Some("true")), Ok(SnapshotMode::Ci));
        assert_eq!(detect(false, false, Some("1")), Ok(SnapshotMode::Ci));
        assert_eq!(detect(false, false, Some("TRUE")), Ok(SnapshotMode::Ci));
        assert_eq!(
            detect(false, false, Some("false")),
            Ok(SnapshotMode::Record)
        );
        assert_eq!(detect(false, false, Some("0")), Ok(SnapshotMode::Record));
        assert_eq!(detect(true, true, None), Err(CONFLICTING_FLAGS));
        assert_eq!(detect(true, false, Some("1")), Err(CONFLICTING_FLAGS));
    }

    fn snapshot(state: SnapshotState) -> SnapshotResult {
        SnapshotResult {
            name: "default".into(),
            state,
            path: "a/__snapshots__/x.default.snap.yaml".into(),
            diff: None,
        }
    }

    #[test]
    fn counts_and_the_summary_line() {
        let results = [
            snapshot(SnapshotState::Matched),
            snapshot(SnapshotState::Matched),
            snapshot(SnapshotState::Recorded),
        ];
        let counts = SnapshotCounts::of(&results);
        assert_eq!((counts.matched, counts.recorded), (2, 1));
        assert_eq!(
            counts.line().unwrap(),
            "2 snapshots matched, 1 snapshot recorded"
        );
        assert_eq!(SnapshotCounts::default().line(), None);
        assert_eq!(
            results[2].note(),
            "snapshot default recorded: a/__snapshots__/x.default.snap.yaml"
        );
        assert!(results[2].wrote() && !results[0].wrote());
        assert!(SnapshotCounts::default().is_empty());
    }

    #[test]
    fn errors_and_diffs_are_redacted() {
        let mut redactor = Redactor::new();
        redactor.add(&Secret::new("token", "abc123".to_owned()));
        let error = SnapshotError::Invalid("bad file: abc123".into());
        let result = assertion_result("default", Err(error), &redactor);
        assert_eq!(result.outcome, Outcome::Error);
        assert_eq!(result.message, "bad file: «redacted:token»");
        let diff = SnapshotDiff {
            changes: vec![super::super::Change {
                path: "$.token".into(),
                kind: super::super::ChangeKind::Changed,
                old: Some(serde_json::json!("abc123")),
                new: Some(serde_json::json!("x")),
            }],
            ..SnapshotDiff::default()
        };
        let redacted = redact_diff(diff, &redactor);
        assert_eq!(
            redacted.changes[0].old,
            Some(serde_json::json!("«redacted:token»"))
        );
    }
}
