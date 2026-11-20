//! The plain-text end-of-run summary of a stress run: the step table and
//! the breakpoint of a breakpoint run, the recovery of a spike, the trends
//! of a soak, fuzzed against valid traffic, what each wire fault got
//! back, then the findings. Nothing here holds request data, so there
//! is nothing to redact.

use std::fmt::Write as _;

use rustman_report::exploration::Severity;

use crate::stress::breakpoint::{BreakpointResult, StopReason};
use crate::stress::recovery::{RecoveryResult, RecoveryStatus};
use crate::stress::run::FuzzOutcome;
use crate::stress::soak::{MIN_BUCKETS, SoakResult};
use crate::stress::spike::SpikeResult;
use crate::stress::wire::{WireFamily, WireResult};
use crate::stress::{Minimization, StressFinding, StressOutcome, format_rate};

/// The summary of `outcome`, ending with a newline.
pub fn render(outcome: &StressOutcome) -> String {
    let mut out = format!("stress {}", outcome.mode);
    if !outcome.slos.is_empty() {
        let _ = write!(out, " (SLOs: {})", outcome.slos.join(", "));
    }
    out.push('\n');
    if let Some(bp) = &outcome.breakpoint {
        breakpoint(&mut out, bp);
    }
    if let Some(spike) = &outcome.spike {
        spike_lines(&mut out, spike);
    }
    if let Some(soak) = &outcome.soak {
        soak_lines(&mut out, soak);
    }
    if let Some(fuzz) = &outcome.fuzz {
        fuzz_line(&mut out, fuzz, outcome.seed);
    }
    for result in &outcome.wire {
        wire_line(&mut out, result);
    }
    if let Some(killed) = &outcome.killed {
        let _ = writeln!(out, "{killed}");
    }
    recovery_line(&mut out, &outcome.recovery);
    if outcome.interrupted {
        out.push_str("interrupted (Ctrl+C)\n");
    } else if outcome.reached_max_duration {
        out.push_str("stopped at the run time cap (max_duration in rustman.toml [stress])\n");
    }
    findings(&mut out, &outcome.findings);
    out
}

fn breakpoint(out: &mut String, bp: &BreakpointResult) {
    out.push_str("step  offered   achieved  p95        errors   verdict\n");
    for (n, step) in bp.steps.iter().enumerate() {
        let _ = writeln!(
            out,
            "{:>4}  {:<8}  {:<8}  {:<9}  {:<7}  {}",
            n + 1,
            format_rate(step.rate),
            format_rate(step.achieved_rps),
            format!("{:.0}ms", step.p95_ms),
            format!("{:.2}%", step.error_rate * 100.0),
            step.verdict.name()
        );
    }
    let show = |rate: Option<f64>| rate.map_or_else(|| "none".to_owned(), format_rate);
    let _ = writeln!(out, "max_sustainable_rps: {}", show(bp.max_sustainable_rps));
    let _ = write!(out, "breaking_rps: {}", show(bp.breaking_rps));
    if let Some(mode) = bp.failure_mode {
        let _ = write!(out, " ({mode}");
        let first = bp
            .steps
            .iter()
            .find(|s| s.rate == bp.breaking_rps.unwrap_or(f64::NAN))
            .and_then(|s| s.first_failing.as_ref());
        if let Some(first) = first {
            let _ = write!(
                out,
                "; first failing endpoint: {}, p95 {:.0}ms, errors {:.2}%",
                first.name(),
                first.p95_ms,
                first.error_rate * 100.0
            );
        }
        out.push(')');
    }
    out.push('\n');
    if let Some(rate) = bp.rate_limited_at {
        let _ = writeln!(out, "rate limited at {} req/s", format_rate(rate));
    }
    match bp.stop {
        StopReason::CapReached => {
            let _ = writeln!(out, "no break found up to max_rps = {}", format_rate(bp.max_rps));
        }
        StopReason::ClientSaturated => out.push_str(
            "stopped: the client could not offer the next rate (client-saturated); raise the open-file limit or use a faster machine\n",
        ),
        _ => {}
    }
}

fn spike_lines(out: &mut String, spike: &SpikeResult) {
    let _ = writeln!(
        out,
        "baseline p95 {:.0}ms; during the spike {} requests, {} errors; peak p95 {:.0}ms",
        spike.baseline_p95_ms, spike.spike_requests, spike.spike_errors, spike.peak_p95_ms
    );
    match spike.recovered_after {
        Some(after) => {
            let _ = writeln!(out, "recovered in {}s", after.as_secs_f64().round());
        }
        None => out.push_str("not recovered before the end of the run\n"),
    }
}

