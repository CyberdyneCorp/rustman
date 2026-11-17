//! The human-readable report: one line per request, the reasons below each
//! failure, and a summary line.
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
    let iterations = run.requests.iter().map(|r| r.iteration).max().unwrap_or(1);
    let mut out = String::new();
    for record in &run.requests {
        line(&mut out, record, iterations > 1, styled);
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

fn line(out: &mut String, record: &RequestRecord, show_iteration: bool, styled: bool) {
    let (label, style) = label(record.status);
    let label = paint(label, style, styled);
    let iteration = if show_iteration {
        format!("[{}] ", record.iteration)
    } else {
        String::new()
    };
    let _ = write!(out, "{label} {iteration}{}", record.id);
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
