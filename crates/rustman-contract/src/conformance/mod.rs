//! Passive conformance (contract-testing "Passive conformance of recorded
//! traffic", "Conformance coverage and unmatched traffic"): recorded
//! exchanges are matched to the document's operations with the mock
//! server's [`Router`], their requests validated with its [`Checks`] and
//! their responses with the explorer's schema validator. Nothing is sent:
//! the command is built with a [`Sender`] it never calls (design D7).

pub mod exchange;

use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue};
use regex::Regex;
use rustman_explore::explore::detect::documented_response;
use rustman_explore::openapi::model::Operation;
use rustman_explore::openapi::schema::{errors, validator};
use rustman_explore::synth::is_json_media_type;
use rustman_mock::request::RequestView;
use rustman_mock::router::{Routed, Router};
use rustman_mock::validate::Checks;
use rustman_report::contract::{ContractCoverage, OperationCoverage};
use serde_json::Value;

pub use exchange::{RecordedExchange, RecordedResponse};

use crate::pact::path::{Step, from_pointer};
use crate::sender::Sender;
use crate::spec::Spec;

/// How one exchange compares with the document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// It breaks the document.
    Violation,
    /// No operation matches its method and path.
    Undocumented,
    Conforms,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Violation => "violation",
            Self::Undocumented => "undocumented",
            Self::Conforms => "conforms",
        }
    }
}

/// One checked exchange.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checked {
    pub exchange: RecordedExchange,
    /// The matched operation's index in the model.
    pub operation: Option<usize>,
    /// `GET /users/{id}`, or the exchange's `METHOD /path` when undocumented.
    pub subject: String,
    pub verdict: Verdict,
    /// Every violation (`$.email: required property missing`).
    pub messages: Vec<String>,
}

/// The result of a conformance check.
#[derive(Clone, Debug)]
pub struct Conformance {
    pub checked: Vec<Checked>,
    pub coverage: ContractCoverage,
}

impl Conformance {
    pub fn count(&self, verdict: Verdict) -> usize {
        self.checked.iter().filter(|c| c.verdict == verdict).count()
    }
}

/// Checks recorded exchanges against one document. `S` is never used: it
/// is there so tests can build the checker with a sender that panics
/// (design D7). The checker itself holds no client; the document must be
/// read with [`Spec::load_local`] for the whole command to stay offline.
pub struct Checker<'a, S> {
    spec: &'a Spec,
    router: Router,
    checks: Checks,
    _sender: &'a S,
}

impl<'a, S: Sender> Checker<'a, S> {
    pub fn new(spec: &'a Spec, sender: &'a S) -> Self {
        Self {
            spec,
            router: Router::new(&spec.model),
            checks: Checks::new(&spec.model),
            _sender: sender,
        }
    }

    /// Check every exchange, in order.
    pub fn check(&self, exchanges: Vec<RecordedExchange>) -> Conformance {
        let checked: Vec<Checked> = exchanges.into_iter().map(|e| self.one(e)).collect();
        let coverage = self.coverage(&checked);
        Conformance { checked, coverage }
    }

    fn one(&self, exchange: RecordedExchange) -> Checked {
        let (path, _) = exchange.path_and_query();
        let routed = self.router.route(&exchange.method, path);
        let Routed::Operation {
            operation, params, ..
        } = routed
        else {
            let subject = format!("{} {path}", exchange.method);
            return Checked {
                exchange,
                operation: None,
                subject,
                verdict: Verdict::Undocumented,
                messages: Vec::new(),
            };
        };
        let op = &self.spec.model.operations[operation];
        let mut messages = self.request_errors(operation, &exchange, params);
        if let Some(response) = &exchange.response {
            messages.extend(response_errors(op, &self.spec.model.components, response));
        }
        let verdict = if messages.is_empty() {
            Verdict::Conforms
        } else {
            Verdict::Violation
        };
        Checked {
            exchange,
            operation: Some(operation),
            subject: format!("{} {}", op.method, op.path),
            verdict,
            messages,
        }
    }

