//! The `kind: stress` report of a run (test-reports "Stress report
//! content"), built from its [`StressOutcome`]. Every text in it is
//! already redacted: findings, evidence and inputs were redacted with the
//! request's redactor when they were recorded.
//!
//! Each finding with an input also carries its regression request file
//! (the finding's request file, with the minimized input as its body and
//! the status a correct API answers), so `rustman stress export` works
//! from the report alone. Secret placeholders in an input become
//! `{{name}}` references again, so no export holds a secret.

use std::collections::BTreeMap;
use std::time::Duration;

use rustman_core::Workspace;
use rustman_core::format::write::request_to_yaml;
use rustman_core::model::{
    Assertion, Body, HttpRequest, JsonContent, NameValue, Request, StatusExpectation,
};
use rustman_report::bench::BenchSecond;
use rustman_report::exploration::{Evidence, ReportFinding, Reproduction, SeverityCounts};
use rustman_report::stress::{
    BreakpointSection, FailingEndpoint, KIND_STRESS, MinimizedInput, RecoveryProbe,
    RecoverySection, SoakSection, SpikeSection, StressProfile, StressReport, StressStep,
    StressSummary, WireSection,
};

use crate::stress::breakpoint::{BreakpointResult, StopReason};
use crate::stress::recovery::RecoveryResult;
use crate::stress::window::WindowVerdict;
use crate::stress::wire::WireFamily;
use crate::stress::{ModeSpec, StressFinding, StressOutcome};

/// What the report says beyond the outcome.
pub struct StressReportContext<'a> {
    /// `str-<date>-<hex>`.
    pub id: &'a str,
    pub target: &'a str,
    pub environment: Option<&'a str>,
    pub mode: &'a ModeSpec,
    pub fuzz_ratio: Option<f64>,
    pub malformed: &'a [String],
    pub wire: &'a [String],
    /// The workspace, for the regression request files.
    pub workspace: &'a Workspace,
}

/// The most windows a step of a mode without steps spans (a minute).
const SEGMENT_WINDOWS: usize = 12;

/// The report of `outcome`.
pub fn stress_report(outcome: &StressOutcome, ctx: &StressReportContext<'_>) -> StressReport {
    let findings = report_findings(&outcome.findings, ctx);
    let mut counts = SeverityCounts::default();
    for f in &findings {
        counts.add(f.finding.severity);
    }
    let fuzzed = outcome.fuzz.as_ref().map(|f| f.fuzzed.overall.clone());
    StressReport {
        schema: rustman_report::SCHEMA.to_owned(),
        kind: KIND_STRESS.to_owned(),
        rustman_version: env!("CARGO_PKG_VERSION").to_owned(),
        id: ctx.id.to_owned(),
        started_at: outcome.started_at.clone(),
        finished_at: outcome.finished_at.clone(),
        target: ctx.target.to_owned(),
        environment: ctx.environment.map(str::to_owned),
        mode: outcome.mode.name().to_owned(),
        profile: profile(ctx, &outcome.recovery),
        seed: outcome.seed,
        slos: outcome.slos.clone(),
        overall: outcome.summary.overall.clone(),
        files: outcome.summary.files.clone(),
        summary: StressSummary {
            findings: counts,
            requests: outcome.summary.overall.requests,
            fuzzed_requests: fuzzed.as_ref().map_or(0, |f| f.requests),
            duration_ms: outcome.summary.elapsed_ms.round() as u64,
        },
        fuzzed,
        series: outcome.series.clone(),
        fuzzed_series: Vec::<BenchSecond>::new(),
        steps: steps(outcome),
        breakpoint: outcome.breakpoint.as_ref().map(breakpoint),
        rate_limited_at: outcome.breakpoint.as_ref().and_then(|b| b.rate_limited_at),
        killed: outcome.killed.map(|k| rustman_report::stress::Killed {
            rps: k.rps,
            reason: k.reason.to_string(),
            message: k.to_string(),
        }),
        recovery: recovery(&outcome.recovery),
        spike: outcome.spike.as_ref().map(|s| SpikeSection {
            baseline_p95_ms: s.baseline_p95_ms,
            spike_requests: s.spike_requests,
            spike_errors: s.spike_errors,
            peak_p95_ms: s.peak_p95_ms,
            recovered_after_ms: s.recovered_after.map(ms),
        }),
        soak: outcome.soak.as_ref().map(|s| SoakSection {
            p95_ms: s.p95_ms.clone(),
            error_rates: s.error_rates.clone(),
            baseline_p95_ms: s.baseline_p95_ms,
            p95_slope: s.p95_slope,
            error_slope: s.error_slope,
        }),
        wire: outcome
            .wire
            .iter()
            .map(|w| WireSection {
                family: w.family.name().to_owned(),
                outcomes: w.outcomes.iter().map(ToString::to_string).collect(),
            })
            .collect(),
        findings,
        client_saturated: outcome.client_saturated,
        interrupted: outcome.interrupted,
        warnings: outcome.warnings.clone(),
    }
}

