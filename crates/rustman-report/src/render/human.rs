//! Markdown and self-contained HTML, from `minijinja` templates. The HTML
//! inlines its stylesheet and script (built in with `include_str!`), so it
//! references nothing on the network; evidence is in `<details>`, which
//! works without JavaScript, and only the filters need it.

use std::collections::BTreeSet;

use minijinja::value::Serde;
use minijinja::{AutoEscape, Environment, Value};
use serde::Serialize;

use super::RenderOptions;
use crate::exploration::{Evidence, ExplorationReport, ReportFinding, Severity};
use crate::report::Report;
use crate::run::{RunReport, RunRequest, Status};

const EXPLORATION_MD: &str = include_str!("templates/exploration.md.jinja");
const RUN_MD: &str = include_str!("templates/run.md.jinja");
const HTML: &str = include_str!("templates/report.html.jinja");
const CSS: &str = include_str!("templates/report.css");
const JS: &str = include_str!("templates/report.js");

/// `report` as Markdown.
pub fn markdown(report: &Report, options: &RenderOptions) -> String {
    let (name, source) = match report {
        Report::Run(_) => ("run.md", RUN_MD),
        Report::Exploration(_) => ("exploration.md", EXPLORATION_MD),
    };
    finish(tidy(&render(name, source, context(report, options))))
}

/// Markdown with at most one blank line in a row outside `~~~~` fences, so
/// template tags leave no gaps; evidence inside fences is kept as is. A
/// fence closes only on a line of at least as many tildes as opened it.
fn tidy(markdown: &str) -> String {
    let mut out = String::with_capacity(markdown.len());
    let (mut fence, mut blank) = (0, false);
    for line in markdown.lines() {
        let tildes = line.len() - line.trim_start_matches('~').len();
        if fence == 0 && tildes >= 4 {
            fence = tildes;
        } else if fence > 0 && tildes >= fence && line.trim_end().len() == tildes {
            fence = 0;
        }
        let fenced = fence > 0;
        let empty = line.trim().is_empty();
        if !fenced && empty && blank {
            continue;
        }
        blank = !fenced && empty;
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// `report` as one self-contained HTML file.
pub fn html(report: &Report, options: &RenderOptions) -> String {
    finish(render("report.html", HTML, context(report, options)))
}

fn finish(mut text: String) -> String {
    let trimmed = text.trim_end().len();
    text.truncate(trimmed);
    text.push('\n');
    text
}

fn render(name: &str, source: &str, context: Value) -> String {
    let mut env = Environment::new();
    env.set_auto_escape_callback(|name| {
        if name.ends_with(".html") {
            AutoEscape::Html
        } else {
            AutoEscape::None
        }
    });
    env.add_filter("cell", cell);
    env.add_filter("code", code);
    env.add_filter("text", text);
    env.add_filter("fence", fence);
    env.add_template(name, source)
        .expect("built-in templates parse");
    env.get_template(name)
        .and_then(|t| t.render(context))
        .expect("built-in templates render")
}

/// A `~` fence for `content`: longer than any run of tildes in it (at
/// least four), so untrusted content cannot close it early.
fn fence(content: String) -> String {
    let longest = content.split(|c| c != '~').map(str::len).max().unwrap_or(0);
    "~".repeat(longest.max(3) + 1)
}

/// Text that is safe in one Markdown table cell or inline code span.
fn cell(value: String) -> String {
    one_line(&value).replace('|', "\\|")
}

/// `value` as one inline code span: delimited by more backticks than any
/// run of them in it, and padded when it starts or ends with one, so
/// untrusted content cannot close the span early.
fn code(value: String) -> String {
    let value = one_line(&value);
    let longest = value.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let ticks = "`".repeat(longest + 1);
    let pad = if value.starts_with('`') || value.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{ticks}{pad}{value}{pad}{ticks}")
}

/// Untrusted text (a response excerpt in a message) as literal Markdown in
/// one table cell: every character Markdown or HTML would interpret is
/// backslash-escaped, so no markup, link or raw HTML gets through.
fn text(value: String) -> String {
    const SPECIAL: &str = "\\`*_[]()<>&#!|~";
    let mut out = String::with_capacity(value.len());
    for c in one_line(&value).chars() {
        if SPECIAL.contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn one_line(value: &str) -> String {
    value.replace("\r\n", " ").replace(['\n', '\r'], " ")
}

fn context(report: &Report, options: &RenderOptions) -> Value {
    match report {
        Report::Run(r) => Value::from(Serde(run_view(r))),
        Report::Exploration(r) => Value::from(Serde(exploration_view(r, options))),
    }
}

#[derive(Serialize)]
struct ExplorationView<'a> {
    kind: &'static str,
    title: String,
    r: &'a ExplorationReport,
    p: &'a crate::exploration::ProbeTotals,
    c: &'a crate::exploration::Coverage,
    fail_on: Severity,
    failing: usize,
    counts: String,
    duration: String,
    safety: String,
    findings: Vec<FindingView<'a>>,
    severities: Vec<Severity>,
    finding_operations: BTreeSet<&'a str>,
    css: &'static str,
    js: &'static str,
}

#[derive(Serialize)]
struct FindingView<'a> {
    #[serde(flatten)]
    f: &'a ReportFinding,
    request_text: String,
    response_text: String,
}

fn exploration_view<'a>(r: &'a ExplorationReport, options: &RenderOptions) -> ExplorationView<'a> {
    let fail_on = options.fail_on.unwrap_or(r.settings.fail_on);
    let severities: BTreeSet<Severity> = r.findings.iter().map(|f| f.severity).collect();
    ExplorationView {
        kind: "exploration",
        title: format!("Exploration {}", r.id),
        r,
        p: &r.summary.probes,
        c: &r.coverage,
        fail_on,
        failing: r.failing(fail_on),
        counts: r.summary.findings.to_string(),
        duration: seconds(r.summary.duration_ms as f64),
        safety: safety(r),
        findings: r
            .findings
            .iter()
            .map(|f| FindingView {
                f,
                request_text: request_text(&f.evidence),
                response_text: response_text(&f.evidence),
            })
            .collect(),
        severities: severities.into_iter().collect(),
        finding_operations: r.findings.iter().map(|f| f.operation.as_str()).collect(),
        css: CSS,
        js: JS,
    }
}

