//! `status` and `latency` assertions.

use std::time::Duration;

use super::{AssertionResult, Check, ResponseView};
use crate::model::{LatencyAssertion, StatusExpectation};

pub(super) fn evaluate(
    expected: &StatusExpectation,
    response: &ResponseView<'_>,
) -> AssertionResult {
    let actual = response.status;
    let (shown, ok) = match expected {
        StatusExpectation::Code(code) => (code.to_string(), actual == *code),
        StatusExpectation::AnyOf(codes) => {
            let list: Vec<String> = codes.iter().map(u16::to_string).collect();
            (
                format!("one of [{}]", list.join(", ")),
                codes.contains(&actual),
            )
        }
        StatusExpectation::Class(digit) => {
            (format!("{digit}xx"), actual / 100 == u16::from(*digit))
        }
    };
    let check = Check::new(
        "status",
        format!("status {}", shown.trim_start_matches("one of ")),
    );
    check.verdict(ok, "status", shown, actual.to_string())
}

/// `latency: {total_lt, ttfb_lt}`: one result covering every bound given.
pub(super) fn latency(latency: &LatencyAssertion, response: &ResponseView<'_>) -> AssertionResult {
    let bounds = [
        ("total", latency.total_lt, response.total),
        ("ttfb", latency.ttfb_lt, response.ttfb),
    ];
    let label = bounds
        .iter()
        .filter_map(|(name, limit, _)| limit.map(|ms| format!("{name} < {ms} ms")))
        .collect::<Vec<_>>()
        .join(", ");
    let check = Check::new("latency", format!("latency {label}"));
    let failed = bounds.iter().find_map(|(name, limit, actual)| {
        let limit = (*limit)?;
        (*actual >= Duration::from_millis(limit)).then_some((name, limit, actual))
    });
    match failed {
        None => check.pass(),
        Some((name, limit, actual)) => check.fail(
            &format!("latency {name}"),
            format!("< {limit} ms"),
            format!("{} ms", actual.as_millis()),
        ),
    }
}
