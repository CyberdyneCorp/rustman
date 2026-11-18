//! GraphQL checks (add-graphql-client "GraphQL assertions"): `no_errors`,
//! `errors_count`, `error_message_contains`, `error_code` (an error's
//! `extensions.code`) and `data`, a JSONPath check rooted at `data`.
//!
//! They read the body as a GraphQL result: a JSON object with `data` or
//! `errors`. For a subscription, the runner evaluates them over the
//! collected events: `data` is the list of every event's data and `errors`
//! every event's errors.

use serde_json::Value;

use super::expected::Interp;
use super::response::Parsed;
use super::shown::plain;
use super::{AssertionResult, Check, json, quoted};
use crate::model::{Assertion, CountExpectation, JsonAssertion};

/// Why a GraphQL check cannot be evaluated against a body.
const NOT_GRAPHQL: &str = "response is not a GraphQL result";

/// How many errors a failure lists before `...`.
const SHOWN_ERRORS: usize = 3;

/// Whether `assertion` is a GraphQL check (and so reads the body).
pub(super) fn reads_result(assertion: &Assertion) -> bool {
    matches!(
        assertion,
        Assertion::NoErrors(_)
            | Assertion::ErrorsCount(_)
            | Assertion::ErrorMessageContains(_)
            | Assertion::ErrorCode(_)
            | Assertion::Data(_)
    )
}

pub(super) fn evaluate(
    assertion: &Assertion,
    response: &Parsed<'_, '_>,
    interp: &mut Interp<'_, '_>,
) -> AssertionResult {
    match assertion {
        Assertion::NoErrors(want) => no_errors(*want, response),
        Assertion::ErrorsCount(count) => errors_count(count, response),
        Assertion::ErrorMessageContains(text) => message_contains(text, response, interp),
        Assertion::ErrorCode(code) => error_code(code, response, interp),
        Assertion::Data(check) => data(check, response, interp),
        other => unreachable!("not a GraphQL check: {other:?}"),
    }
}

/// The `errors` of the body, or why it is not a GraphQL result.
fn errors<'r>(response: &'r Parsed<'_, '_>) -> Result<Vec<&'r Value>, &'static str> {
    let Ok(Value::Object(result)) = response.json() else {
        return Err(NOT_GRAPHQL);
    };
    if !result.contains_key("data") && !result.contains_key("errors") {
        return Err(NOT_GRAPHQL);
    }
    match result.get("errors") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(all)) => Ok(all.iter().collect()),
        Some(_) => Err(NOT_GRAPHQL),
    }
}

/// The errors and the interpolated expected text, or the first problem.
fn with_text<'r>(
    errors: Result<Vec<&'r Value>, &'static str>,
    text: Result<String, String>,
) -> Result<(Vec<&'r Value>, String), String> {
    Ok((errors?, text?))
}

