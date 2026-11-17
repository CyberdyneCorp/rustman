//! `body: {contains | not_contains | matches | equals}` on the decoded text body.

use super::expected::Interp;
use super::{AssertionResult, Check, ResponseView, quoted, regex};
use crate::model::BodyAssertion;

pub(super) fn evaluate(
    body: &BodyAssertion,
    response: &ResponseView<'_>,
    interp: &mut Interp<'_, '_>,
) -> AssertionResult {
    let (op, raw) = operator(body);
    let check = Check::new("body", format!("body {op} {}", quoted(raw)));
    let text = match response.text() {
        Ok(text) => text,
        Err(e) => return check.error(e),
    };
    let expected = match interp.text(raw) {
        Ok(expected) => expected,
        Err(e) => return check.error(format!("body: {e}")),
    };
    let (ok, shown) = match op {
        "contains" => (
            text.contains(&expected),
            format!("to contain {}", quoted(&expected)),
        ),
        "not_contains" => (
            !text.contains(&expected),
            format!("not to contain {}", quoted(&expected)),
        ),
        "matches" => match regex(&expected) {
            Ok(re) => (re.is_match(text), format!("to match /{expected}/")),
            Err(e) => return check.error(format!("body: {e}")),
        },
        _ => (text == expected, quoted(&expected)),
    };
    check.verdict(ok, "body", shown, quoted(text))
}

fn operator(body: &BodyAssertion) -> (&'static str, &str) {
    [
        ("contains", &body.contains),
        ("not_contains", &body.not_contains),
        ("matches", &body.matches),
        ("equals", &body.equals),
    ]
    .into_iter()
    .find_map(|(op, value)| value.as_deref().map(|v| (op, v)))
    .unwrap_or(("equals", ""))
}
