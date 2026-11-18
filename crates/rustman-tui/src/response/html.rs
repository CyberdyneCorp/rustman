//! "Export HTML report": the current exchange as one self-contained HTML
//! page, the fallback offered when the viz companion is unavailable (spec:
//! Open in viz companion).
//!
//! Everything written comes from the redacted record and the redacted body
//! the viewer holds, so no secret can reach the file. The page is written
//! under `.rustman/reports/` in the workspace (ignored by Git), or to the
//! temp directory in a scratch session.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rustman_core::assertions::Outcome;
use rustman_core::run::RequestRecord;

use crate::app::{Effect, State};
use crate::response::Source;
use crate::toast::Severity;
use crate::workbench;

/// Bodies are cut after this many bytes in the report.
const BODY_LIMIT: usize = 10 * 1024 * 1024;

/// One report to write, owned so it can be written on another thread.
#[derive(Debug, Clone)]
pub struct ReportJob {
    pub path: PathBuf,
    /// The redacted record of the exchange.
    pub record: Box<RequestRecord>,
    /// The redacted body.
    pub source: Option<Source>,
}

impl PartialEq for ReportJob {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
    }
}

impl Eq for ReportJob {}

/// `report.html`: write the current tab's exchange as an HTML report.
pub fn start(state: &mut State) -> Vec<Effect> {
    let dir =
        workbench::workspace(state).map_or_else(std::env::temp_dir, |ws| reports_dir(&ws.root));
    let Some(received) = state
        .tabs
        .active()
        .filter(|t| t.in_flight.is_none())
        .and_then(|t| t.exchange.as_deref())
    else {
        state.notify(Severity::Info, "Send the request to export a report");
        return Vec::new();
    };
    let job = ReportJob {
        path: dir.join(file_name(&received.record.id)),
        record: Box::new(received.record.clone()),
        source: received.source.clone(),
    };
    vec![Effect::WriteReport(Box::new(job))]
}

/// The report was written (or not).
pub fn on_written(state: &mut State, result: Result<PathBuf, String>) {
    match result {
        Ok(path) => state.notify(
            Severity::Success,
            format!("Wrote HTML report {}", path.display()),
        ),
        Err(e) => state.notify(Severity::Error, format!("Cannot write the report: {e}")),
    }
}

/// `<request id>-<unix ms>.html`, with `/` in the id made `_`.
fn file_name(id: &str) -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let id: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{id}-{millis}.html")
}

/// Write the report (the effect side).
pub fn write(job: &ReportJob) -> Result<PathBuf, String> {
    let body = match &job.source {
        Some(source) => source.read_all().map_err(|e| e.to_string())?,
        None => Vec::new(),
    };
    let html = render(&job.record, &body);
    if let Some(dir) = job.path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&job.path, html).map_err(|e| e.to_string())?;
    Ok(job.path.clone())
}

/// The page for `record` with the (redacted) `body`.
pub fn render(record: &RequestRecord, body: &[u8]) -> String {
    let mut out = String::new();
    let title = escape(&record.name);
    let _ = write!(
        out,
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <title>{title} · Rustman</title>\n<style>{STYLE}</style>\n</head>\n<body>\n\
         <h1>{title}</h1>\n"
    );
    summary(&mut out, record);
    assertions(&mut out, record);
    headers(&mut out, record);
    body_section(&mut out, record, body);
    let _ = write!(
        out,
        "<footer>rustman {} · secrets are redacted</footer>\n</body>\n</html>\n",
        env!("CARGO_PKG_VERSION")
    );
    out
}

const STYLE: &str = "body{font-family:system-ui,sans-serif;margin:2rem;max-width:72rem}\
pre{background:#f4f4f4;padding:1rem;overflow:auto}\
table{border-collapse:collapse}td,th{border:1px solid #ccc;padding:.25rem .5rem;text-align:left}\
.pass{color:#1a7f37}.fail{color:#cf222e}footer{color:#666;margin-top:2rem}";

