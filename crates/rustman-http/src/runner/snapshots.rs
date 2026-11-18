//! `snapshot` assertions in a run (snapshot-testing): the runner maps the
//! response onto the shared snapshot exchange and lets
//! [`rustman_core::snapshot::check_all`] record, compare or update each
//! snapshot file in the run's mode.

use rustman_core::Redactor;
use rustman_core::assertions::{AssertionResult, ResponseView};
use rustman_core::model::Assertion;
use rustman_core::snapshot::{CheckedSnapshots, Exchange, SnapshotContext, check_all};

use super::Runner;
use super::execute::Checked;

impl Runner<'_> {
    /// Check the snapshot assertions of `checked` against `view`, filling
    /// their slots of `results`. Returns the (redacted) warnings and the
    /// snapshots to review.
    pub(super) fn check_snapshots(
        &self,
        checked: &Checked<'_, '_>,
        view: &ResponseView<'_>,
        redactor: &Redactor,
        results: &mut [AssertionResult],
    ) -> CheckedSnapshots {
        let has_snapshot = checked
            .assertions
            .iter()
            .any(|a| matches!(a, Assertion::Snapshot(_)));
        if !has_snapshot {
            return CheckedSnapshots::default();
        }
        let entry = checked.request.entry;
        let request_file = self.ctx.root.join(&entry.path);
        let request = entry.path.to_string_lossy();
        let exchange = Exchange {
            kind: checked.kind,
            status: Some(view.status),
            headers: &view.headers,
            body: &view.body,
            events: checked.events.as_deref(),
        };
        let ctx = SnapshotContext {
            request_file: &request_file,
            request: &request,
            mode: self.options.snapshots,
            redactor,
        };
        check_all(checked.assertions, results, &exchange, &ctx)
    }
}
