//! Findings from fuzzed traffic (stress-testing "Malformed payload
//! families", "Malformed input acceptance"): how the target answered the
//! variants it was sent, collected by rule and family while the run goes
//! on, with the first exchange of each as evidence (redacted) and, for a
//! failure, the smallest input that failed (a nesting depth, a size).
//!
//! | Rule | Severity | When |
//! |------|----------|------|
//! | `server-error-on-malformed-input` | high | a 5xx answer to a variant |
//! | `timeout-on-malformed-input` | high | no answer to a variant in time |
//! | `accepted-malformed-input` | medium | a 2xx answer to invalid input (a malformed family or a schema violation) |
//! | `oversized-input-accepted` | medium | a 2xx answer to a body over a documented `maxLength`/`maxItems` or `[stress] expected_max_body` |

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use rustman_core::Redactor;
use rustman_core::run::RequestRecord;
use rustman_report::exploration::Severity;

use rustman_http::BodyOverride;

use crate::metrics::ErrorClass;
use crate::stress::fuzz::gate::Fuzzed;
use crate::stress::{Evidence, RESPONSE_EXCERPT, StressFinding};

pub const SERVER_ERROR: &str = "server-error-on-malformed-input";
pub const TIMEOUT: &str = "timeout-on-malformed-input";
pub const ACCEPTED: &str = "accepted-malformed-input";
pub const OVERSIZED: &str = "oversized-input-accepted";

struct Entry {
    severity: Severity,
    count: u64,
    file: String,
    /// The first status seen, for the message.
    status: Option<u16>,
    evidence: Evidence,
    /// The smallest failing input (what, how much).
    smallest: Option<(&'static str, u64)>,
    /// The body behind a crash-like finding, for minimization.
    input: Option<BodyOverride>,
    /// The variant's whole body as text, redacted, for export.
    text: Option<String>,
}

/// The input behind a crash-like finding (a 5xx or timeout caused by a
/// variant), to replay alone once load has stopped.
#[derive(Clone, Debug)]
pub struct CrashInput {
    pub rule: &'static str,
    pub family: String,
    /// The request file it was sent as.
    pub file: String,
    pub body: BodyOverride,
}

/// Findings of the fuzzed exchanges of a run; shared by every VU.
pub struct FuzzFindings {
    expected_max_body: Option<u64>,
    entries: Mutex<BTreeMap<(&'static str, String), Entry>>,
}

/// One fuzzed exchange, as observed.
pub struct Observed<'a> {
    pub fuzzed: &'a Fuzzed,
    pub file: &'a str,
    pub record: &'a RequestRecord,
    pub error: Option<ErrorClass>,
    pub redactor: &'a Redactor,
}

impl FuzzFindings {
    pub fn new(expected_max_body: Option<u64>) -> Self {
        Self {
            expected_max_body,
            entries: Mutex::new(BTreeMap::new()),
        }
    }

    /// Judge one fuzzed exchange.
    pub fn observe(&self, seen: &Observed<'_>) {
        let status = seen.record.exchange.as_ref().map(|e| e.response.status);
        for (rule, severity) in self.rules(seen.fuzzed, status, seen.error) {
            self.add(rule, severity, seen, status);
        }
    }

    fn rules(
        &self,
        fuzzed: &Fuzzed,
        status: Option<u16>,
        error: Option<ErrorClass>,
    ) -> Vec<(&'static str, Severity)> {
        let mut rules = Vec::new();
        match status {
            Some(500..=599) => rules.push((SERVER_ERROR, Severity::High)),
            Some(200..=299) => {
                if fuzzed.invalid {
                    rules.push((ACCEPTED, Severity::Medium));
                }
                let over_expected = self.expected_max_body.is_some_and(|m| fuzzed.expanded > m);
                if fuzzed.oversized || over_expected {
                    rules.push((OVERSIZED, Severity::Medium));
                }
            }
            None if error == Some(ErrorClass::Timeout) => rules.push((TIMEOUT, Severity::High)),
            _ => {}
        }
        rules
    }

