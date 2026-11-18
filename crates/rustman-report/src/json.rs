//! The JSON report of a run: the canonical [`Report`](crate::Report) of
//! kind `run`. Its JSON Schema is published at
//! `docs/schemas/report.schema.json`.

use rustman_core::run::RunRecord;

use crate::report::Report;
use crate::run::RunReport;

/// `run` as a pretty-printed `rustman.report/v1` document, ending in a newline.
pub fn json(run: &RunRecord) -> String {
    Report::Run(Box::new(RunReport::new(run))).to_json()
}
