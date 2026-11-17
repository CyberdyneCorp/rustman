//! The JUnit XML report (via `quick-junit`): one `<testcase>` per request,
//! with a `<failure>` for a failed request, an `<error>` for an errored one
//! and `<skipped>` for a skipped one.

use std::time::Duration;

use quick_junit::{NonSuccessKind, Property, Report, TestCase, TestCaseStatus, TestSuite};
use rustman_core::run::{RequestRecord, RequestStatus, RunRecord};

use crate::pretty::reasons;

/// The name of the report and of its one test suite.
const SUITE: &str = "rustman";

/// `run` as a JUnit XML document, ending in a newline.
pub fn junit(run: &RunRecord) -> String {
    let show_iteration = run.requests.iter().any(|r| r.iteration > 1);
    let mut suite = TestSuite::new(SUITE);
    suite.set_time(duration(run.summary.duration_ms));
    if let Some(env) = &run.environment {
        suite.add_property(Property::new("environment", env.as_str()));
    }
    suite.add_property(Property::new("started_at", run.started_at.as_str()));
    if run.summary.interrupted {
        suite.add_property(Property::new("interrupted", "true"));
    }
    suite.add_test_cases(run.requests.iter().map(|r| test_case(r, show_iteration)));
    let mut report = Report::new(SUITE);
    report.set_time(duration(run.summary.duration_ms));
    report.add_test_suite(suite);
    let mut text = report.to_string().expect("JUnit reports serialize");
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

fn test_case(record: &RequestRecord, show_iteration: bool) -> TestCase {
    let name = if show_iteration {
        format!("{} [{}]", record.id, record.iteration)
    } else {
        record.id.clone()
    };
    let mut case = TestCase::new(name, status(record));
    case.set_classname(record.file.as_str());
    case.set_time(duration(record.duration_ms));
    case.set_assertions(record.assertions.len());
    case
}

fn status(record: &RequestRecord) -> TestCaseStatus {
    let kind = match record.status {
        RequestStatus::Passed => return TestCaseStatus::success(),
        RequestStatus::Skipped => {
            let mut status = TestCaseStatus::skipped();
            if let Some(reason) = record.skip_reason {
                status.set_message(format!("skipped ({reason})"));
            }
            return status;
        }
        RequestStatus::Failed => NonSuccessKind::Failure,
        RequestStatus::Errored => NonSuccessKind::Error,
    };
    let reasons = reasons(record);
    let mut status = TestCaseStatus::non_success(kind);
    if let Some(first) = reasons.first() {
        status.set_message(first.as_str());
    }
    status.set_description(reasons.join("\n"));
    status
}

fn duration(ms: f64) -> Duration {
    Duration::from_secs_f64(ms.max(0.0) / 1000.0)
}
