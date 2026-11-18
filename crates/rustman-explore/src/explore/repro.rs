//! Reproducing a finding from the report alone: a curl command whose
//! secrets stay `{{name}}` references, and a regression request file whose
//! assertions encode the correct behavior (`explore export` writes it for
//! `rustman run`).

use rustman_core::format::write::request_to_yaml;
use rustman_core::model::{
    Assertion, BodyAssertion, Checked, HeaderAssertion, HttpRequest, JsonAssertion, Request,
    SchemaAssertion, StatusExpectation, Validate,
};
use rustman_core::vars::SecretLookup;
use serde_json::Value;

use super::detect::conformance_schema;
use super::plan::Check;
use crate::collection::BASE_URL;
use crate::openapi::model::Operation;

pub use rustman_report::exploration::Reproduction;

/// Resolves every secret to its own `{{name}}` reference, so a resolved
/// request shows where secrets go without their values.
pub struct SecretReferences;

impl SecretLookup for SecretReferences {
    fn secret(&self, _env: &str, name: &str) -> Result<Option<String>, String> {
        Ok(Some(format!("{{{{{name}}}}}")))
    }
}

/// What a regression for one finding is built from.
#[derive(Clone, Copy, Debug)]
pub struct Regression<'a> {
    pub rule: &'a str,
    pub operation: Option<&'a Operation>,
    pub components: &'a Value,
    /// The status the evidence response had.
    pub status: u16,
    pub check: Option<&'a Check>,
    /// The finding's message, redacted.
    pub message: &'a str,
}

/// The regression request file: `request` (whose URL starts with
/// `target`) sent to `{{baseUrl}}`, with the assertions of `regression`.
pub fn request_file(mut request: HttpRequest, target: &str, regression: &Regression<'_>) -> String {
    let target = target.trim_end_matches('/');
    if let Some(path) = request.url.strip_prefix(target) {
        request.url = format!("{{{{{BASE_URL}}}}}{path}");
    }
    request.name = match regression.operation {
        Some(op) => format!("{}: {} {}", regression.rule, op.method, op.path),
        None => regression.rule.to_owned(),
    };
    request.assertions = Some(assertions(regression)).filter(|a| !a.is_empty());
    request_to_yaml(&Request::Http(Box::new(request)))
}

/// The assertions a fixed API passes for the finding's rule.
pub fn assertions(r: &Regression<'_>) -> Vec<Assertion> {
    match r.rule {
        "auth-not-enforced" | "unexpected-auth-status" => vec![any_of(&[401, 403])],
        "broken-object-level-authorization" => vec![any_of(&[401, 403, 404])],
        "server-error-on-invalid-input" | "invalid-input-accepted" => {
            vec![Assertion::Status(StatusExpectation::Class(4))]
        }
        "undocumented-method-allowed" => vec![any_of(&[404, 405])],
        "resource-survives-delete" => vec![any_of(&[404, 410])],
        "server-error" | "undocumented-status" => documented(r.operation).into_iter().collect(),
        "response-schema-violation" => schema(r),
        "update-not-reflected" => reflected(r.check),
        "error-leakage" => leakage(r.message),
        "missing-hsts" => vec![header("Strict-Transport-Security", |h| {
            h.exists = Some(true);
        })],
        "missing-content-type-options" => vec![header("X-Content-Type-Options", |h| {
            h.equals = Some("nosniff".into());
        })],
        "permissive-cors" => vec![header("Access-Control-Allow-Credentials", |h| {
            h.exists = Some(false);
        })],
        _ => Vec::new(),
    }
}

fn any_of(codes: &[u16]) -> Assertion {
    Assertion::Status(StatusExpectation::AnyOf(codes.to_vec()))
}

/// The documented status codes of `op` (ranges and `default` aside).
fn documented(op: Option<&Operation>) -> Option<Assertion> {
    let codes: Vec<u16> = op?
        .responses
        .iter()
        .filter_map(|r| r.status.parse().ok())
        .collect();
    (!codes.is_empty()).then(|| Assertion::Status(StatusExpectation::AnyOf(codes)))
}

