//! Evaluating assertions against a response (assertions spec, design D9).
//!
//! Every assertion of a request is evaluated, even after one fails, into an
//! [`AssertionResult`] that passed, failed (expected vs. actual) or could not
//! be evaluated (`error`). Expected values are interpolated with the same
//! resolver as the request. Messages are redacted before they are returned,
//! because they can quote response data that echoes a secret: in full, and
//! only then quoted and cut (see [`shown`]).

mod body;
mod expected;
mod graphql;
mod grpc;
mod header;
mod json;
mod jsonrpc;
mod mcp;
mod realtime;
mod response;
mod schema;
mod shown;
mod status;
mod value;

use std::fmt;

use serde::Serialize;

use crate::model::Assertion;
use crate::redact::Redactor;
use crate::vars::Resolver;
use crate::workspace::RequestRef;

pub use mcp::McpFacts;
pub use realtime::{RealtimeFacts, wait_matches};
pub use response::{GrpcView, ResponseView};
pub use schema::MAX_VIOLATIONS;

use expected::Interp;
use response::Parsed;
pub(crate) use shown::quoted;

/// Snapshot assertions read and write files, so the runner checks them
/// with [`crate::snapshot::check`] instead of [`evaluate`].
pub const SNAPSHOT_NEEDS_RUNNER: &str = "snapshot assertions are checked by the runner";

/// How one assertion ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Pass,
    Fail,
    /// The assertion could not be evaluated (a non-JSON body for a JSONPath,
    /// an invalid regular expression, an unresolved variable...).
    Error,
}

/// The result of one assertion.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AssertionResult {
    /// The assertion type: `status`, `header`, `json`, `schema`, `body`,
    /// `latency`, a GraphQL check (`no_errors`, `errors_count`,
    /// `error_message_contains`, `error_code`, `data`), `rpc_error`, an
    /// MCP check
    /// (`is_error`, `content_text_contains`, `structured`, `output_schema`,
    /// `tool_exists`), a
    /// gRPC check (`grpc_status`, `metadata`), or a realtime check
    /// (`messages`, `close_code`).
    pub kind: &'static str,
    pub outcome: Outcome,
    /// A short description of the check, such as `json $.id equals 42`.
    pub label: String,
    /// The expected value, as shown in a failure.
    pub expected: Option<String>,
    /// The actual value, as shown in a failure.
    pub actual: Option<String>,
    /// Why it failed or errored, such as `status: expected 200, got 404`.
    /// Empty when it passed.
    pub message: String,
    /// The outcome of a `snapshot` assertion: its state, file and diff.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<crate::snapshot::SnapshotResult>,
}

impl AssertionResult {
    pub fn passed(&self) -> bool {
        self.outcome == Outcome::Pass
    }
}

impl fmt::Display for AssertionResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.outcome {
            Outcome::Pass => write!(f, "pass: {}", self.label),
            Outcome::Fail => f.write_str(&self.message),
            Outcome::Error => write!(f, "error: {}", self.message),
        }
    }
}

/// What evaluating an assertion needs besides the response.
pub struct Context<'r, 'a> {
    /// Resolves `{{variables}}` in expected values.
    pub resolver: &'r Resolver<'a>,
    /// The request the assertions belong to; schema files are relative to
    /// its folder. Without it, `schema: {file}` is an error.
    pub request: Option<&'r RequestRef<'a>>,
    /// Redacts messages; secrets used in expected values are added to it.
    pub redactor: &'r Redactor,
}

/// Whether evaluating `assertions` reads the response body.
pub fn needs_body(assertions: &[Assertion]) -> bool {
    assertions.iter().any(|a| {
        matches!(
            a,
            Assertion::Json(_)
                | Assertion::Schema(_)
                | Assertion::Body(_)
                | Assertion::RpcError(_)
                | Assertion::Snapshot(_)
        ) || graphql::reads_result(a)
            || mcp::reads_result(a)
    })
}

/// Evaluate every assertion, in order.
pub fn evaluate(
    assertions: &[Assertion],
    response: &ResponseView<'_>,
    ctx: &Context<'_, '_>,
) -> Vec<AssertionResult> {
    evaluate_all(assertions, response, ctx, Facts::None)
}

/// Evaluate the assertions of an MCP request against `response`, whose
/// body is the MCP result, plus the implicit `output_schema` check when the
/// tool declares an `outputSchema`.
pub fn evaluate_mcp(
    assertions: &[Assertion],
    response: &ResponseView<'_>,
    ctx: &Context<'_, '_>,
    facts: &McpFacts,
) -> Vec<AssertionResult> {
    let all = mcp::with_implicit(assertions, facts);
    evaluate_all(&all, response, ctx, Facts::Mcp(facts))
}

