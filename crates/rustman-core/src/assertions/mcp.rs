//! MCP checks (mcp-client "MCP assertions and captures"): `is_error`,
//! `content_text_contains`, `structured` (a JSONPath check rooted at
//! `structuredContent`), `output_schema` and `tool_exists`.
//!
//! They read the body as the MCP result the server answered: a tool result
//! (`content`, `structuredContent`, `isError`), a `tools/list` result
//! (`tools`), and so on. `output_schema` reads what the client found when
//! it validated `structuredContent` against the tool's `outputSchema`
//! ([`McpFacts`]), so the schema is checked once, by the client.

use serde_json::Value;

use super::expected::Interp;
use super::response::Parsed;
use super::{AssertionResult, Check, json, quoted};
use crate::model::{Assertion, JsonAssertion};

/// Why an MCP check cannot be evaluated against a body.
const NOT_MCP: &str = "response is not an MCP result";

/// How many text items or tool names a failure lists before `...`.
const SHOWN_ITEMS: usize = 20;

/// What the client learned about an MCP result besides its body.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct McpFacts {
    /// The violations of the tool's `outputSchema` by `structuredContent`;
    /// `None` when the tool declares no `outputSchema` (or the operation is
    /// not `tools/call`).
    pub output_violations: Option<Vec<String>>,
}

/// Whether `assertion` is an MCP check (and so reads the body).
pub(super) fn reads_result(assertion: &Assertion) -> bool {
    matches!(
        assertion,
        Assertion::IsError(_)
            | Assertion::ContentTextContains(_)
            | Assertion::Structured(_)
            | Assertion::OutputSchema(_)
            | Assertion::ToolExists(_)
    )
}

/// `assertions` with the implicit `output_schema: true` appended when the
/// tool declares an `outputSchema` and no `output_schema` check is written.
pub(super) fn with_implicit(assertions: &[Assertion], facts: &McpFacts) -> Vec<Assertion> {
    let mut all = assertions.to_vec();
    let written = all.iter().any(|a| matches!(a, Assertion::OutputSchema(_)));
    if facts.output_violations.is_some() && !written {
        all.push(Assertion::OutputSchema(true));
    }
    all
}

pub(super) fn evaluate(
    assertion: &Assertion,
    response: &Parsed<'_, '_>,
    interp: &mut Interp<'_, '_>,
    facts: Option<&McpFacts>,
) -> AssertionResult {
    match assertion {
        Assertion::IsError(want) => is_error(*want, response),
        Assertion::ContentTextContains(text) => text_contains(text, response, interp),
        Assertion::Structured(check) => structured(check, response, interp),
        Assertion::OutputSchema(want) => output_schema(*want, facts),
        Assertion::ToolExists(name) => tool_exists(name, response, interp),
        other => unreachable!("not an MCP check: {other:?}"),
    }
}

/// The body as an MCP result object.
fn result<'r>(response: &'r Parsed<'_, '_>) -> Result<&'r serde_json::Map<String, Value>, String> {
    match response.json() {
        Ok(Value::Object(result)) => Ok(result),
        _ => Err(NOT_MCP.to_owned()),
    }
}

/// `a, b, c` (the first few, then `...`).
fn listed(items: &[String]) -> String {
    let mut shown: Vec<String> = items.iter().take(SHOWN_ITEMS).cloned().collect();
    if items.len() > SHOWN_ITEMS {
        shown.push("...".to_owned());
    }
    shown.join(", ")
}

fn is_error(want: bool, response: &Parsed<'_, '_>) -> AssertionResult {
    let check = Check::new("is_error", format!("is_error {want}"));
    let result = match result(response) {
        Ok(result) => result,
        Err(e) => return check.error(format!("is_error: {e}")),
    };
    let actual = result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    check.verdict(
        actual == want,
        "is_error",
        want.to_string(),
        actual.to_string(),
    )
}