/// The status, and the body against the schema documented for it.
fn schema(r: &Regression<'_>) -> Vec<Assertion> {
    let mut found = vec![Assertion::Status(StatusExpectation::Code(r.status))];
    if let Some(inline) = r
        .operation
        .and_then(|op| conformance_schema(op, r.status, r.components))
    {
        found.extend(
            checked(SchemaAssertion {
                file: None,
                inline: Some(inline),
            })
            .map(Assertion::Schema),
        );
    }
    found
}

/// One `json` assertion per field the update set.
fn reflected(check: Option<&Check>) -> Vec<Assertion> {
    let Some(Check::ReflectsUpdate {
        expected: Value::Object(fields),
    }) = check
    else {
        return Vec::new();
    };
    fields
        .iter()
        .filter_map(|(name, value)| {
            let json = JsonAssertion {
                path: format!("$['{}']", name.replace('\\', "\\\\").replace('\'', "\\'")),
                equals: Some(value.clone()),
                not_equals: None,
                contains: None,
                matches: None,
                gt: None,
                gte: None,
                lt: None,
                lte: None,
                exists: None,
                length: None,
                value_type: None,
            };
            checked(json).map(|j| Assertion::Json(Box::new(j)))
        })
        .collect()
}

/// The body no longer contains the leaked excerpt (`<label>: <excerpt>`).
fn leakage(message: &str) -> Vec<Assertion> {
    let excerpt = message
        .split_once(": ")
        .map_or(message, |(_, e)| e)
        .lines()
        .next()
        .unwrap_or_default();
    if excerpt.is_empty() {
        return Vec::new();
    }
    checked(BodyAssertion {
        contains: None,
        not_contains: Some(excerpt.to_owned()),
        matches: None,
        equals: None,
    })
    .map(Assertion::Body)
    .into_iter()
    .collect()
}

fn header(name: &str, set: impl FnOnce(&mut HeaderAssertion)) -> Assertion {
    let mut assertion = HeaderAssertion {
        name: name.to_owned(),
        equals: None,
        contains: None,
        matches: None,
        exists: None,
    };
    set(&mut assertion);
    Assertion::Header(Checked::new(assertion).unwrap_or_else(|e| unreachable!("{e}")))
}

fn checked<T: Validate>(value: T) -> Option<Checked<T>> {
    Checked::new(value).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustman_core::format::write::assertions_to_yaml;

    fn regression(rule: &str) -> Regression<'_> {
        Regression {
            rule,
            operation: None,
            components: &Value::Null,
            status: 200,
            check: None,
            message: "Java stack trace: at a.B.c(B.java:1)",
        }
    }

    #[test]
    fn every_rule_has_a_regression_that_writes() {
        for rule in super::super::rules::RULES {
            let found = assertions(&regression(rule.id));
            let needs_operation = [
                "server-error",
                "undocumented-status",
                "update-not-reflected",
            ];
            assert_eq!(
                found.is_empty(),
                needs_operation.contains(&rule.id),
                "{}",
                rule.id
            );
            assertions_to_yaml(&found);
        }
    }

    #[test]
    fn leakage_asserts_the_excerpt_is_gone() {
        let yaml = assertions_to_yaml(&assertions(&regression("error-leakage")));
        assert!(yaml.contains("not_contains: at a.B.c(B.java:1)"), "{yaml}");
    }

    #[test]
    fn updates_assert_each_field() {
        let check = Check::ReflectsUpdate {
            expected: serde_json::json!({"name": "updated", "it's": 1}),
        };
        let r = Regression {
            check: Some(&check),
            ..regression("update-not-reflected")
        };
        let yaml = assertions_to_yaml(&assertions(&r));
        assert!(yaml.contains("$['name']"), "{yaml}");
        assert!(yaml.contains("$['it\\'s']"), "{yaml}");
    }

    #[test]
    fn secrets_resolve_to_references() {
        assert_eq!(
            SecretReferences.secret("dev", "token").unwrap().as_deref(),
            Some("{{token}}")
        );
    }
}
