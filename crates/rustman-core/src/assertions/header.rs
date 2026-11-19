//! `header: {name, equals | contains | matches | exists}`, and its gRPC
//! twin `metadata` over response headers and trailers. Names match
//! case-insensitively; with a repeated header, any value may satisfy it.

use super::expected::Interp;
use super::{AssertionResult, Check, ResponseView, quoted, regex};
use crate::model::HeaderAssertion;
use crate::redact::Redactor;

pub(super) fn evaluate(
    header: &HeaderAssertion,
    response: &ResponseView<'_>,
    interp: &mut Interp<'_, '_>,
) -> AssertionResult {
    let values = response.header_values(&header.name);
    check("header", header, &values, interp)
}

/// `metadata`: the response headers and trailers of a gRPC call.
pub(super) fn metadata(
    metadata: &HeaderAssertion,
    response: &ResponseView<'_>,
    interp: &mut Interp<'_, '_>,
) -> AssertionResult {
    if response.grpc.is_none() {
        let label = format!("metadata {}", metadata.name);
        return Check::new("metadata", label).error(super::grpc::NOT_GRPC_METADATA);
    }
    let values = response.metadata_values(&metadata.name);
    check("metadata", metadata, &values, interp)
}

fn check(
    kind: &'static str,
    header: &HeaderAssertion,
    values: &[&str],
    interp: &mut Interp<'_, '_>,
) -> AssertionResult {
    let name = header.name.as_str();
    let prefix = format!("{kind} {name}");
    let shown = shown(name, values);
    if let Some(exists) = header.exists {
        let check = Check::new(kind, format!("{prefix} exists: {exists}"));
        let expected = if exists { "to exist" } else { "not to exist" };
        return check.verdict(values.is_empty() != exists, &prefix, expected.into(), shown);
    }
    let (op, raw) = operator(header);
    let check = Check::new(kind, format!("{prefix} {op} {}", quoted(raw)));
    let expected = match interp.text(raw) {
        Ok(text) => text,
        Err(e) => return check.error(format!("{prefix}: {e}")),
    };
    let ok = match op {
        "equals" => values.iter().any(|v| *v == expected),
        "contains" => values.iter().any(|v| v.contains(expected.as_str())),
        _ => match regex(&expected) {
            Ok(re) => values.iter().any(|v| re.is_match(v)),
            Err(e) => return check.error(format!("{prefix}: {e}")),
        },
    };
    let expected = match op {
        "equals" => quoted(&expected),
        "contains" => format!("to contain {}", quoted(&expected)),
        _ => format!("to match /{expected}/"),
    };
    check.verdict(ok, &prefix, expected, shown)
}

fn operator(header: &HeaderAssertion) -> (&'static str, &str) {
    [
        ("equals", &header.equals),
        ("contains", &header.contains),
        ("matches", &header.matches),
    ]
    .into_iter()
    .find_map(|(op, value)| value.as_deref().map(|v| (op, v)))
    .unwrap_or(("equals", ""))
}

/// The header's values as shown in a message; sensitive headers are redacted.
fn shown(name: &str, values: &[&str]) -> String {
    if values.is_empty() {
        return "nothing".to_owned();
    }
    let redactor = Redactor::new();
    values
        .iter()
        .map(|v| quoted(&redactor.header_value(name, v)))
        .collect::<Vec<_>>()
        .join(", ")
}