    fn request_errors(
        &self,
        operation: usize,
        exchange: &RecordedExchange,
        params: Vec<(String, String)>,
    ) -> Vec<String> {
        let headers = header_map(&exchange.request_headers);
        let body = Bytes::from(exchange.request_body.clone().unwrap_or_default());
        let (path, query) = exchange.path_and_query();
        let mut view = RequestView::new(&exchange.method, path, query, &headers, &body);
        view.params = params;
        match self
            .checks
            .validate(operation, &view, &self.spec.model.components)
        {
            Ok(()) => Vec::new(),
            Err(problem) if problem.errors.is_empty() => {
                vec![format!("request: {}", problem.detail)]
            }
            Err(problem) => problem
                .errors
                .iter()
                .map(|e| request_message(&e.location, &e.pointer, &e.message))
                .collect(),
        }
    }

    fn coverage(&self, checked: &[Checked]) -> ContractCoverage {
        let operations: Vec<OperationCoverage> = self
            .spec
            .model
            .operations
            .iter()
            .enumerate()
            .map(|(index, op)| {
                let mine = || checked.iter().filter(move |c| c.operation == Some(index));
                OperationCoverage {
                    operation: format!("{} {}", op.method, op.path),
                    exchanges: mine().count(),
                    violations: mine().filter(|c| c.verdict == Verdict::Violation).count(),
                }
            })
            .collect();
        ContractCoverage {
            observed: operations.iter().filter(|o| o.exchanges > 0).count(),
            total: operations.len(),
            operations,
        }
    }
}

/// `request body $.email: required`, `request query /limit: ...`.
fn request_message(location: &str, pointer: &str, message: &str) -> String {
    if location == "body" {
        return format!("request body {}: {message}", from_pointer(pointer));
    }
    format!(
        "request {location} {}: {message}",
        pointer.trim_start_matches('/')
    )
}

fn header_map(headers: &[(String, String)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in headers {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            map.append(name, value);
        }
    }
    map
}

/// The status is documented, and a JSON body matches its schema.
fn response_errors(op: &Operation, components: &Value, response: &RecordedResponse) -> Vec<String> {
    let Some(documented) = documented_response(op, response.status) else {
        return vec![format!("status {} is not documented", response.status)];
    };
    let Some(schema) = documented
        .content
        .iter()
        .find(|m| is_json_media_type(&m.media_type))
        .and_then(|m| m.schema.as_ref())
    else {
        return Vec::new();
    };
    let content_type = response
        .headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case("content-type"))
        .map(|(_, v)| v.as_str());
    let is_json = content_type.is_none_or(is_json_media_type);
    let Some(body) = response
        .body
        .as_deref()
        .filter(|b| is_json && !b.trim().is_empty())
    else {
        return Vec::new();
    };
    let Ok(instance) = serde_json::from_str::<Value>(body) else {
        return vec!["$: the body is not valid JSON".to_owned()];
    };
    let Ok(validator) = validator(schema, components) else {
        return Vec::new();
    };
    errors(&validator, &instance)
        .into_iter()
        .map(|v| schema_message(&v.instance, &v.schema, &v.message))
        .collect()
}

/// `$.email: required property missing` for a missing required property;
/// `<path>: <message>` otherwise.
pub fn schema_message(instance: &str, schema: &str, message: &str) -> String {
    let path = from_pointer(instance);
    if schema.ends_with("/required") {
        let name = Regex::new(r#"^"((?:[^"\\]|\\.)*)" is a required property"#)
            .expect("a valid pattern")
            .captures(message)
            .map(|c| c[1].to_owned());
        if let Some(name) = name {
            return format!("{}: required property missing", path.child(Step::Key(name)));
        }
    }
    format!("{path}: {message}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_properties_are_named_by_their_path() {
        assert_eq!(
            schema_message(
                "",
                "/components/schemas/User/required",
                "\"email\" is a required property"
            ),
            "$.email: required property missing"
        );
        assert_eq!(
            schema_message(
                "/id",
                "/properties/id/type",
                "\"7\" is not of type \"integer\""
            ),
            "$.id: \"7\" is not of type \"integer\""
        );
        assert_eq!(
            request_message("body", "/name", "required"),
            "request body $.name: required"
        );
        assert_eq!(
            request_message("query", "/limit", "too big"),
            "request query limit: too big"
        );
    }
}