fn ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

fn profile(ctx: &StressReportContext<'_>, recovery: &RecoveryResult) -> StressProfile {
    let secs = |d: Duration| d.as_secs_f64() * 1000.0;
    let settings: BTreeMap<String, f64> = match ctx.mode {
        ModeSpec::Breakpoint(b) => [
            ("start_rps", b.start_rps),
            ("step_factor", b.step_factor),
            ("step_duration_ms", secs(b.step_duration)),
            ("refine", f64::from(b.refine)),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect(),
        ModeSpec::Spike(s) => [
            ("baseline_rps", s.baseline_rps),
            ("baseline_ms", secs(s.baseline)),
            ("spike_factor", s.spike_factor),
            ("spike_duration_ms", secs(s.spike_duration)),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect(),
        ModeSpec::Soak(s) => [
            ("rps", s.rps),
            ("duration_ms", secs(s.duration)),
            ("drift_threshold", s.drift_threshold),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect(),
        ModeSpec::Fuzz(f) => [("rps", f.rps), ("duration_ms", secs(f.duration))]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v))
            .collect(),
    };
    StressProfile {
        settings,
        fuzz_ratio: ctx.fuzz_ratio,
        malformed: ctx.malformed.to_vec(),
        wire: ctx.wire.to_vec(),
        health: recovery.health.clone(),
    }
}

/// The step table: a breakpoint's steps, or stretches of windows at one
/// rate (at most a minute each).
fn steps(outcome: &StressOutcome) -> Vec<StressStep> {
    let windows = &outcome.windows;
    if let Some(bp) = &outcome.breakpoint {
        return bp
            .steps
            .iter()
            .map(|step| {
                let of: Vec<&WindowVerdict> = step
                    .windows
                    .iter()
                    .filter_map(|i| windows.get(*i))
                    .collect();
                let mut row = row(step.rate, &of, windows);
                row.achieved_rps = step.achieved_rps;
                row.p95_ms = step.p95_ms;
                row.error_rate = step.error_rate;
                row.verdict = step.verdict.name().to_owned();
                row.failure_mode = step.failure_mode.map(|m| m.name().to_owned());
                row
            })
            .collect();
    }
    let mut rows = Vec::new();
    let mut run: Vec<&WindowVerdict> = Vec::new();
    for window in windows {
        let same = run
            .first()
            .is_some_and(|w| (w.rate - window.rate).abs() < 1e-9);
        if !run.is_empty() && (!same || run.len() >= SEGMENT_WINDOWS) {
            rows.push(segment(&run, windows));
            run.clear();
        }
        run.push(window);
    }
    if !run.is_empty() {
        rows.push(segment(&run, windows));
    }
    rows
}

fn segment(windows: &[&WindowVerdict], all: &[WindowVerdict]) -> StressStep {
    let offered = windows[0].rate * windows[0].stats.requests_per_iteration.max(1) as f64;
    let mut row = row(offered, windows, all);
    let failed = |w: &&WindowVerdict| w.failed() && !w.client_saturated;
    let broken = windows.windows(2).any(|p| failed(&p[0]) && failed(&p[1]));
    row.verdict = if broken { "broken" } else { "ok" }.to_owned();
    row.failure_mode = broken
        .then(|| windows.iter().find_map(|w| w.failure_mode))
        .flatten()
        .map(|m| m.name().to_owned());
    row
}

/// A row over `windows` (of the run's `all`): the highest percentiles,
/// summed errors.
fn row(offered: f64, windows: &[&WindowVerdict], all: &[WindowVerdict]) -> StressStep {
    let n = windows.len().max(1) as f64;
    let max = |f: fn(&WindowVerdict) -> f64| windows.iter().map(|w| f(w)).fold(0.0, f64::max);
    let mut errors: BTreeMap<String, u64> = BTreeMap::new();
    let (mut requests, mut failed) = (0, 0);
    for w in windows {
        requests += w.stats.overall.requests;
        for (class, count) in &w.stats.overall.errors {
            *errors.entry(class.clone()).or_default() += count;
            failed += count;
        }
    }
    let start = windows.first().map_or(0, |w| window_start_ms(all, w.index));
    let end = windows
        .last()
        .map_or(0, |w| window_start_ms(all, w.index) + ms(w.stats.length));
    StressStep {
        offered_rps: offered,
        achieved_rps: windows.iter().map(|w| w.stats.completed_rps()).sum::<f64>() / n,
        p50_ms: max(|w| w.stats.overall.latency.p50_ms),
        p95_ms: max(|w| w.stats.overall.latency.p95_ms),
        p99_ms: max(|w| w.stats.overall.latency.p99_ms),
        error_rate: if requests == 0 {
            0.0
        } else {
            failed as f64 / requests as f64
        },
        errors,
        verdict: String::new(),
        failure_mode: None,
        start_ms: start,
        end_ms: end,
    }
}

/// When window `index` started: every window but the last (which a kill
/// can cut short) has the length of the first.
fn window_start_ms(all: &[WindowVerdict], index: usize) -> u64 {
    all.first().map_or(0, |w| ms(w.stats.length) * index as u64)
}

fn breakpoint(bp: &BreakpointResult) -> BreakpointSection {
    let first = bp
        .steps
        .iter()
        .find(|s| Some(s.rate) == bp.breaking_rps)
        .and_then(|s| s.first_failing.as_ref());
    BreakpointSection {
        max_sustainable_rps: bp.max_sustainable_rps,
        breaking_rps: bp.breaking_rps,
        failure_mode: bp.failure_mode.map(|m| m.name().to_owned()),
        first_failing_endpoint: first.map(|f| FailingEndpoint {
            file: f.file.clone(),
            p95_ms: f.p95_ms,
            error_rate: f.error_rate,
        }),
        stop: stop_name(bp.stop).to_owned(),
        max_rps: bp.max_rps,
    }
}

fn stop_name(stop: StopReason) -> &'static str {
    match stop {
        StopReason::Refined => "refined",
        StopReason::CapReached => "cap-reached",
        StopReason::BrokeAtStart => "broke-at-start",
        StopReason::ClientSaturated => "client-saturated",
        StopReason::MaxDuration => "max-duration",
        StopReason::Interrupted => "interrupted",
        StopReason::Killed => "killed",
    }
}

