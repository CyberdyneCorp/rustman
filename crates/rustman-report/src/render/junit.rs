//! JUnit XML (via `quick-junit`).
//!
//! - A run: one `<testcase>` per request, with a `<failure>` for a failed
//!   request, an `<error>` for an errored one and `<skipped>` for a skipped
//!   one. A failed snapshot's diff is in the failure message; recorded and
//!   updated snapshots go to system-out.
//! - An exploration: one `<testsuite>` per tag and one `<testcase>` per
//!   operation. Findings at or above `--fail-on` become a `<failure>` whose
//!   type is the rule id; an operation whose probes were all blocked or
//!   skipped is `<skipped>`; findings below the threshold go to system-out.
//! - A contract check: one `<testsuite>` named after the mode and one
//!   `<testcase>` per difference, interaction or exchange, named after its
//!   rule and subject. An item that fails the command is a `<failure>`
//!   whose type is the rule id; the others pass with their status (and an
//!   allowlist justification) in system-out.

use std::collections::BTreeMap;
use std::time::Duration;

use quick_junit::{NonSuccessKind, Property, Report, TestCase, TestCaseStatus, TestSuite};

use super::view::{OperationView, operations};
use crate::bench::{BenchReport, ThresholdResult};
use crate::contract::{ContractItem, ContractReport};
use crate::exploration::{ExplorationReport, ReportFinding, Severity};
use crate::run::{RunReport, RunRequest, Status};
use crate::stress::{StressFinding, StressReport};

/// The name of the report and of a run's one test suite.
const SUITE: &str = "rustman";

/// `run` as a JUnit XML document, ending in a newline.
pub fn run_junit(run: &RunReport) -> String {
    let iterated = run.is_iterated();
    let mut suite = TestSuite::new(SUITE);
    suite.set_time(duration(run.summary.duration_ms));
    if let Some(env) = &run.environment {
        suite.add_property(Property::new("environment", env.as_str()));
    }
    suite.add_property(Property::new("started_at", run.started_at.as_str()));
    if run.summary.interrupted {
        suite.add_property(Property::new("interrupted", "true"));
    }
    if let Some(line) = run.summary.snapshots.line() {
        suite.add_property(Property::new("snapshots", line));
    }
    suite.add_test_cases(run.requests.iter().map(|r| request_case(r, iterated)));
    let mut report = Report::new(SUITE);
    report.set_time(duration(run.summary.duration_ms));
    report.add_test_suite(suite);
    finish(&report)
}

/// `report` as a JUnit XML document, failing findings at or above `fail_on`.
pub fn exploration_junit(report: &ExplorationReport, fail_on: Severity) -> String {
    let mut suites: BTreeMap<String, TestSuite> = BTreeMap::new();
    for op in operations(report) {
        let suite = suites.entry(op.group.clone()).or_insert_with(|| {
            let mut suite = TestSuite::new(op.group.as_str());
            suite.add_property(Property::new("target", report.target.as_str()));
            suite.add_property(Property::new("seed", report.seed.to_string()));
            suite
        });
        suite.add_test_case(operation_case(report, &op, fail_on));
    }
    let mut junit = Report::new(SUITE);
    junit.set_time(Duration::from_millis(report.summary.duration_ms));
    junit.add_test_suites(suites.into_values());
    finish(&junit)
}

/// A contract report as a JUnit XML document, ending in a newline.
pub fn contract_junit(report: &ContractReport) -> String {
    let mut suite = TestSuite::new(format!("contract {}", report.mode));
    for source in &report.sources {
        suite.add_property(Property::new(source.role.as_str(), source.name.as_str()));
    }
    if let Some(version) = report.catalog_version {
        suite.add_property(Property::new("catalog_version", version.to_string()));
    }
    suite.add_test_cases(report.items.iter().map(contract_case));
    let mut junit = Report::new(SUITE);
    junit.add_test_suite(suite);
    finish(&junit)
}

fn contract_case(item: &ContractItem) -> TestCase {
    let line = format!("{}: {}", item.label(), item.message);
    let status = if item.failing {
        let mut status = TestCaseStatus::non_success(NonSuccessKind::Failure);
        status.set_type(item.rule.as_str());
        status.set_message(line.as_str());
        status.set_description(item.description.as_str());
        status
    } else {
        TestCaseStatus::success()
    };
    let mut case = TestCase::new(format!("{} {}", item.rule, item.subject), status);
    case.set_classname(item.subject.as_str());
    if !item.failing {
        let out = match &item.allowed {
            Some(why) => format!("{line}\nallowed: {why}"),
            None => line,
        };
        case.set_system_out(out);
    }
    case
}

/// A bench report as a JUnit XML document, ending in a newline: one test
/// case per threshold, plus one for an aborted run.
pub fn bench_junit(report: &BenchReport) -> String {
    let mut suite = TestSuite::new("bench");
    suite.add_property(Property::new("target", report.target.as_str()));
    suite.add_property(Property::new("executor", report.profile.executor.as_str()));
    suite.add_test_cases(report.thresholds.iter().map(threshold_case));
    if let Some(aborted) = report.aborted {
        let mut status = TestCaseStatus::non_success(NonSuccessKind::Error);
        status.set_type("aborted");
        status.set_message(format!("aborted: {aborted}"));
        suite.add_test_case(TestCase::new("bench run", status));
    }
    let mut junit = Report::new(SUITE);
    junit.set_time(Duration::from_secs_f64(
        report.totals.duration_ms.max(0.0) / 1000.0,
    ));
    junit.add_test_suite(suite);
    finish(&junit)
}