    fn add(
        &self,
        rule: &'static str,
        severity: Severity,
        seen: &Observed<'_>,
        status: Option<u16>,
    ) {
        let fuzzed = seen.fuzzed;
        let failure = rule == SERVER_ERROR || rule == TIMEOUT;
        let measure = fuzzed.measure.filter(|_| failure);
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = entries
            .entry((rule, fuzzed.family.clone()))
            .or_insert_with(|| Entry {
                severity,
                count: 0,
                file: seen.file.to_owned(),
                status,
                evidence: evidence(seen, status),
                smallest: measure,
                input: fuzzed.input.clone().filter(|_| failure),
                text: input_text(seen),
            });
        entry.count += 1;
        if let (Some(now), Some(best)) = (measure, entry.smallest)
            && now.1 < best.1
        {
            entry.smallest = Some(now);
            entry.evidence = evidence(seen, status);
            entry.input = fuzzed.input.clone().filter(|_| failure);
            entry.text = input_text(seen);
        }
    }

    /// The inputs of the crash-like findings so far, by rule then family.
    pub fn crash_inputs(&self) -> Vec<CrashInput> {
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries
            .iter()
            .filter_map(|((rule, family), entry)| {
                Some(CrashInput {
                    rule,
                    family: family.clone(),
                    file: entry.file.clone(),
                    body: entry.input.clone()?,
                })
            })
            .collect()
    }