fn recovery(r: &RecoveryResult) -> RecoverySection {
    let last = r.last();
    RecoverySection {
        status: r.status.name().to_owned(),
        after_ms: r.after.map(ms),
        baseline_ms: r.baseline.map(|b| b.as_secs_f64() * 1000.0),
        health: r.health.clone(),
        reason: r.reason.clone(),
        last_status: last.and_then(|p| p.status),
        last_latency_ms: last.map(|p| p.latency.as_secs_f64() * 1000.0),
        timeline: r
            .timeline
            .iter()
            .map(|p| RecoveryProbe {
                offset_ms: ms(p.offset),
                status: p.probe.status,
                latency_ms: p.probe.latency.as_secs_f64() * 1000.0,
                good: p.good,
            })
            .collect(),
    }
}

/// The findings by severity then rule, numbered `f001`, `f002`, ...
fn report_findings(
    findings: &[StressFinding],
    ctx: &StressReportContext<'_>,
) -> Vec<rustman_report::stress::StressFinding> {
    let mut sorted: Vec<&StressFinding> = findings.iter().collect();
    sorted.sort_by(|a, b| (a.severity, a.rule).cmp(&(b.severity, b.rule)));
    sorted
        .into_iter()
        .enumerate()
        .map(|(n, f)| report_finding(&format!("f{:03}", n + 1), f, ctx))
        .collect()
}