fn summary(out: &mut String, record: &RequestRecord) {
    let Some(exchange) = &record.exchange else {
        let error = record.error.as_deref().unwrap_or("no response");
        let _ = writeln!(out, "<p class=\"fail\">{}</p>", escape(error));
        return;
    };
    let insecure = if exchange.insecure {
        " · TLS verification off"
    } else {
        ""
    };
    let _ = writeln!(
        out,
        "<p><code>{} {}</code></p>\n<p>{} · {:.0} ms · {} bytes{insecure}</p>",
        escape(&exchange.request.method),
        escape(&exchange.request.url),
        exchange.response.status,
        exchange.timings.total_ms,
        exchange.response.body_bytes,
    );
}

fn assertions(out: &mut String, record: &RequestRecord) {
    if record.assertions.is_empty() {
        return;
    }
    out.push_str("<h2>Assertions</h2>\n<table>\n<tr><th></th><th>Check</th><th>Expected</th><th>Actual</th></tr>\n");
    for a in &record.assertions {
        let (class, mark) = match a.outcome {
            Outcome::Pass => ("pass", "pass"),
            Outcome::Fail => ("fail", "fail"),
            Outcome::Error => ("fail", "error"),
        };
        let _ = writeln!(
            out,
            "<tr><td class=\"{class}\">{mark}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            escape(&a.label),
            escape(a.expected.as_deref().unwrap_or("")),
            escape(a.actual.as_deref().unwrap_or("")),
        );
    }
    out.push_str("</table>\n");
}

fn headers(out: &mut String, record: &RequestRecord) {
    let Some(exchange) = &record.exchange else {
        return;
    };
    out.push_str("<h2>Response headers</h2>\n<table>\n");
    for h in &exchange.response.headers {
        let _ = writeln!(
            out,
            "<tr><th>{}</th><td>{}</td></tr>",
            escape(&h.name),
            escape(&h.value)
        );
    }
    out.push_str("</table>\n");
}

fn body_section(out: &mut String, record: &RequestRecord, body: &[u8]) {
    let binary = record.exchange.as_ref().is_some_and(|e| e.response.binary);
    if record.exchange.is_none() {
        return;
    }
    out.push_str("<h2>Body</h2>\n");
    if binary {
        let _ = writeln!(
            out,
            "<p>Binary body, {} bytes (not included).</p>",
            body.len()
        );
        return;
    }
    let cut = body.len() > BODY_LIMIT;
    let text = String::from_utf8_lossy(&body[..body.len().min(BODY_LIMIT)]);
    let _ = writeln!(out, "<pre>{}</pre>", escape(&text));
    if cut {
        let _ = writeln!(out, "<p>Cut after {BODY_LIMIT} bytes.</p>");
    }
}

/// `text` safe inside HTML elements and attribute values.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Where reports of the workspace at `root` go.
pub fn reports_dir(root: &Path) -> PathBuf {
    root.join(".rustman").join("reports")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_is_escaped() {
        assert_eq!(
            escape("<a href=\"x\">&'</a>"),
            "&lt;a href=&quot;x&quot;&gt;&amp;&#39;&lt;/a&gt;"
        );
    }

    #[test]
    fn file_names_are_flat() {
        let name = file_name("users/get user");
        assert!(
            name.starts_with("users_get_user-") && name.ends_with(".html"),
            "{name}"
        );
    }

    #[test]
    fn a_record_without_exchange_shows_its_error() {
        let mut record = RequestRecord::new("r".into(), "<R>".into(), "r.rman.yaml".into());
        record.error = Some("dns: no such host".into());
        let html = render(&record, b"");
        assert!(html.contains("<h1>&lt;R&gt;</h1>"), "{html}");
        assert!(html.contains("dns: no such host"));
        assert!(!html.contains("<h2>Body</h2>"));
    }
}