/// An error as a failure shows it: its message and, when it has one, its
/// path joined with dots (`forbidden at user.email`).
fn shown_error(error: &Value) -> String {
    let message = match error.get("message").and_then(Value::as_str) {
        Some(message) => quoted(message),
        None => plain(&error.to_string()),
    };
    let path: Vec<String> = error
        .get("path")
        .and_then(Value::as_array)
        .map(|segments| {
            segments
                .iter()
                .map(|s| s.as_str().map_or_else(|| s.to_string(), str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    if path.is_empty() {
        message
    } else {
        format!("{message} at {}", path.join("."))
    }
}

/// `N errors: "a" at x.y, "b"` (the first few).
fn shown_errors(errors: &[&Value]) -> String {
    let count = match errors.len() {
        0 => return "no errors".to_owned(),
        1 => "1 error".to_owned(),
        n => format!("{n} errors"),
    };
    let mut listed: Vec<String> = errors
        .iter()
        .take(SHOWN_ERRORS)
        .map(|e| shown_error(e))
        .collect();
    if errors.len() > SHOWN_ERRORS {
        listed.push("...".to_owned());
    }
    format!("{count}: {}", listed.join(", "))
}

fn no_errors(want: bool, response: &Parsed<'_, '_>) -> AssertionResult {
    let check = Check::new("no_errors", format!("no_errors {want}"));
    let errors = match errors(response) {
        Ok(errors) => errors,
        Err(e) => return check.error(format!("no_errors: {e}")),
    };
    let expected = if want { "no errors" } else { "errors" };
    check.verdict(
        errors.is_empty() == want,
        "no_errors",
        expected.to_owned(),
        shown_errors(&errors),
    )
}

fn errors_count(count: &CountExpectation, response: &Parsed<'_, '_>) -> AssertionResult {
    let op = count.op.as_str();
    let check = Check::new("errors_count", format!("errors_count {op} {}", count.value));
    let errors = match errors(response) {
        Ok(errors) => errors,
        Err(e) => return check.error(format!("errors_count: {e}")),
    };
    let actual = errors.len() as u64;
    let expected = match op {
        "equals" => count.value.to_string(),
        _ => format!("{op} {}", count.value),
    };
    check.verdict(
        count.op.holds(actual, count.value),
        "errors_count",
        expected,
        actual.to_string(),
    )
}

fn message_contains(
    raw: &str,
    response: &Parsed<'_, '_>,
    interp: &mut Interp<'_, '_>,
) -> AssertionResult {
    let check = Check::new(
        "error_message_contains",
        format!("error_message_contains {}", quoted(raw)),
    );
    let prefix = "error_message_contains";
    let (errors, text) = match with_text(errors(response), interp.text(raw)) {
        Ok(found) => found,
        Err(e) => return check.error(format!("{prefix}: {e}")),
    };
    let found = errors.iter().any(|e| {
        e.get("message")
            .and_then(Value::as_str)
            .is_some_and(|m| m.contains(&text))
    });
    check.verdict(
        found,
        prefix,
        format!("an error message containing {}", quoted(&text)),
        shown_errors(&errors),
    )
}

fn error_code(
    raw: &str,
    response: &Parsed<'_, '_>,
    interp: &mut Interp<'_, '_>,
) -> AssertionResult {
    let check = Check::new("error_code", format!("error_code {}", quoted(raw)));
    let (errors, code) = match with_text(errors(response), interp.text(raw)) {
        Ok(found) => found,
        Err(e) => return check.error(format!("error_code: {e}")),
    };
    let codes: Vec<&str> = errors
        .iter()
        .filter_map(|e| e.pointer("/extensions/code").and_then(Value::as_str))
        .collect();
    let actual = if codes.is_empty() {
        "no error codes".to_owned()
    } else {
        codes
            .iter()
            .map(|c| quoted(c))
            .collect::<Vec<_>>()
            .join(", ")
    };
    check.verdict(
        codes.contains(&code.as_str()),
        "error_code",
        format!("an error with code {}", quoted(&code)),
        actual,
    )
}

fn data(
    check: &JsonAssertion,
    response: &Parsed<'_, '_>,
    interp: &mut Interp<'_, '_>,
) -> AssertionResult {
    if let Err(e) = errors(response) {
        return Check::new("data", format!("data {}", check.path)).error(format!("data: {e}"));
    }
    json::evaluate_at(
        check,
        response,
        interp,
        "data",
        &rooted_at_data(&check.path),
    )
}

/// `path` with its root `$` moved to `$.data`: `$.user.name` becomes
/// `$.data.user.name`. A path not starting with `$` is left for the
/// JSONPath parser to reject.
fn rooted_at_data(path: &str) -> String {
    match path.strip_prefix('$') {
        Some(rest) => format!("$.data{rest}"),
        None => path.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_paths_are_rooted_at_data() {
        assert_eq!(rooted_at_data("$"), "$.data");
        assert_eq!(rooted_at_data("$.user.name"), "$.data.user.name");
        assert_eq!(rooted_at_data("$['user']"), "$.data['user']");
        assert_eq!(rooted_at_data("user"), "user");
    }

    fn finished(text: &str) -> String {
        super::super::shown::finish(text, &crate::redact::Redactor::new())
    }

    #[test]
    fn errors_are_shown_with_their_paths() {
        let a = serde_json::json!({"message": "forbidden", "path": ["user", "email"]});
        let b = serde_json::json!({"message": "boom", "path": ["users", 0]});
        let c = serde_json::json!({"oops": true});
        assert_eq!(shown_errors(&[]), "no errors");
        assert_eq!(
            finished(&shown_errors(&[&a, &b])),
            "2 errors: \"forbidden\" at user.email, \"boom\" at users.0"
        );
        assert_eq!(
            finished(&shown_errors(&[&c, &c, &c, &c])),
            "4 errors: {\"oops\":true}, {\"oops\":true}, {\"oops\":true}, ..."
        );
    }
}