fn report_finding(
    id: &str,
    f: &StressFinding,
    ctx: &StressReportContext<'_>,
) -> rustman_report::stress::StressFinding {
    let (description, remediation) = rule_text(f.rule);
    let operation = operation(f);
    let finding = ReportFinding {
        id: id.to_owned(),
        rule: f.rule.to_owned(),
        severity: f.severity,
        operation: operation.clone(),
        description: description.to_owned(),
        remediation: remediation.to_owned(),
        message: f.message.clone(),
        occurrences: 1,
        probes: f.family.iter().cloned().collect(),
        instance_pointer: None,
        schema_pointer: None,
        evidence: evidence(f),
        reproduction: reproduction(f, ctx),
    };
    rustman_report::stress::StressFinding {
        finding,
        family: f.family.clone(),
        minimized: f.minimization.as_ref().map(|m| MinimizedInput {
            body: m.body.clone(),
            content_type: m.content_type.clone(),
            replays: m.replays,
            load_dependent: m.load_dependent,
        }),
    }
}

/// The request file, `wire <family>` for a wire fault, else the run.
fn operation(f: &StressFinding) -> String {
    if let Some(file) = &f.file {
        return file.clone();
    }
    match (&f.family, wire_family(f)) {
        (Some(family), Some(_)) => format!("wire {family}"),
        _ => "run".to_owned(),
    }
}

/// The method and URL of `METHOD URL`, or of a wire fault's request line
/// `METHOD TARGET HTTP/1.1` (the version is not part of the URL).
fn method_and_url(request: &str) -> (String, String) {
    let Some((method, rest)) = request.split_once(' ') else {
        return (String::new(), request.to_owned());
    };
    let url = match rest.rsplit_once(' ') {
        Some((target, version)) if version.starts_with("HTTP/") => target,
        _ => rest,
    };
    (method.to_owned(), url.to_owned())
}

fn wire_family(f: &StressFinding) -> Option<WireFamily> {
    f.family
        .as_deref()?
        .parse()
        .ok()
        .filter(|_| f.file.is_none())
}

fn evidence(f: &StressFinding) -> Evidence {
    let Some(e) = &f.evidence else {
        return Evidence::default();
    };
    let (method, url) = method_and_url(&e.request);
    // The exchange as it happened: the body sent (the minimized input has
    // its own `minimized` field) and the response's body. Without a
    // response, the body says what happened instead.
    let status = e.response.parse().unwrap_or(0);
    let (body, truncated) = if status == 0 {
        (e.response.clone(), false)
    } else {
        (e.response_body.clone(), e.response_truncated)
    };
    Evidence {
        method,
        url,
        request_headers: e.headers.clone(),
        request_body: f
            .input
            .clone()
            .or_else(|| (!e.body.is_empty()).then(|| e.body.clone())),
        status,
        response_headers: Vec::new(),
        body,
        truncated,
        trace: None,
    }
}