fn safety(r: &ExplorationReport) -> String {
    let s = &r.settings;
    let methods = if s.allow_destructive {
        "every method allowed (--allow-destructive)"
    } else if s.allow_mutations {
        "mutations allowed, DELETE blocked"
    } else {
        "read-only"
    };
    let production = s
        .production
        .as_deref()
        .map_or_else(String::new, |h| format!("; production host {h} confirmed"));
    format!(
        "{methods}; {} requests/s, {} at once; budget {} requests{production}",
        s.rate, s.concurrency, s.max_probes
    )
}

/// The evidence request as HTTP text.
fn request_text(e: &Evidence) -> String {
    let mut text = format!("{} {}", e.method, e.url);
    for (name, value) in &e.request_headers {
        text.push_str(&format!("\n{name}: {value}"));
    }
    if let Some(body) = &e.request_body {
        text.push_str("\n\n");
        text.push_str(body);
    }
    text
}

/// The evidence response as HTTP text.
fn response_text(e: &Evidence) -> String {
    let mut text = format!("HTTP {}", e.status);
    for (name, value) in &e.response_headers {
        text.push_str(&format!("\n{name}: {value}"));
    }
    if !e.body.is_empty() {
        text.push_str("\n\n");
        text.push_str(&e.body);
    }
    if e.truncated {
        text.push_str("\n[truncated]");
    }
    text
}

fn seconds(ms: f64) -> String {
    format!("{:.1} s", ms / 1000.0)
}

#[derive(Serialize)]
struct RunView<'a> {
    kind: &'static str,
    title: &'static str,
    r: &'a RunReport,
    duration: String,
    requests: Vec<RequestView<'a>>,
    failures: Vec<RequestView<'a>>,
    css: &'static str,
    js: &'static str,
}

#[derive(Clone, Serialize)]
struct RequestView<'a> {
    name: String,
    file: &'a str,
    status: Status,
    exchange: String,
    duration: String,
    reasons: Vec<String>,
}

fn run_view(r: &RunReport) -> RunView<'_> {
    let show_iteration = r.requests.iter().any(|q| q.iteration > 1);
    let requests: Vec<RequestView> = r
        .requests
        .iter()
        .map(|q| request_view(q, show_iteration))
        .collect();
    let failures = requests
        .iter()
        .filter(|q| matches!(q.status, Status::Failed | Status::Errored))
        .cloned()
        .collect();
    RunView {
        kind: "run",
        title: "Run report",
        r,
        duration: seconds(r.summary.duration_ms),
        requests,
        failures,
        css: CSS,
        js: JS,
    }
}

fn request_view(q: &RunRequest, show_iteration: bool) -> RequestView<'_> {
    let name = if show_iteration {
        format!("{} [{}]", q.id, q.iteration)
    } else {
        q.id.clone()
    };
    let exchange = q.exchange.as_ref().map_or_else(
        || q.skip_reason.clone().unwrap_or_default(),
        |e| {
            format!(
                "{} {} -> {}",
                e.request.method, e.request.url, e.response.status
            )
        },
    );
    RequestView {
        name,
        file: &q.file,
        status: q.status,
        exchange,
        duration: format!("{:.0} ms", q.duration_ms),
        reasons: q.reasons(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tidy_keeps_one_blank_line_outside_fences() {
        let text = "# A\n\n\n\nb\n~~~~\nx\n\n\ny\n~~~~\n\n\nc\n";
        assert_eq!(tidy(text), "# A\n\nb\n~~~~\nx\n\n\ny\n~~~~\n\nc\n");
    }

    #[test]
    fn fences_outgrow_the_tildes_they_hold() {
        assert_eq!(fence("plain".into()), "~~~~");
        assert_eq!(fence("a\n~~~~~~\nb ~~".into()), "~~~~~~~");
        let text = "~~~~~\n~~~~\n\n\nx\n~~~~~\n\n\nc\n";
        assert_eq!(tidy(text), "~~~~~\n~~~~\n\n\nx\n~~~~~\n\nc\n");
    }

    #[test]
    fn cells_escape_pipes_and_newlines() {
        assert_eq!(cell("a | b\r\nc\nd".into()), "a \\| b c d");
    }

    #[test]
    fn code_spans_cannot_be_closed_by_their_content() {
        assert_eq!(code("GET /x".into()), "`GET /x`");
        assert_eq!(code("GET /a``b".into()), "```GET /a``b```");
        assert_eq!(code("`x".into()), "`` `x ``");
    }

    #[test]
    fn text_escapes_markup_and_html() {
        assert_eq!(
            text("at /name: <img src=x onerror=alert(1)> is *bad* | [x](y)\nz".into()),
            "at /name: \\<img src=x onerror=alert\\(1\\)\\> is \\*bad\\* \\| \\[x\\]\\(y\\) z"
        );
    }
}
