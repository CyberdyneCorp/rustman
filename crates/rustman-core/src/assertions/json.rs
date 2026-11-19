//! `json: {path, <op>: value}` with an RFC 9535 JSONPath (`serde_json_path`).
//!
//! A path matching one node compares against that node; a path matching
//! several compares against the list of nodes; a path matching nothing fails
//! (`exists: false` passes).

use serde_json::Value;
use serde_json_path::JsonPath;

use super::expected::{Expected, Interp};
use super::response::Parsed;
use super::value::{self, show};
use super::{AssertionResult, Check, quoted, regex};
use crate::model::JsonAssertion;

/// The operand of the assertion's one operator.
enum Operand<'m> {
    Value(&'m Value),
    Pattern(&'m str),
    Exists(bool),
    Type(&'m str),
}

/// What an operator concluded.
struct Verdict {
    ok: bool,
    expected: String,
    /// The actual value as shown, when not the node itself.
    actual: Option<String>,
}

impl Verdict {
    fn new(ok: bool, expected: String) -> Self {
        Self {
            ok,
            expected,
            actual: None,
        }
    }
}

pub(super) fn evaluate(
    json: &JsonAssertion,
    response: &Parsed<'_, '_>,
    interp: &mut Interp<'_, '_>,
) -> AssertionResult {
    evaluate_at(json, response, interp, "json", &json.path)
}

/// [`evaluate`] as the assertion `kind`, querying `query` (the written
/// path, or the path rewritten under another root).
pub(super) fn evaluate_at(
    json: &JsonAssertion,
    response: &Parsed<'_, '_>,
    interp: &mut Interp<'_, '_>,
    kind: &'static str,
    query: &str,
) -> AssertionResult {
    let (op, operand) = operator(json);
    let prefix = format!("{kind} {}", json.path);
    let check = Check::new(kind, format!("{prefix} {op} {}", label(&operand)));
    let path = match JsonPath::parse(query) {
        Ok(path) => path,
        Err(e) => return check.error(format!("{prefix}: invalid JSONPath: {e}")),
    };
    let document = match response.json() {
        Ok(document) => document,
        Err(e) => return check.error(e),
    };
    let actual = select(&path, document);
    let verdict = match (&operand, &actual) {
        (Operand::Exists(want), _) => Ok(exists(*want, actual.is_some())),
        (_, None) => Ok(Verdict::new(false, expected_shown(op, &operand))),
        (_, Some(actual)) => compare(op, &operand, actual, Some(interp)),
    };
    match verdict {
        Ok(v) => {
            let shown = v
                .actual
                .unwrap_or_else(|| actual.as_ref().map_or("nothing".to_owned(), show));
            check.verdict(v.ok, &prefix, v.expected, shown)
        }
        Err(e) => check.error(format!("{prefix}: {e}")),
    }
}

/// The node, the list of nodes when there are several, or `None`.
fn select(path: &JsonPath, document: &Value) -> Option<Value> {
    let nodes = path.query(document).all();
    match nodes.as_slice() {
        [] => None,
        [one] => Some((*one).clone()),
        many => Some(Value::Array(many.iter().map(|v| (*v).clone()).collect())),
    }
}

fn exists(want: bool, found: bool) -> Verdict {
    let expected = if want { "to exist" } else { "not to exist" };
    Verdict::new(want == found, expected.to_owned())
}

/// Whether `json` holds for `document`, with its operands used as written
/// (already interpolated): the `wait_for` matcher of realtime sessions.
pub(super) fn holds(json: &JsonAssertion, document: &Value) -> Result<bool, String> {
    let (op, operand) = operator(json);
    let path = JsonPath::parse(&json.path)
        .map_err(|e| format!("json {}: invalid JSONPath: {e}", json.path))?;
    let actual = select(&path, document);
    let verdict = match (&operand, &actual) {
        (Operand::Exists(want), _) => exists(*want, actual.is_some()),
        (_, None) => return Ok(false),
        (_, Some(actual)) => compare(op, &operand, actual, None)?,
    };
    Ok(verdict.ok)
}

/// Compare with the operand interpolated by `interp`, or as written when
/// there is none.
fn compare(
    op: &str,
    operand: &Operand<'_>,
    actual: &Value,
    interp: Option<&mut Interp<'_, '_>>,
) -> Result<Verdict, String> {
    match operand {
        Operand::Value(raw) => {
            let expected = match interp {
                Some(interp) => interp.value(raw)?,
                None => Expected::literal(raw),
            };
            compare_value(op, actual, &expected)
        }
        Operand::Pattern(raw) => {
            let pattern = match interp {
                Some(interp) => interp.text(raw)?,
                None => (*raw).to_owned(),
            };
            let re = regex(&pattern)?;
            let ok = value::text(actual).is_some_and(|t| re.is_match(&t));
            Ok(Verdict::new(ok, format!("to match /{pattern}/")))
        }
        Operand::Type(name) => type_of(name, actual),
        Operand::Exists(_) => unreachable!("handled by the caller"),
    }
}

fn compare_value(op: &str, actual: &Value, expected: &Expected) -> Result<Verdict, String> {
    let e = &expected.value;
    Ok(match op {
        "equals" => Verdict::new(value::equal(actual, e, expected.templated), show(e)),
        "not_equals" => Verdict::new(
            !value::equal(actual, e, expected.templated),
            format!("not {}", show(e)),
        ),
        "contains" => Verdict::new(
            value::contains(actual, e),
            format!("to contain {}", show(e)),
        ),
        "length" => length(actual, e)?,
        _ => ordering(op, actual, e)?,
    })
}

fn ordering(op: &str, actual: &Value, expected: &Value) -> Result<Verdict, String> {
    let bound = value::number(expected)
        .ok_or_else(|| format!("'{op}' needs a number, got {}", show(expected)))?;
    let symbol = match op {
        "gt" => ">",
        "gte" => ">=",
        "lt" => "<",
        _ => "<=",
    };
    let ok = actual.as_f64().is_some_and(|n| match op {
        "gt" => n > bound,
        "gte" => n >= bound,
        "lt" => n < bound,
        _ => n <= bound,
    });
    Ok(Verdict::new(ok, format!("{symbol} {}", show(expected))))
}

fn length(actual: &Value, expected: &Value) -> Result<Verdict, String> {
    let want = value::number(expected)
        .filter(|n| n.fract() == 0.0 && *n >= 0.0)
        .ok_or_else(|| format!("'length' needs a whole number, got {}", show(expected)))?;
    let got = value::length(actual);
    Ok(Verdict {
        ok: got.is_some_and(|n| n as f64 == want),
        expected: format!("length {want}"),
        actual: Some(match got {
            Some(n) => n.to_string(),
            None => format!("{} (a {})", show(actual), value::type_name(actual)),
        }),
    })
}

fn type_of(name: &str, actual: &Value) -> Result<Verdict, String> {
    if !value::TYPES.contains(&name) {
        return Err(format!(
            "unknown type '{name}' (expected one of: {})",
            value::TYPES.join(", ")
        ));
    }
    Ok(Verdict {
        ok: value::is_type(actual, name),
        expected: format!("type {name}"),
        actual: Some(value::type_name(actual).to_owned()),
    })
}

/// The expected value as shown when the path matched nothing.
fn expected_shown(op: &str, operand: &Operand<'_>) -> String {
    match (op, operand) {
        ("equals", Operand::Value(v)) => show(v),
        ("length", Operand::Value(v)) => format!("length {}", show(v)),
        (_, operand) => format!("{op} {}", label(operand)),
    }
}

fn label(operand: &Operand<'_>) -> String {
    match operand {
        Operand::Value(v) => show(v),
        Operand::Pattern(p) => format!("/{p}/"),
        Operand::Exists(b) => b.to_string(),
        Operand::Type(t) => quoted(t),
    }
}

fn operator(json: &JsonAssertion) -> (&'static str, Operand<'_>) {
    let values = [
        ("equals", &json.equals),
        ("not_equals", &json.not_equals),
        ("contains", &json.contains),
        ("gt", &json.gt),
        ("gte", &json.gte),
        ("lt", &json.lt),
        ("lte", &json.lte),
        ("length", &json.length),
    ];
    if let Some((op, v)) = values
        .into_iter()
        .find_map(|(op, v)| v.as_ref().map(|v| (op, v)))
    {
        return (op, Operand::Value(v));
    }
    if let Some(pattern) = &json.matches {
        return ("matches", Operand::Pattern(pattern));
    }
    if let Some(name) = &json.value_type {
        return ("type", Operand::Type(name));
    }
    ("exists", Operand::Exists(json.exists.unwrap_or(true)))
}