/// What a fixed API answers, and why, per rule.
fn rule_text(rule: &str) -> (&'static str, &'static str) {
    match rule {
        "server-error-on-malformed-input" => (
            "A malformed or mutated input made the server fail with a 5xx.",
            "Validate input and answer 4xx instead of failing.",
        ),
        "timeout-on-malformed-input" => (
            "A malformed or mutated input got no answer in time.",
            "Bound parsing work and answer 4xx to input it cannot handle.",
        ),
        "accepted-malformed-input" => (
            "A malformed input was accepted with a 2xx.",
            "Reject invalid input with a 4xx.",
        ),
        "oversized-input-accepted" => (
            "A body above the documented or expected size was accepted.",
            "Enforce size limits and answer 413 or another 4xx.",
        ),
        "ambiguous-framing-accepted" => (
            "A request with conflicting framing was accepted.",
            "Answer 400 to requests with both Content-Length and Transfer-Encoding.",
        ),
        "connection-dropped-without-response" => (
            "The server closed the connection without a response.",
            "Answer 431 (or 400) before closing the connection.",
        ),
        "no-slow-client-timeout" => (
            "The server kept slow clients connected for the whole test.",
            "Time out clients that send their body too slowly.",
        ),
        "fuzz-degrades-valid-traffic" => (
            "Malformed traffic degraded the service for valid clients.",
            "Reject bad input early and isolate its cost from valid traffic.",
        ),
        "latency-drift" => (
            "Latency kept rising under steady load (a possible leak).",
            "Look for growing queues, caches or connections under steady load.",
        ),
        "error-drift" => (
            "The error rate kept rising under steady load.",
            "Look for exhausted resources under steady load.",
        ),
        "no-recovery" => (
            "The target did not recover after load stopped.",
            "Make the service shed load and recover without a restart.",
        ),
        _ => ("A stress finding.", ""),
    }
}

/// The regression of a finding with an input: its request file with the
/// input as body and the status a correct API answers; a wire fault as
/// comments. Findings without an input have none.
fn reproduction(f: &StressFinding, ctx: &StressReportContext<'_>) -> Option<Reproduction> {
    if let Some(family) = wire_family(f) {
        return Some(Reproduction {
            curl: String::new(),
            request: wire_comments(f, family, ctx.target),
        });
    }
    let file = f.file.as_deref()?;
    let body = f
        .minimization
        .as_ref()
        .and_then(|m| m.body.clone())
        .or_else(|| f.input.clone())?;
    let http = ctx.workspace.requests().iter().find_map(|r| {
        let path = r.entry.path.to_string_lossy().replace('\\', "/");
        match (r.request(), path == file) {
            (Request::Http(http), true) => Some(http.as_ref().clone()),
            _ => None,
        }
    })?;
    let content_type = f
        .minimization
        .as_ref()
        .and_then(|m| m.content_type.clone())
        .or_else(|| sent_content_type(f))
        .or_else(|| request_content_type(&http));
    Some(Reproduction {
        curl: String::new(),
        request: regression_file(http, f, &references(&body), content_type.as_deref()),
    })
}

/// The `Content-Type` the fuzzed request was sent with.
fn sent_content_type(f: &StressFinding) -> Option<String> {
    f.evidence
        .as_ref()?
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        .map(|(_, value)| value.clone())
}

/// The `Content-Type` the request file sends: its header, else that of
/// its body type.
fn request_content_type(request: &HttpRequest) -> Option<String> {
    let header = request
        .headers
        .iter()
        .flatten()
        .find(|h| h.name.eq_ignore_ascii_case("content-type"))
        .map(|h| h.value.clone());
    header.or_else(|| match request.body {
        Some(Body::Json(_)) => Some("application/json".to_owned()),
        Some(Body::Form(_)) => Some("application/x-www-form-urlencoded".to_owned()),
        _ => None,
    })
}

/// `request` with `body` and the assertion of `f`'s rule.
fn regression_file(
    mut request: HttpRequest,
    f: &StressFinding,
    body: &str,
    content_type: Option<&str>,
) -> String {
    request.name = format!("{}: {}", f.rule, request.name);
    let json = content_type.is_none_or(|c| c.contains("json"))
        && serde_json::from_str::<serde_json::Value>(body).is_ok();
    request.body = Some(if json {
        Body::Json(JsonContent::Text(body.to_owned()))
    } else {
        Body::Text(body.to_owned())
    });
    if let (false, Some(content_type)) = (json, content_type) {
        let mut headers: Vec<NameValue> = request
            .headers
            .take()
            .unwrap_or_default()
            .into_iter()
            .filter(|h| !h.name.eq_ignore_ascii_case("content-type"))
            .collect();
        headers.push(NameValue::new("Content-Type", content_type));
        request.headers = Some(headers);
    }
    request.assertions = Some(vec![Assertion::Status(StatusExpectation::Class(4))]);
    request.captures = None;
    request_to_yaml(&Request::Http(Box::new(request)))
}