    /// The findings so far, by rule then family.
    pub fn findings(&self) -> Vec<StressFinding> {
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries
            .iter()
            .map(|((rule, family), entry)| StressFinding {
                rule,
                severity: entry.severity,
                message: message(rule, family, entry),
                family: Some(family.clone()),
                evidence: Some(entry.evidence.clone()),
                file: Some(entry.file.clone()),
                minimization: None,
                input: entry.text.clone(),
            })
            .collect()
    }
}

fn message(rule: &str, family: &str, entry: &Entry) -> String {
    let times = match entry.count {
        1 => "once".to_owned(),
        n => format!("{n} times"),
    };
    let answer = entry
        .status
        .map_or_else(|| "no answer".to_owned(), |s| format!("{s}"));
    let mut text = match rule {
        TIMEOUT => format!("{family} input on {} timed out ({times})", entry.file),
        _ => format!(
            "{family} input on {} answered {answer} ({times})",
            entry.file
        ),
    };
    if let Some((what, n)) = entry.smallest {
        text.push_str(&format!("; smallest failing {what}: {n}"));
    }
    match rule {
        ACCEPTED => text.push_str("; expected a 4xx refusal"),
        OVERSIZED => text.push_str("; expected 413 or a 4xx refusal"),
        _ => {}
    }
    text
}

/// The longest variant body kept whole for export.
const MAX_INPUT_TEXT: usize = 64 * 1024;

/// The variant's body as text, redacted: only an uncompressed UTF-8 body
/// of at most [`MAX_INPUT_TEXT`] bytes.
fn input_text(seen: &Observed<'_>) -> Option<String> {
    let input = seen.fuzzed.input.as_ref()?;
    if input.content_encoding.is_some() || input.bytes.len() > MAX_INPUT_TEXT {
        return None;
    }
    let text = std::str::from_utf8(&input.bytes).ok()?;
    Some(seen.redactor.text(text).into_owned())
}

/// The exchange as evidence: the request line and headers as recorded
/// (redacted), the variant's body preview and what came back, redacted
/// with the request's redactor.
fn evidence(seen: &Observed<'_>, status: Option<u16>) -> Evidence {
    let redact = |text: &str| seen.redactor.text(text).into_owned();
    let (request, mut headers): (String, Vec<(String, String)>) = match &seen.record.exchange {
        Some(exchange) => (
            format!("{} {}", exchange.request.method, exchange.request.url),
            exchange
                .request
                .headers
                .iter()
                .map(|h| (h.name.clone(), redact(&h.value)))
                .collect(),
        ),
        None => (seen.file.to_owned(), Vec::new()),
    };
    if let Some(sent) = seen
        .fuzzed
        .input
        .as_ref()
        .and_then(|i| i.content_type.as_ref())
    {
        set_content_type(&mut headers, sent);
    }
    let response = match status {
        Some(status) => status.to_string(),
        None => redact(&crate::metrics::describe(seen.record)),
    };
    let (response_body, response_truncated) = seen
        .record
        .exchange
        .as_ref()
        .and_then(|e| e.response.body.as_deref())
        .map_or((String::new(), false), |body| {
            let (text, cut) = excerpt(&redact(body), RESPONSE_EXCERPT);
            let truncated = cut
                || seen
                    .record
                    .exchange
                    .as_ref()
                    .is_some_and(|e| e.response.body_truncated);
            (text, truncated)
        });
    Evidence {
        request: redact(&request),
        headers,
        variant: format!("{}: {}", seen.fuzzed.family, redact(&seen.fuzzed.label)),
        body: seen.fuzzed.preview(seen.redactor),
        response,
        response_body,
        response_truncated,
    }
}

/// The variant's `Content-Type` replaces the request's: it is what was
/// sent (the recorded request keeps its own headers).
fn set_content_type(headers: &mut Vec<(String, String)>, value: &str) {
    headers.retain(|(name, _)| !name.eq_ignore_ascii_case("content-type"));
    headers.push(("content-type".to_owned(), value.to_owned()));
}

/// The first `max` bytes of `text` (cut at a character boundary), and
/// whether it was cut.
fn excerpt(text: &str, max: usize) -> (String, bool) {
    if text.len() <= max {
        return (text.to_owned(), false);
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fuzzed(family: &str, invalid: bool, measure: Option<(&'static str, u64)>) -> Fuzzed {
        Fuzzed {
            family: family.into(),
            label: "x".into(),
            measure,
            expanded: 10,
            invalid,
            oversized: false,
            body: bytes::Bytes::from_static(b"{"),
            input: None,
        }
    }

    fn rules(findings: &FuzzFindings, f: &Fuzzed, status: Option<u16>) -> Vec<&'static str> {
        findings
            .rules(f, status, None)
            .into_iter()
            .map(|(r, _)| r)
            .collect()
    }

    /// The recorded request keeps its own headers: the evidence shows the
    /// `Content-Type` the variant was sent with.
    #[test]
    fn evidence_headers_carry_the_sent_content_type() {
        let mut headers = vec![
            ("Content-Type".to_owned(), "text/plain".to_owned()),
            ("x-a".to_owned(), "1".to_owned()),
        ];
        set_content_type(&mut headers, "application/json");
        assert_eq!(
            headers,
            [
                ("x-a".to_owned(), "1".to_owned()),
                ("content-type".to_owned(), "application/json".to_owned())
            ]
        );
    }

    #[test]
    fn response_excerpts_cut_at_a_character_boundary() {
        assert_eq!(excerpt("short", 10), ("short".to_owned(), false));
        assert_eq!(excerpt("ééé", 3), ("é".to_owned(), true));
    }

    #[test]
    fn statuses_map_to_rules() {
        let findings = FuzzFindings::new(Some(5));
        let invalid = fuzzed("duplicate-keys", true, None);
        assert_eq!(rules(&findings, &invalid, Some(201)), [ACCEPTED, OVERSIZED]);
        assert_eq!(rules(&findings, &invalid, Some(400)), Vec::<&str>::new());
        assert_eq!(rules(&findings, &invalid, Some(503)), [SERVER_ERROR]);
        let deep = fuzzed("deep-nesting", false, Some(("depth", 5000)));
        let lenient = FuzzFindings::new(None);
        assert_eq!(rules(&lenient, &deep, Some(200)), Vec::<&str>::new());
        let timeout = lenient.rules(&deep, None, Some(ErrorClass::Timeout));
        assert_eq!(timeout, [(TIMEOUT, Severity::High)]);
    }
}
