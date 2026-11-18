//! The TAP report (Test Anything Protocol, version 14): one test point per
//! request, with a YAML diagnostic block listing why a request did not pass.

use std::fmt::Write as _;

use rustman_core::run::{RequestRecord, RequestStatus, RunRecord};

use crate::pretty::reasons;

/// `run` as a TAP version 14 stream, ending in a newline.
pub fn tap(run: &RunRecord) -> String {
    let mut out = format!("TAP version 14\n1..{}\n", run.requests.len());
    for (number, record) in (1..).zip(&run.requests) {
        point(&mut out, number, record);
    }
    if run.summary.interrupted {
        out.push_str("Bail out! interrupted: the results are partial\n");
    }
    out
}

fn point(out: &mut String, number: usize, record: &RequestRecord) {
    let ok = if matches!(
        record.status,
        RequestStatus::Passed | RequestStatus::Skipped
    ) {
        "ok"
    } else {
        "not ok"
    };
    // `#` starts a directive in TAP, so it is escaped in descriptions.
    let mut description = record.id.clone();
    if let Some(label) = record.iteration_label() {
        let _ = write!(description, " [{label}]");
    }
    let description = description.replace('#', "\\#");
    let _ = write!(out, "{ok} {number} - {description}");
    if let Some(reason) = record.skip_reason {
        let _ = write!(out, " # SKIP {reason}");
    }
    out.push('\n');
    if matches!(
        record.status,
        RequestStatus::Failed | RequestStatus::Errored
    ) {
        diagnostics(out, record);
    }
}

/// The YAML block under a failed or errored test point. Strings are written
/// as JSON strings, which are valid YAML scalars whatever they contain.
fn diagnostics(out: &mut String, record: &RequestRecord) {
    let quote = |text: &str| serde_json::Value::from(text).to_string();
    out.push_str("  ---\n");
    let status = match record.status {
        RequestStatus::Failed => "failed",
        _ => "errored",
    };
    let _ = writeln!(out, "  status: {status}");
    let _ = writeln!(out, "  file: {}", quote(&record.file));
    out.push_str("  reasons:\n");
    for reason in reasons(record) {
        let _ = writeln!(out, "    - {}", quote(&reason));
    }
    out.push_str("  ...\n");
}