/// `«redacted:name»` placeholders as `{{name}}` references.
fn references(text: &str) -> String {
    const OPEN: &str = "\u{ab}redacted:";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(OPEN) {
        let after = &rest[start + OPEN.len()..];
        let Some(end) = after.find('\u{bb}') else {
            break;
        };
        out.push_str(&rest[..start]);
        out.push_str(&format!("{{{{{}}}}}", &after[..end]));
        rest = &after[end + '\u{bb}'.len_utf8()..];
    }
    out.push_str(rest);
    out
}

/// A wire fault as comments: a request file cannot express it.
fn wire_comments(f: &StressFinding, family: WireFamily, target: &str) -> String {
    let request = f.evidence.as_ref().map_or("", |e| e.request.as_str());
    format!(
        "# rustman stress regression: {rule} ({family})\n\
         # A request file cannot express this wire fault. Send it again with:\n\
         #   rustman stress fuzz <path> --target {target} --rps 1 --wire {family} --allow-wire-faults\n\
         # Request line sent: {request}\n\
         # Expected: status {status}\n\
         # Observed: {message}\n",
        rule = f.rule,
        status = family.expected_status(),
        message = f.message.replace(['\n', '\r'], " "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(index: usize, secs: u64, requests: u64, errors: u64) -> WindowVerdict {
        let mut overall = rustman_report::bench::StatsSummary {
            requests,
            ..Default::default()
        };
        overall.errors.insert("connection".into(), errors);
        WindowVerdict {
            index,
            rate: 10.0,
            stats: crate::stress::window::WindowStats {
                length: Duration::from_secs(secs),
                scheduled: requests,
                requests_per_iteration: 1,
                overall,
                files: Vec::new(),
                throttled: 0,
                throttled_5xx: 0,
                fuzzed: 0,
                in_flight: 0,
            },
            failures: Vec::new(),
            failure_mode: None,
            throttled: false,
            client_saturated: false,
        }
    }

    /// Malformed JSON is exported as text but keeps the `Content-Type` it
    /// was fuzzed with, so a server that rejects `text/plain` cannot pass.
    #[test]
    fn a_malformed_json_regression_keeps_the_sent_content_type() {
        let request: HttpRequest = rustman_core::format::from_yaml(
            std::path::Path::new("post.yaml"),
            "version: 1\nkind: http\nname: post\nmethod: POST\nurl: \"{{baseUrl}}/items\"\n",
        )
        .unwrap();
        let mut finding = StressFinding::new(
            "accepted-malformed-input",
            rustman_report::exploration::Severity::Medium,
            String::new(),
        );
        finding.evidence = Some(crate::stress::Evidence {
            headers: vec![("content-type".into(), "application/json".into())],
            ..Default::default()
        });
        let content_type = sent_content_type(&finding);
        assert_eq!(content_type.as_deref(), Some("application/json"));
        let yaml = regression_file(request, &finding, "{'id': 1}", content_type.as_deref());
        assert!(yaml.contains("Content-Type"), "{yaml}");
        assert!(yaml.contains("application/json"), "{yaml}");
        assert!(!yaml.contains("text/plain"), "{yaml}");
    }

    /// A json-syntax finding whose evidence lost its `Content-Type` still
    /// exports with the request file's own (a JSON body's), so the
    /// regression reaches the JSON parser instead of sending text/plain.
    #[test]
    fn a_regression_without_a_sent_content_type_keeps_the_request_one() {
        let request: HttpRequest = rustman_core::format::from_yaml(
            std::path::Path::new("post.yaml"),
            "version: 1\nkind: http\nname: post\nmethod: POST\nurl: \"{{baseUrl}}/items\"\nbody:\n  type: json\n  content: {qty: 2}\n",
        )
        .unwrap();
        let mut finding = StressFinding::new(
            "accepted-malformed-input",
            rustman_report::exploration::Severity::Medium,
            String::new(),
        );
        finding.evidence = Some(crate::stress::Evidence::default());
        assert_eq!(sent_content_type(&finding), None);
        let content_type = request_content_type(&request);
        assert_eq!(content_type.as_deref(), Some("application/json"));
        let yaml = regression_file(request, &finding, "{\"qty\": 2,", content_type.as_deref());
        assert!(yaml.contains("Content-Type"), "{yaml}");
        assert!(yaml.contains("application/json"), "{yaml}");
    }

    /// The evidence is the exchange as it happened: the status, the
    /// response's body and the body sent; the minimized input is apart.
    #[test]
    fn evidence_keeps_the_response_body_and_the_sent_input() {
        let mut finding = StressFinding::new(
            "server-error-on-malformed-input",
            rustman_report::exploration::Severity::High,
            String::new(),
        );
        finding.input = Some("{\"qty\":\"2\",\"sku\":\"A1\"}".into());
        finding.evidence = Some(crate::stress::Evidence {
            request: "POST http://127.0.0.1:1/orders".into(),
            body: "{\\\"qty\\\":\\\"2\\\"".into(),
            response: "500".into(),
            response_body: "internal error: token \u{ab}redacted:api_token\u{bb}".into(),
            response_truncated: true,
            ..Default::default()
        });
        finding.minimization = Some(crate::stress::Minimization {
            body: Some("{\"qty\":\"2\"}".into()),
            content_type: None,
            replays: 4,
            load_dependent: false,
        });
        let e = evidence(&finding);
        assert_eq!(e.status, 500);
        assert_eq!(
            e.body,
            "internal error: token \u{ab}redacted:api_token\u{bb}"
        );
        assert!(e.truncated);
        assert_eq!(
            e.request_body.as_deref(),
            Some("{\"qty\":\"2\",\"sku\":\"A1\"}")
        );

        // No response: the body says what happened instead.
        finding.evidence.as_mut().unwrap().response = "timed out".into();
        let e = evidence(&finding);
        assert_eq!((e.status, e.body.as_str()), (0, "timed out"));
    }

    #[test]
    fn a_request_line_splits_into_method_and_url_without_the_version() {
        let split = |r: &str| method_and_url(r);
        assert_eq!(split("POST / HTTP/1.1"), ("POST".into(), "/".into()));
        assert_eq!(
            split("GET http://127.0.0.1:1/a?b=c"),
            ("GET".into(), "http://127.0.0.1:1/a?b=c".into())
        );
        assert_eq!(split("broken"), (String::new(), "broken".into()));
    }

    /// A window a kill cut short (the last) starts where the full ones end.
    #[test]
    fn a_cut_short_window_is_timed_after_the_full_ones() {
        let all = [
            window(0, 5, 50, 0),
            window(1, 5, 50, 0),
            window(2, 2, 20, 20),
        ];
        let last = row(10.0, &[&all[2]], &all);
        assert_eq!((last.start_ms, last.end_ms), (10_000, 12_000));
        assert_eq!(last.error_rate, 1.0);
        assert_eq!(last.errors["connection"], 20);
        let only = [window(0, 2, 20, 20)];
        let first = row(10.0, &[&only[0]], &only);
        assert_eq!((first.start_ms, first.end_ms), (0, 2_000));
    }

    /// A fuzzed run's row compares like with like: the offered rate counts
    /// every request, so the achieved one counts fuzzed requests too.
    #[test]
    fn a_fuzzed_row_counts_fuzzed_requests_as_achieved() {
        let mut fuzzing = window(0, 10, 80, 0);
        fuzzing.stats.scheduled = 100;
        fuzzing.stats.fuzzed = 20;
        let all = [fuzzing];
        let row = segment(&[&all[0]], &all);
        assert_eq!(row.offered_rps, 10.0);
        assert_eq!(row.achieved_rps, 10.0);
    }

    #[test]
    fn placeholders_become_references() {
        assert_eq!(
            references("{\"t\":\"\u{ab}redacted:api_token\u{bb}\",\"n\":1}"),
            "{\"t\":\"{{api_token}}\",\"n\":1}"
        );
        assert_eq!(references("plain"), "plain");
        assert_eq!(references("\u{ab}redacted:broken"), "\u{ab}redacted:broken");
    }
}
