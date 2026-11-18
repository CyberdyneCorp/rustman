//! The human-readable report: one line per request, the reasons below each
//! failure, and a summary line. The requests of a data-driven or repeated
//! run are grouped under one header line per iteration.
//!
//! The styled form adds ANSI colors; the terminal writer (`anstream` in the
//! CLI) strips them when color is off, so the text is otherwise identical.

use std::fmt::Write as _;

use anstyle::{AnsiColor, Style};
use rustman_core::assertions::Outcome;
use rustman_core::run::{RequestRecord, RequestStatus, RunRecord, Summary};

/// `run` as plain text (no color), ending in a newline.
pub fn pretty(run: &RunRecord) -> String {
    render(run, false)
}

/// `run` with ANSI colors on the status labels and reasons.
pub fn pretty_styled(run: &RunRecord) -> String {
    render(run, true)
}

fn render(run: &RunRecord, styled: bool) -> String {
    let mut out = String::new();
    let mut current = None;
    for record in &run.requests {
        let grouped = record.data_row.is_some() || record.iterations > 1;
        let index = grouped.then_some(record.iteration);
        if index != current {
            current = index;
            iteration_header(&mut out, record, styled);
        }
        line(&mut out, record, styled);
    }
    out.push('\n');
    out.push_str(&summary(&run.summary));
    out
}

/// `text` wrapped in `style` when `styled`.
fn paint(text: &str, style: Style, styled: bool) -> String {
    if styled {
        format!("{style}{text}{style:#}")
    } else {
        text.to_owned()
    }
}

fn label(status: RequestStatus) -> (&'static str, Style) {
    let color = |c: AnsiColor| Style::new().fg_color(Some(c.into())).bold();
    match status {
        RequestStatus::Passed => ("PASS ", color(AnsiColor::Green)),
        RequestStatus::Failed => ("FAIL ", color(AnsiColor::Red)),
        RequestStatus::Errored => ("ERROR", color(AnsiColor::Magenta)),
        RequestStatus::Skipped => ("SKIP ", color(AnsiColor::Yellow)),
    }
}

/// The line that opens the requests of one iteration: `iteration 2/3`,
/// with the data row's label when it has one (`iteration 2/3 · bo@example.com`).
fn iteration_header(out: &mut String, record: &RequestRecord, styled: bool) {
    if !out.is_empty() {
        out.push('\n');
    }
    let text = match &record.data_row {
        Some(row) if row.label != row.index.to_string() => {
            format!("iteration {}/{} · {}", row.index, row.total, row.label)
        }
        Some(row) => format!("iteration {}/{}", row.index, row.total),
        None => format!("iteration {}/{}", record.iteration, record.iterations),
    };
    let _ = writeln!(out, "{}", paint(&text, Style::new().bold(), styled));
}

fn line(out: &mut String, record: &RequestRecord, styled: bool) {
    let (label, style) = label(record.status);
    let label = paint(label, style, styled);
    let _ = write!(out, "{label} {}", record.id);
    match (&record.exchange, record.skip_reason) {
        (_, Some(reason)) => {
            let _ = write!(out, "  skipped ({reason})");
        }
        (Some(exchange), None) => {
            let _ = write!(
                out,
                "  {} {}  {:.0} ms",
                exchange.request.method, exchange.response.status, exchange.timings.total_ms
            );
        }
        (None, None) => {}
    }
    out.push('\n');
    let dim = Style::new().dimmed();
    for reason in reasons(record) {
        let _ = writeln!(out, "      {}", paint(&reason, dim, styled));
    }
}

/// Why a request did not pass: its error, failed assertions and warnings.
pub(crate) fn reasons(record: &RequestRecord) -> Vec<String> {
    let mut reasons: Vec<String> = record.error.iter().cloned().collect();
    reasons.extend(
        record
            .assertions
            .iter()
            .filter(|a| a.outcome != Outcome::Pass)
            .map(ToString::to_string),
    );
    reasons.extend(record.warnings.iter().map(|w| format!("warning: {w}")));
    reasons
}

fn summary(summary: &Summary) -> String {
    let noun = if summary.total == 1 {
        "request"
    } else {
        "requests"
    };
    let mut text = format!(
        "{} {noun}: {} passed, {} failed, {} errored, {} skipped ({:.0} ms)\n",
        summary.total,
        summary.passed,
        summary.failed,
        summary.errored,
        summary.skipped,
        summary.duration_ms
    );
    if summary.interrupted {
        text.push_str("interrupted: the results are partial\n");
    }
    text
}
