//! Realtime sessions (realtime-sessions): the `wait_for` matcher of
//! WebSocket steps, on the same JSONPath (RFC 9535) and regular-expression
//! evaluators as the `json` and `body` assertions (design D4), and the
//! session checks `messages` and `close_code`. `json`, `body` and
//! `latency` assertions read the session through its response view (the
//! messages at `$.messages`, the time to the first message as `ttfb`).

use serde_json::Value;

use super::{AssertionResult, Check, json, regex};
use crate::model::{Assertion, CountOp, MessagesAssertion, WaitFor};

/// What a realtime session produced besides its messages.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RealtimeFacts {
    /// Messages or events received, including those dropped from the
    /// timeline.
    pub received: u64,
    /// The close code either side sent; `None` without a close frame.
    pub close_code: Option<u16>,
}

/// Whether `assertion` reads a realtime session's facts.
pub(super) fn is_session_check(assertion: &Assertion) -> bool {
    matches!(assertion, Assertion::Messages(_) | Assertion::CloseCode(_))
}

pub(super) fn evaluate(assertion: &Assertion, facts: Option<&RealtimeFacts>) -> AssertionResult {
    match assertion {
        Assertion::Messages(m) => messages(m, facts),
        Assertion::CloseCode(code) => close_code(*code, facts),
        other => unreachable!("not a realtime check: {other:?}"),
    }
}

const NOT_REALTIME: &str = "applies to ws and sse requests only";

fn messages(check: &MessagesAssertion, facts: Option<&RealtimeFacts>) -> AssertionResult {
    let want = check.expectation();
    let symbol = match want.op {
        CountOp::Equals => "==",
        CountOp::Gt => ">",
        CountOp::Gte => ">=",
        CountOp::Lt => "<",
        CountOp::Lte => "<=",
    };
    let expected = format!("count {symbol} {}", want.value);
    let result = Check::new("messages", format!("messages {expected}"));
    let Some(facts) = facts else {
        return result.error(format!("messages: {NOT_REALTIME}"));
    };
    let ok = want.op.holds(facts.received, want.value);
    result.verdict(ok, "messages", expected, facts.received.to_string())
}

fn close_code(code: u16, facts: Option<&RealtimeFacts>) -> AssertionResult {
    let result = Check::new("close_code", format!("close_code {code}"));
    let Some(facts) = facts else {
        return result.error(format!("close_code: {NOT_REALTIME}"));
    };
    let actual = facts
        .close_code
        .map_or_else(|| "none".to_owned(), |c| c.to_string());
    let ok = facts.close_code == Some(code);
    result.verdict(ok, "close_code", code.to_string(), actual)
}

/// Whether the received message `payload` satisfies `wait`: its `regex`
/// matches the text, or its `json` check holds for the text parsed as
/// JSON. A message that is not JSON never matches a `json` check. The
/// error is an invalid path or pattern.
pub fn wait_matches(wait: &WaitFor, payload: &str) -> Result<bool, String> {
    if let Some(pattern) = &wait.regex {
        return Ok(regex(pattern)?.is_match(payload));
    }
    let Some(check) = &wait.json else {
        return Ok(false);
    };
    match serde_json::from_str::<Value>(payload) {
        Ok(document) => json::holds(check, &document),
        Err(_) => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assertions::Outcome;
    use crate::model::Checked;

    fn assertion(yaml: &str) -> Assertion {
        crate::format::from_yaml(std::path::Path::new("a"), yaml).unwrap()
    }

    #[test]
    fn message_counts_compare_with_the_received_total() {
        let facts = RealtimeFacts {
            received: 5,
            close_code: Some(1000),
        };
        let gte = assertion("messages: {count_gte: 3}");
        assert!(evaluate(&gte, Some(&facts)).passed());
        let eq = assertion("messages: {count_eq: 4}");
        let failed = evaluate(&eq, Some(&facts));
        assert_eq!(failed.message, "messages: expected count == 4, got 5");
        let lte = assertion("messages: {count_lte: 5}");
        assert!(evaluate(&lte, Some(&facts)).passed());
    }

    #[test]
    fn close_codes_name_both_sides() {
        let facts = RealtimeFacts {
            received: 0,
            close_code: Some(1011),
        };
        let check = assertion("close_code: 1000");
        let failed = evaluate(&check, Some(&facts));
        assert_eq!(failed.message, "close_code: expected 1000, got 1011");
        let none = evaluate(&check, Some(&RealtimeFacts::default()));
        assert_eq!(none.message, "close_code: expected 1000, got none");
        assert!(evaluate(&assertion("close_code: 1011"), Some(&facts)).passed());
    }

    #[test]
    fn session_checks_need_a_session() {
        let result = evaluate(&assertion("close_code: 1000"), None);
        assert_eq!(result.outcome, Outcome::Error);
        assert_eq!(
            result.message,
            "close_code: applies to ws and sse requests only"
        );
    }

    #[test]
    fn a_messages_check_needs_one_bound() {
        let err = crate::format::from_yaml::<Assertion>(
            std::path::Path::new("a"),
            "messages: {count_gte: 1, count_eq: 2}",
        )
        .unwrap_err();
        assert!(
            err.message.contains("count_gte, count_lte, count_eq"),
            "{}",
            err.message
        );
    }

    fn wait(yaml: &str) -> WaitFor {
        let checked: Checked<WaitFor> =
            crate::format::from_yaml(std::path::Path::new("w"), yaml).unwrap();
        checked.into_inner()
    }

    #[test]
    fn json_checks_read_the_parsed_message() {
        let id = wait("json: {path: $.id, equals: 1}");
        assert_eq!(wait_matches(&id, r#"{"id":1,"op":"pong"}"#), Ok(true));
        assert_eq!(wait_matches(&id, r#"{"id":2}"#), Ok(false));
        assert_eq!(wait_matches(&id, "pong"), Ok(false));
        let exists = wait("json: {path: $.op, exists: true}");
        assert_eq!(wait_matches(&exists, r#"{"op":1}"#), Ok(true));
        let missing = wait("json: {path: $.op, exists: false}");
        assert_eq!(wait_matches(&missing, r#"{"id":1}"#), Ok(true));
        let pattern = wait("json: {path: $.op, matches: '^po'}");
        assert_eq!(wait_matches(&pattern, r#"{"op":"pong"}"#), Ok(true));
    }

    #[test]
    fn regexes_match_the_text() {
        let pong = wait("regex: pong");
        assert_eq!(wait_matches(&pong, "a pong!"), Ok(true));
        assert_eq!(wait_matches(&pong, "ping"), Ok(false));
        let bad = wait("regex: '('");
        assert!(
            wait_matches(&bad, "x")
                .unwrap_err()
                .contains("invalid regular expression")
        );
        let bad_path = wait("json: {path: '$[', exists: true}");
        assert!(
            wait_matches(&bad_path, "{}")
                .unwrap_err()
                .contains("invalid JSONPath")
        );
    }
}