fn soak_lines(out: &mut String, soak: &SoakResult) {
    let buckets = soak.p95_ms.len();
    if buckets < MIN_BUCKETS {
        let _ = writeln!(
            out,
            "p95 trend not available: too little data ({buckets} one-minute buckets, {MIN_BUCKETS} needed); baseline p95 {:.0}ms",
            soak.baseline_p95_ms
        );
        return;
    }
    let _ = writeln!(
        out,
        "p95 trend {:+.1} ms per hour (baseline {:.0}ms), error-rate trend {:+.2} points per hour",
        soak.p95_slope,
        soak.baseline_p95_ms,
        soak.error_slope * 100.0
    );
}

fn recovery_line(out: &mut String, recovery: &RecoveryResult) {
    match recovery.status {
        RecoveryStatus::Recovered => {
            let after = recovery.after.unwrap_or_default().as_secs_f64().round();
            let _ = writeln!(out, "recovery: recovered {after}s after load stopped");
        }
        RecoveryStatus::NotRecovered => out.push_str("recovery: not recovered (no-recovery)\n"),
        RecoveryStatus::Cancelled => out.push_str("recovery: check cancelled\n"),
        RecoveryStatus::Skipped => {
            let reason = recovery.reason.as_deref().unwrap_or("no health request");
            let _ = writeln!(out, "recovery: not checked ({reason})");
        }
    }
}

fn findings(out: &mut String, findings: &[StressFinding]) {
    if findings.is_empty() {
        return;
    }
    out.push_str("findings:\n");
    for finding in findings {
        let _ = writeln!(
            out,
            "  {} {}: {}",
            severity(finding.severity),
            finding.rule,
            finding.message
        );
        if let Some(min) = &finding.minimization {
            minimization_line(out, min);
        }
    }
}

fn minimization_line(out: &mut String, min: &Minimization) {
    if min.load_dependent {
        out.push_str("    load-dependent: the input did not reproduce the failure alone\n");
        return;
    }
    match &min.body {
        Some(body) => {
            let _ = writeln!(out, "    minimized ({} replays): {body}", min.replays);
        }
        None => out.push_str("    reproduces alone (not reduced: not a JSON body)\n"),
    }
}

fn fuzz_line(out: &mut String, fuzz: &FuzzOutcome, seed: u64) {
    let fuzzed = &fuzz.fuzzed.overall;
    if fuzz.sent_any() {
        let _ = write!(
            out,
            "fuzz: {:.0}% of requests replaced (seed {seed}): {} fuzzed, p95 {:.0}ms, errors {:.2}%",
            fuzz.ratio * 100.0,
            fuzzed.requests,
            fuzzed.latency.p95_ms,
            fuzzed.error_rate * 100.0
        );
    } else {
        let _ = write!(
            out,
            "fuzz: warning: no request was fuzzed (seed {seed}): no request of the plan has a JSON body to mutate, so valid traffic was not judged against fuzzing"
        );
    }
    if let (Some(base), Some(valid)) = (&fuzz.baseline, &fuzz.valid) {
        let _ = write!(
            out,
            "; valid p95 {:.0}ms (baseline {:.0}ms), errors {:.2}% (baseline {:.2}%)",
            valid.p95_ms,
            base.p95_ms,
            valid.error_rate * 100.0,
            base.error_rate * 100.0
        );
    }
    out.push('\n');
}

fn wire_line(out: &mut String, result: &WireResult) {
    let _ = write!(out, "wire {}: ", result.family);
    if result.family == WireFamily::SlowBody {
        let closed = result.slow.iter().filter(|c| c.is_some()).count();
        let _ = writeln!(
            out,
            "{} connections, {closed} closed by the server within {}s",
            result.slow.len(),
            result.hold.as_secs()
        );
        return;
    }
    let outcomes: Vec<String> = result.outcomes.iter().map(ToString::to_string).collect();
    let _ = writeln!(out, "{}", outcomes.join(", "));
}