/// A stress report as a JUnit XML document, ending in a newline: one test
/// case per finding, a `<failure>` at or above `fail_on`; a run without
/// findings is one passing case.
pub fn stress_junit(report: &StressReport, fail_on: Severity) -> String {
    let mut suite = TestSuite::new("stress");
    suite.add_property(Property::new("target", report.target.as_str()));
    suite.add_property(Property::new("mode", report.mode.as_str()));
    suite.add_property(Property::new("seed", report.seed.to_string()));
    if report.findings.is_empty() {
        suite.add_test_case(TestCase::new("stress run", TestCaseStatus::success()));
    }
    suite.add_test_cases(report.findings.iter().map(|f| stress_case(f, fail_on)));
    let mut junit = Report::new(SUITE);
    junit.set_time(duration(report.summary.duration_ms as f64));
    junit.add_test_suite(suite);
    finish(&junit)
}

fn stress_case(finding: &StressFinding, fail_on: Severity) -> TestCase {
    let f = &finding.finding;
    let failing = f.severity <= fail_on;
    let status = if failing {
        let mut status = TestCaseStatus::non_success(NonSuccessKind::Failure);
        status.set_type(f.rule.as_str());
        status.set_message(finding_line(f));
        status
    } else {
        TestCaseStatus::success()
    };
    let mut case = TestCase::new(format!("{} {}", f.rule, f.operation), status);
    case.set_classname(f.operation.as_str());
    if !failing {
        case.set_system_out(finding_line(f));
    }
    case
}

fn threshold_case(result: &ThresholdResult) -> TestCase {
    let status = if result.passed {
        TestCaseStatus::success()
    } else {
        let mut status = TestCaseStatus::non_success(NonSuccessKind::Failure);
        status.set_type("threshold");
        status.set_message(result.line());
        status
    };
    let mut case = TestCase::new(result.expression.as_str(), status);
    case.set_classname("threshold");
    if result.passed {
        case.set_system_out(result.line());
    }
    case
}

fn finish(report: &Report) -> String {
    let mut text = report.to_string().expect("JUnit reports serialize");
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

/// One test case, named `<id> [<iteration label>]` in an iterated run.
fn request_case(record: &RunRequest, iterated: bool) -> TestCase {
    let name = match record.iteration_label(iterated) {
        Some(label) => format!("{} [{label}]", record.id),
        None => record.id.clone(),
    };
    let mut case = TestCase::new(name, request_status(record));
    case.set_classname(record.file.as_str());
    case.set_time(duration(record.duration_ms));
    case.set_assertions(record.assertions.len());
    let notes = record.snapshot_notes();
    if !notes.is_empty() {
        case.set_system_out(notes.join("\n"));
    }
    case
}

fn request_status(record: &RunRequest) -> TestCaseStatus {
    let kind = match record.status {
        Status::Passed => return TestCaseStatus::success(),
        Status::Skipped => {
            let mut status = TestCaseStatus::skipped();
            if let Some(reason) = &record.skip_reason {
                status.set_message(format!("skipped ({})", reason.replace('_', " ")));
            }
            return status;
        }
        Status::Failed => NonSuccessKind::Failure,
        Status::Errored => NonSuccessKind::Error,
    };
    let reasons = record.reasons();
    let mut status = TestCaseStatus::non_success(kind);
    if let Some(first) = reasons.first() {
        status.set_message(first.as_str());
    }
    status.set_description(reasons.join("\n"));
    status
}

fn operation_case(report: &ExplorationReport, op: &OperationView, fail_on: Severity) -> TestCase {
    let (failing, other): (Vec<&ReportFinding>, Vec<&ReportFinding>) = report
        .findings
        .iter()
        .filter(|f| f.operation == op.operation)
        .partition(|f| f.severity <= fail_on);
    let status = match failing.first() {
        Some(_) => failure(&failing),
        None => unexercised(report, &op.operation).unwrap_or_else(TestCaseStatus::success),
    };
    let mut case = TestCase::new(op.operation.as_str(), status);
    case.set_classname(op.group.as_str());
    let probes = report
        .probes
        .iter()
        .filter(|p| p.operation == op.operation)
        .count();
    case.set_assertions(probes);
    if !other.is_empty() {
        let lines: Vec<String> = other.iter().map(|f| finding_line(f)).collect();
        case.set_system_out(lines.join("\n"));
    }
    case
}

fn failure(failing: &[&ReportFinding]) -> TestCaseStatus {
    let mut status = TestCaseStatus::non_success(NonSuccessKind::Failure);
    let first = failing[0];
    status.set_type(first.rule.as_str());
    status.set_message(finding_line(first));
    let lines: Vec<String> = failing.iter().map(|f| finding_line(f)).collect();
    status.set_description(lines.join("\n"));
    status
}

/// `skipped` when every probe of `operation` was blocked, skipped or not run.
fn unexercised(report: &ExplorationReport, operation: &str) -> Option<TestCaseStatus> {
    let probes: Vec<_> = report
        .probes
        .iter()
        .filter(|p| p.operation == operation)
        .collect();
    let idle = |o: &str| matches!(o, "blocked" | "skipped" | "not run");
    if probes.is_empty() || !probes.iter().all(|p| idle(&p.outcome)) {
        return None;
    }
    let mut status = TestCaseStatus::skipped();
    let reason = probes
        .iter()
        .find_map(|p| p.reason.as_deref())
        .unwrap_or("");
    status.set_message(format!(
        "{} probes {}: {reason}",
        probes.len(),
        probes[0].outcome
    ));
    Some(status)
}

fn finding_line(f: &ReportFinding) -> String {
    format!("{} [{}] {}: {}", f.id, f.severity, f.rule, f.message)
}

fn duration(ms: f64) -> Duration {
    Duration::from_secs_f64(ms.max(0.0) / 1000.0)
}
