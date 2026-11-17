//! The JSON report: `rustman.report/v1` with `kind: run`. Its JSON Schema is
//! published at `docs/schemas/report.schema.json`.

use rustman_core::run::{RequestRecord, RunRecord, Summary};
use serde::Serialize;

/// The `schema` of every report this version writes.
pub const SCHEMA: &str = "rustman.report/v1";

/// The `kind` of a `rustman run` (and `rustman send`) report.
pub const KIND_RUN: &str = "run";

#[derive(Serialize)]
struct Report<'a> {
    schema: &'static str,
    kind: &'static str,
    rustman_version: &'static str,
    started_at: &'a str,
    environment: Option<&'a str>,
    summary: &'a Summary,
    requests: &'a [RequestRecord],
}

/// `run` as a pretty-printed `rustman.report/v1` document, ending in a newline.
pub fn json(run: &RunRecord) -> String {
    let report = Report {
        schema: SCHEMA,
        kind: KIND_RUN,
        rustman_version: rustman_core::VERSION,
        started_at: &run.started_at,
        environment: run.environment.as_deref(),
        summary: &run.summary,
        requests: &run.requests,
    };
    let mut text = serde_json::to_string_pretty(&report).expect("reports serialize");
    text.push('\n');
    text
}