/// Evaluate the assertions of a realtime session against `response`,
/// whose body is `{"messages": [...]}` (each received message parsed as
/// JSON, or its text), with what `facts` says about the session.
pub fn evaluate_realtime(
    assertions: &[Assertion],
    response: &ResponseView<'_>,
    ctx: &Context<'_, '_>,
    facts: &RealtimeFacts,
) -> Vec<AssertionResult> {
    evaluate_all(assertions, response, ctx, Facts::Realtime(facts))
}

/// What a protocol client learned besides the response.
#[derive(Clone, Copy)]
enum Facts<'f> {
    None,
    Mcp(&'f McpFacts),
    Realtime(&'f RealtimeFacts),
}

fn evaluate_all(
    assertions: &[Assertion],
    response: &ResponseView<'_>,
    ctx: &Context<'_, '_>,
    facts: Facts<'_>,
) -> Vec<AssertionResult> {
    let mut interp = Interp::new(ctx.resolver, ctx.redactor);
    let parsed = Parsed::new(response);
    let results: Vec<AssertionResult> = assertions
        .iter()
        .map(|a| evaluate_one(a, &parsed, ctx, &mut interp, facts))
        .collect();
    let redactor = interp.into_redactor();
    results.into_iter().map(|r| redact(r, &redactor)).collect()
}

fn evaluate_one(
    assertion: &Assertion,
    response: &Parsed<'_, '_>,
    ctx: &Context<'_, '_>,
    interp: &mut Interp<'_, '_>,
    facts: Facts<'_>,
) -> AssertionResult {
    let view = response.view;
    match assertion {
        Assertion::Status(expected) => status::evaluate(expected, view),
        Assertion::Header(header) => header::evaluate(header, view, interp),
        Assertion::Json(json) => json::evaluate(json, response, interp),
        Assertion::Schema(schema) => schema::evaluate(schema, response, ctx.request),
        Assertion::Body(body) => body::evaluate(body, view, interp),
        Assertion::Latency(latency) => status::latency(latency, view),
        Assertion::RpcError(expected) => jsonrpc::evaluate(expected, response, interp),
        Assertion::GrpcStatus(expected) => grpc::evaluate(*expected, view),
        Assertion::Metadata(metadata) => header::metadata(metadata, view, interp),
        Assertion::Snapshot(snapshot) => {
            Check::new("snapshot", format!("snapshot {}", snapshot.name()))
                .error(SNAPSHOT_NEEDS_RUNNER)
        }
        other if mcp::reads_result(other) => {
            let mcp = match facts {
                Facts::Mcp(facts) => Some(facts),
                _ => None,
            };
            mcp::evaluate(other, response, interp, mcp)
        }
        other if realtime::is_session_check(other) => {
            let session = match facts {
                Facts::Realtime(facts) => Some(facts),
                _ => None,
            };
            realtime::evaluate(other, session)
        }
        graphql => graphql::evaluate(graphql, response, interp),
    }
}

fn redact(result: AssertionResult, redactor: &Redactor) -> AssertionResult {
    let text = |s: String| shown::finish(&s, redactor);
    AssertionResult {
        label: text(result.label),
        expected: result.expected.map(text),
        actual: result.actual.map(text),
        message: text(result.message),
        ..result
    }
}

/// Builds results with a shared kind and label.
pub(crate) struct Check {
    kind: &'static str,
    label: String,
}

impl Check {
    pub fn new(kind: &'static str, label: impl Into<String>) -> Self {
        Self {
            kind,
            label: label.into(),
        }
    }

    pub fn pass(self) -> AssertionResult {
        self.result(Outcome::Pass, None, None, String::new())
    }

    /// A failure reading `<prefix>: expected <expected>, got <actual>`.
    pub fn fail(self, prefix: &str, expected: String, actual: String) -> AssertionResult {
        let message = format!("{prefix}: expected {expected}, got {actual}");
        self.result(Outcome::Fail, Some(expected), Some(actual), message)
    }

    pub fn error(self, message: impl Into<String>) -> AssertionResult {
        self.result(Outcome::Error, None, None, message.into())
    }

    /// A pass when `ok`, else [`Check::fail`].
    pub fn verdict(
        self,
        ok: bool,
        prefix: &str,
        expected: String,
        actual: String,
    ) -> AssertionResult {
        if ok {
            self.pass()
        } else {
            self.fail(prefix, expected, actual)
        }
    }

    fn result(
        self,
        outcome: Outcome,
        expected: Option<String>,
        actual: Option<String>,
        message: String,
    ) -> AssertionResult {
        AssertionResult {
            kind: self.kind,
            outcome,
            label: self.label,
            expected,
            actual,
            message,
            snapshot: None,
        }
    }
}

/// A regular expression compiled for an assertion or capture.
pub(crate) fn regex(pattern: &str) -> Result<regex::Regex, String> {
    regex::Regex::new(pattern).map_err(|e| format!("invalid regular expression /{pattern}/: {e}"))
}

#[cfg(test)]
mod tests;
