//! Reviewing snapshots after a send (snapshot-testing "TUI snapshot
//! review"): a failed, missing or new snapshot is kept as a
//! [`SnapshotReview`] with the text a run would have written, so a front-end
//! can show its diff and approve it later without sending again.
//!
//! The kept text went through the normalizer (redaction first) and the
//! secret-echo scan in [`super::check`]; the diff and the message are
//! redacted again, like every report field.

use std::path::PathBuf;

use super::run::redact_diff;
use super::{
    SnapshotDiff, SnapshotError, SnapshotMode, SnapshotOutcome, SnapshotState, WriteResult,
    write_expecting,
};
use crate::redact::Redactor;

/// A snapshot a user may want to look at after a send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotReview {
    pub name: String,
    pub state: SnapshotState,
    /// The snapshot file.
    pub path: PathBuf,
    /// The snapshot path relative to the workspace, with `/`.
    pub display_path: String,
    /// The normalized, redacted snapshot text approving would write.
    pub received: String,
    /// The stored text the diff was taken against (`None`: no file then).
    pub stored: Option<String>,
    /// The differences from the stored snapshot (redacted).
    pub diff: Option<SnapshotDiff>,
    /// Why it failed (redacted); empty when it passed.
    pub message: String,
}

impl SnapshotReview {
    /// The review of `outcome`: `None` for a matching or updated snapshot,
    /// which needs no review.
    pub fn of(outcome: &SnapshotOutcome, redactor: &Redactor) -> Option<Self> {
        if !matches!(
            outcome.state,
            SnapshotState::Failed | SnapshotState::Missing | SnapshotState::Recorded
        ) {
            return None;
        }
        Some(Self {
            name: outcome.name.clone(),
            state: outcome.state,
            path: outcome.path.clone(),
            display_path: outcome.display_path.clone(),
            received: outcome.received.clone(),
            stored: outcome.stored.clone(),
            diff: outcome.diff.clone().map(|d| redact_diff(d, redactor)),
            message: redactor.text(&outcome.message).into_owned(),
        })
    }

    /// Whether approving would change anything: the snapshot failed or is
    /// missing. A recorded one is already on disk.
    pub fn approvable(&self) -> bool {
        !self.state.passed()
    }

    /// Write the received snapshot over the stored one, atomically and under
    /// the file's lock, only while the file still holds what the diff was
    /// taken against: a snapshot a later run changed is kept
    /// ([`SnapshotError::Changed`]). CI mode never writes.
    pub fn approve(&self, mode: SnapshotMode) -> Result<WriteResult, SnapshotError> {
        if mode == SnapshotMode::Ci {
            return Err(SnapshotError::CiMode);
        }
        write_expecting(
            &self.path,
            &self.display_path,
            self.stored.as_deref(),
            &self.received,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use serde_json::json;

    use super::*;
    use crate::model::SnapshotAssertion;
    use crate::secret::Secret;
    use crate::snapshot::{Exchange, SnapshotContext, check};

    fn spec() -> SnapshotAssertion {
        let checked: crate::model::Checked<SnapshotAssertion> =
            crate::format::from_yaml(Path::new("s.yaml"), "{}").unwrap();
        checked.into_inner()
    }

    fn outcome(dir: &Path, body: &str, mode: SnapshotMode, redactor: &Redactor) -> SnapshotOutcome {
        let headers = vec![("Content-Type".to_owned(), "application/json".to_owned())];
        let exchange = Exchange {
            kind: "http",
            status: Some(200),
            headers: &headers,
            body: body.as_bytes(),
            events: None,
        };
        let request_file = dir.join("a.rman.yaml");
        let ctx = SnapshotContext {
            request_file: &request_file,
            request: "a.rman.yaml",
            mode,
            redactor,
        };
        check(&spec(), &exchange, &ctx).unwrap()
    }

    #[test]
    fn approving_a_failed_snapshot_writes_the_received_text() {
        let tmp = tempfile::tempdir().unwrap();
        let redactor = Redactor::new();
        let recorded = outcome(tmp.path(), r#"{"a":1}"#, SnapshotMode::Record, &redactor);
        let review = SnapshotReview::of(&recorded, &redactor).unwrap();
        assert_eq!(review.state, SnapshotState::Recorded);
        assert!(!review.approvable());

        let failed = outcome(tmp.path(), r#"{"a":2}"#, SnapshotMode::Record, &redactor);
        let review = SnapshotReview::of(&failed, &redactor).unwrap();
        assert!(review.approvable());
        assert!(review.message.contains("$.a"), "{}", review.message);
        assert_eq!(
            review.approve(SnapshotMode::Ci).unwrap_err().to_string(),
            "snapshot approval is disabled in CI mode"
        );
        assert_eq!(fs::read_to_string(&review.path).unwrap(), recorded.received);
        assert_eq!(
            review.approve(SnapshotMode::Record).unwrap(),
            WriteResult::Written
        );
        assert_eq!(fs::read_to_string(&review.path).unwrap(), review.received);
        assert_eq!(
            review.approve(SnapshotMode::Record).unwrap(),
            WriteResult::Unchanged
        );
        let matched = outcome(tmp.path(), r#"{"a":2}"#, SnapshotMode::Ci, &redactor);
        assert_eq!(matched.state, SnapshotState::Matched);
        assert_eq!(SnapshotReview::of(&matched, &redactor), None);
    }

    #[test]
    fn the_diff_and_message_are_redacted() {
        let tmp = tempfile::tempdir().unwrap();
        let plain = Redactor::new();
        let mut outcome = outcome(tmp.path(), r#"{"a":1}"#, SnapshotMode::Record, &plain);
        outcome.state = SnapshotState::Failed;
        outcome.message = "token tok-123 differs".into();
        outcome.diff = Some(SnapshotDiff {
            changes: vec![crate::snapshot::Change {
                path: "$.t".into(),
                kind: crate::snapshot::ChangeKind::Changed,
                old: Some(json!("tok-123")),
                new: Some(json!("x")),
            }],
            ..SnapshotDiff::default()
        });
        let mut redactor = Redactor::new();
        redactor.add(&Secret::new("tok", "tok-123".to_owned()));
        let review = SnapshotReview::of(&outcome, &redactor).unwrap();
        assert!(!review.message.contains("tok-123"));
        assert!(!format!("{:?}", review.diff).contains("tok-123"));
    }

    #[test]
    fn a_stale_review_does_not_overwrite_a_newer_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let redactor = Redactor::new();
        outcome(tmp.path(), r#"{"a":1}"#, SnapshotMode::Record, &redactor);
        let failed = outcome(tmp.path(), r#"{"a":2}"#, SnapshotMode::Record, &redactor);
        let review = SnapshotReview::of(&failed, &redactor).unwrap();
        let newer = outcome(tmp.path(), r#"{"a":3}"#, SnapshotMode::Update, &redactor);
        assert_eq!(newer.state, SnapshotState::Updated);
        let err = review.approve(SnapshotMode::Record).unwrap_err();
        assert!(matches!(err, SnapshotError::Changed(_)), "{err}");
        assert!(err.to_string().contains("changed on disk"), "{err}");
        assert_eq!(fs::read_to_string(&review.path).unwrap(), newer.received);
    }
}