fn severity(severity: Severity) -> &'static str {
    match severity {
        Severity::Critical => "critical",
        Severity::High => "high",
        Severity::Medium => "medium",
        Severity::Low => "low",
        Severity::Info => "info",
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::metrics::{LatencyKind, Metrics};
    use crate::stress::breakpoint::{Step, StepVerdict};
    use crate::stress::endpoint::FirstFailing;
    use crate::stress::{FailureMode, ModeKind};

    fn outcome(mode: ModeKind) -> StressOutcome {
        StressOutcome {
            mode,
            seed: 42,
            fuzz: None,
            wire: Vec::new(),
            slos: vec!["p95<1s".into(), "error_rate<5%".into()],
            summary: Metrics::new(LatencyKind::Corrected, Vec::new()).summary(),
            series: Vec::new(),
            windows: Vec::new(),
            breakpoint: None,
            spike: None,
            soak: None,
            findings: Vec::new(),
            killed: None,
            recovery: recovered(),
            client_saturated: false,
            interrupted: false,
            reached_max_duration: false,
            warnings: Vec::new(),
            load_started: Instant::now(),
            started_at: String::new(),
            finished_at: String::new(),
        }
    }

    fn recovered() -> RecoveryResult {
        RecoveryResult {
            status: RecoveryStatus::Recovered,
            after: Some(Duration::from_secs(3)),
            ..RecoveryResult::skipped(Some("api/health.rman.yaml".into()), "")
        }
    }

    fn step(rate: f64, verdict: StepVerdict) -> Step {
        Step {
            rate,
            verdict,
            failure_mode: None,
            achieved_rps: rate,
            p95_ms: 12.4,
            error_rate: 0.0,
            windows: Vec::new(),
            first_failing: None,
        }
    }

    #[test]
    fn a_breakpoint_shows_its_steps_and_bounds() {
        let mut broken = step(75.9375, StepVerdict::Broken);
        broken.p95_ms = 1450.0;
        broken.first_failing = Some(FirstFailing {
            file: "api/search.rman.yaml".into(),
            window: 3,
            p95_ms: 2100.0,
            error_rate: 0.01,
        });
        let mut run = outcome(ModeKind::Breakpoint);
        run.breakpoint = Some(BreakpointResult {
            steps: vec![step(50.625, StepVerdict::Ok), broken],
            max_sustainable_rps: Some(50.625),
            breaking_rps: Some(75.9375),
            failure_mode: Some(FailureMode::Latency),
            rate_limited_at: None,
            stop: StopReason::Refined,
            max_rps: 500.0,
        });
        assert_eq!(
            render(&run),
            "stress breakpoint (SLOs: p95<1s, error_rate<5%)\n\
             step  offered   achieved  p95        errors   verdict\n\
             \x20  1  50.62     50.62     12ms       0.00%    ok\n\
             \x20  2  75.94     75.94     1450ms     0.00%    broken\n\
             max_sustainable_rps: 50.62\n\
             breaking_rps: 75.94 (latency; first failing endpoint: search, p95 2100ms, errors 1.00%)\n\
             recovery: recovered 3s after load stopped\n"
        );
    }

    #[test]
    fn the_cap_and_rate_limits_are_stated() {
        let mut run = outcome(ModeKind::Breakpoint);
        run.breakpoint = Some(BreakpointResult {
            steps: vec![step(500.0, StepVerdict::RateLimited)],
            max_sustainable_rps: Some(500.0),
            breaking_rps: None,
            failure_mode: None,
            rate_limited_at: Some(200.0),
            stop: StopReason::CapReached,
            max_rps: 500.0,
        });
        let text = render(&run);
        assert!(text.contains("breaking_rps: none\n"), "{text}");
        assert!(text.contains("rate limited at 200 req/s\n"), "{text}");
        assert!(
            text.ends_with(
                "no break found up to max_rps = 500\nrecovery: recovered 3s after load stopped\n"
            ),
            "{text}"
        );
    }

    #[test]
    fn spike_and_soak_results_and_findings_are_shown() {
        let mut run = outcome(ModeKind::Spike);
        run.spike = Some(SpikeResult {
            baseline_p95_ms: 31.0,
            spike_requests: 300,
            spike_errors: 4,
            peak_p95_ms: 820.0,
            recovered_after: Some(Duration::from_secs(14)),
        });
        let text = render(&run);
        assert!(text.ends_with(
            "baseline p95 31ms; during the spike 300 requests, 4 errors; peak p95 820ms\nrecovered in 14s\nrecovery: recovered 3s after load stopped\n"
        ), "{text}");
        let mut soak = outcome(ModeKind::Soak);
        soak.slos.clear();
        soak.soak = Some(SoakResult {
            p95_ms: vec![80.0, 81.0, 82.0],
            error_rates: vec![0.0; 3],
            baseline_p95_ms: 80.0,
            p95_slope: 30.25,
            error_slope: 0.0,
            findings: Vec::new(),
        });
        soak.findings.push(StressFinding::new(
            "latency-drift",
            Severity::Medium,
            "p95 rose".into(),
        ));
        soak.interrupted = true;
        assert_eq!(
            render(&soak),
            "stress soak\n\
             p95 trend +30.2 ms per hour (baseline 80ms), error-rate trend +0.00 points per hour\n\
             recovery: recovered 3s after load stopped\n\
             interrupted (Ctrl+C)\n\
             findings:\n  medium latency-drift: p95 rose\n"
        );
    }

    /// Under a minute of data (fewer buckets than a trend needs), the
    /// slope of one or two points means nothing: it is not shown.
    #[test]
    fn a_short_soak_says_its_trend_is_not_available() {
        let mut soak = outcome(ModeKind::Soak);
        soak.slos.clear();
        for buckets in [0, 1, MIN_BUCKETS - 1] {
            soak.soak = Some(SoakResult {
                p95_ms: vec![80.0; buckets],
                error_rates: vec![0.0; buckets],
                baseline_p95_ms: 80.0,
                p95_slope: 4800.0,
                error_slope: 0.0,
                findings: Vec::new(),
            });
            let text = render(&soak);
            assert!(
                text.contains(&format!(
                    "\np95 trend not available: too little data ({buckets} one-minute buckets, 3 needed); baseline p95 80ms\n"
                )),
                "{text}"
            );
            assert!(!text.contains("per hour"), "{text}");
        }
    }

    /// A plan without a JSON body to mutate fuzzes nothing: the summary
    /// warns instead of claiming a share of requests was replaced.
    #[test]
    fn a_run_that_fuzzed_nothing_warns_instead_of_claiming_a_ratio() {
        use crate::stress::fuzz::impact::Valid;
        let mut run = outcome(ModeKind::Fuzz);
        run.slos.clear();
        let valid = |p95_ms| Valid {
            requests: 10,
            p95_ms,
            error_rate: 0.0,
        };
        run.fuzz = Some(FuzzOutcome {
            ratio: 0.2,
            fuzzed: Metrics::new(LatencyKind::Corrected, Vec::new()).summary(),
            baseline_ticks: 30,
            baseline: Some(valid(100.0)),
            valid: Some(valid(260.0)),
        });
        let text = render(&run);
        assert!(
            text.contains("fuzz: warning: no request was fuzzed (seed 42): no request of the plan has a JSON body to mutate"),
            "{text}"
        );
        assert!(!text.contains("of requests replaced"), "{text}");
    }

    #[test]
    fn a_kill_a_failed_recovery_and_minimized_inputs_are_shown() {
        use crate::stress::kill::{DownReason, Killed};
        let mut run = outcome(ModeKind::Fuzz);
        run.slos.clear();
        run.killed = Some(Killed {
            rps: 180.0,
            reason: DownReason::Connection,
        });
        run.recovery.status = RecoveryStatus::NotRecovered;
        let mut crash = StressFinding::new(
            "server-error-on-malformed-input",
            Severity::High,
            "mutation input on api/o.rman.yaml answered 500 (once)".into(),
        );
        crash.minimization = Some(Minimization {
            body: Some("{\"qty\":\"2\"}".into()),
            content_type: None,
            replays: 4,
            load_dependent: false,
        });
        let mut loaded = crash.clone();
        loaded.minimization = Some(Minimization {
            load_dependent: true,
            ..crash.minimization.clone().unwrap()
        });
        run.findings = vec![crash, loaded];
        let text = render(&run);
        assert!(text.contains("killed: target down at 180 req/s (connection errors above 50% for 5s)\nrecovery: not recovered (no-recovery)\n"), "{text}");
        assert!(
            text.contains("\n    minimized (4 replays): {\"qty\":\"2\"}\n"),
            "{text}"
        );
        assert!(
            text.ends_with("\n    load-dependent: the input did not reproduce the failure alone\n"),
            "{text}"
        );
    }

    #[test]
    fn fuzz_and_wire_lines_are_shown() {
        use crate::stress::fuzz::impact::Valid;
        use crate::stress::wire::WireOutcome;
        let mut run = outcome(ModeKind::Fuzz);
        run.slos.clear();
        let valid = |p95_ms| Valid {
            requests: 10,
            p95_ms,
            error_rate: 0.0,
        };
        let mut fuzzed = Metrics::new(LatencyKind::Corrected, Vec::new()).summary();
        fuzzed.overall.requests = 5;
        run.fuzz = Some(FuzzOutcome {
            ratio: 0.2,
            fuzzed,
            baseline_ticks: 30,
            baseline: Some(valid(100.0)),
            valid: Some(valid(260.0)),
        });
        run.wire = vec![
            WireResult {
                family: WireFamily::ClTeConflict,
                outcomes: vec![WireOutcome::Status(400), WireOutcome::Closed],
                slow: Vec::new(),
                hold: std::time::Duration::from_secs(60),
            },
            WireResult {
                family: WireFamily::SlowBody,
                outcomes: Vec::new(),
                slow: vec![None, Some(std::time::Duration::from_secs(15))],
                hold: std::time::Duration::from_secs(60),
            },
        ];
        assert_eq!(
            render(&run),
            "stress fuzz\n\
             fuzz: 20% of requests replaced (seed 42): 5 fuzzed, p95 0ms, errors 0.00%; valid p95 260ms (baseline 100ms), errors 0.00% (baseline 0.00%)\n\
             wire cl-te-conflict: 400, closed without a response\n\
             wire slow-body: 2 connections, 1 closed by the server within 60s\n\
             recovery: recovered 3s after load stopped\n"
        );
    }
}