/// The text of every `text` content item (and of embedded text resources).
fn texts(result: &serde_json::Map<String, Value>) -> Vec<String> {
    let items = result
        .get("content")
        .or_else(|| result.get("contents"))
        .and_then(Value::as_array);
    items
        .into_iter()
        .flatten()
        .filter_map(|item| {
            item.get("text")
                .or_else(|| item.pointer("/resource/text"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect()
}

fn text_contains(
    raw: &str,
    response: &Parsed<'_, '_>,
    interp: &mut Interp<'_, '_>,
) -> AssertionResult {
    let prefix = "content_text_contains";
    let check = Check::new(prefix, format!("{prefix} {}", quoted(raw)));
    let found = result(response).and_then(|r| Ok((texts(r), interp.text(raw)?)));
    let (texts, text) = match found {
        Ok(found) => found,
        Err(e) => return check.error(format!("{prefix}: {e}")),
    };
    let actual = if texts.is_empty() {
        "no text content".to_owned()
    } else {
        listed(&texts.iter().map(|t| quoted(t)).collect::<Vec<_>>())
    };
    check.verdict(
        texts.iter().any(|t| t.contains(&text)),
        prefix,
        format!("text content containing {}", quoted(&text)),
        actual,
    )
}

fn structured(
    check: &JsonAssertion,
    response: &Parsed<'_, '_>,
    interp: &mut Interp<'_, '_>,
) -> AssertionResult {
    let failed = |message: &str| {
        Check::new("structured", format!("structured {}", check.path))
            .error(format!("structured: {message}"))
    };
    match result(response) {
        Err(e) => return failed(&e),
        Ok(result) if !result.contains_key("structuredContent") => {
            return failed("the result has no structuredContent");
        }
        Ok(_) => {}
    }
    let query = rooted_at_structured(&check.path);
    json::evaluate_at(check, response, interp, "structured", &query)
}

/// `path` with its root `$` moved to `$.structuredContent`.
fn rooted_at_structured(path: &str) -> String {
    match path.strip_prefix('$') {
        Some(rest) => format!("$.structuredContent{rest}"),
        None => path.to_owned(),
    }
}

fn output_schema(want: bool, facts: Option<&McpFacts>) -> AssertionResult {
    let check = Check::new("output_schema", format!("output_schema {want}"));
    let Some(violations) = facts.and_then(|f| f.output_violations.as_ref()) else {
        return check.error("output_schema: the tool declares no outputSchema");
    };
    let actual = if violations.is_empty() {
        "structuredContent matching outputSchema".to_owned()
    } else {
        violations.join("; ")
    };
    let expected = if want {
        "structuredContent matching outputSchema"
    } else {
        "structuredContent breaking outputSchema"
    };
    check.verdict(
        violations.is_empty() == want,
        "output_schema",
        expected.to_owned(),
        actual,
    )
}

fn tool_exists(
    raw: &str,
    response: &Parsed<'_, '_>,
    interp: &mut Interp<'_, '_>,
) -> AssertionResult {
    let prefix = "tool_exists";
    let check = Check::new(prefix, format!("{prefix} {}", quoted(raw)));
    let names = result(response).and_then(|r| {
        let tools = r.get("tools").and_then(Value::as_array).ok_or_else(|| {
            "the result is not a tools/list result (use operation: tools/list)".to_owned()
        })?;
        let names: Vec<String> = tools
            .iter()
            .filter_map(|t| t.get("name").and_then(Value::as_str).map(str::to_owned))
            .collect();
        Ok((names, interp.text(raw)?))
    });
    let (names, name) = match names {
        Ok(found) => found,
        Err(e) => return check.error(format!("{prefix}: {e}")),
    };
    let actual = if names.is_empty() {
        "no tools".to_owned()
    } else {
        format!("available tools: {}", listed(&names))
    };
    check.verdict(
        names.contains(&name),
        prefix,
        format!("a tool named {}", quoted(&name)),
        actual,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_paths_are_rooted_at_structured_content() {
        assert_eq!(rooted_at_structured("$"), "$.structuredContent");
        assert_eq!(rooted_at_structured("$.count"), "$.structuredContent.count");
        assert_eq!(rooted_at_structured("count"), "count");
    }

    #[test]
    fn the_implicit_output_schema_check_is_added_once() {
        let declared = McpFacts {
            output_violations: Some(Vec::new()),
        };
        let none = McpFacts::default();
        assert_eq!(
            with_implicit(&[], &declared),
            [Assertion::OutputSchema(true)]
        );
        assert!(with_implicit(&[], &none).is_empty());
        let written = [Assertion::OutputSchema(false)];
        assert_eq!(with_implicit(&written, &declared), written);
    }
}
