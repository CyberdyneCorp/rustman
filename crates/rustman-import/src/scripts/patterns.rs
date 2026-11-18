//! The mapping table (design D3 and script migration): anchored patterns
//! over one normalized statement (see [`super::split::normalize`]).

use std::sync::LazyLock;

use regex::Regex;

use crate::ir::{CaptureFrom, CaptureSpec, Check, JsonOp};

/// What one statement maps to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Syntax only, such as the head of a `pm.test` callback; not counted.
    Wrapper,
    /// `const <name> = pm.response.json()`, tracked for later statements.
    Alias(Alias),
    Check(Check),
    Capture(CaptureSpec),
    /// No declarative equivalent; the statement stays comment-only.
    Unmapped,
}

/// A name holding the response body (`pm.response.json()` or
/// `JSON.parse(responseBody)`), or a part of it at `path` (such as `.args`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alias {
    pub name: String,
    pub path: String,
}

/// The parsed response body, in either spelling.
const BODY: &str = r"(?:pm\.response\.json\(\)|JSON\.parse\(responseBody\))";

/// A double-quoted string literal; group 1 is its escaped content.
const STR: &str = r#""((?:[^"\\]|\\.)*)""#;
/// A member path: `.a`, `[0]` or `["a b"]` segments.
const PATH: &str = r#"((?:\.[A-Za-z_$][\w$]*|\[\d+\]|\["(?:[^"\\]|\\.)*"\])*)"#;

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("the script patterns are valid")
}

static TEST: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"^pm\.test\({STR},(?:function\(\)|\(\)=>)(?:(.+)\))?$"
    ))
});
static STATUS: LazyLock<Regex> =
    LazyLock::new(|| re(r"^pm\.response\.to\.have\.status\((\d{3})\)$"));
/// The legacy `tests["name"] = <expression>` form.
static LEGACY: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"^tests\[{STR}\]=(.+)$")));
static LEGACY_STATUS: LazyLock<Regex> = LazyLock::new(|| re(r"^responseCode\.code===?(\d{3})$"));
static LEGACY_HAS: LazyLock<Regex> =
    LazyLock::new(|| re(&format!(r"^responseBody\.has\({STR}\)$")));
static LEGACY_EQUALS: LazyLock<Regex> = LazyLock::new(|| re(r"^([^=!]+)===?([^=]+)$"));
static OK: LazyLock<Regex> = LazyLock::new(|| re(r"^pm\.response\.to\.be\.(?:ok|success)$"));
static HEADER: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"^pm\.response\.to\.have\.header\({STR}(?:,{})?\)$",
        STR.replace("((", "(?P<value>(")
    ))
});
static ALIAS: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"^(?:const|let|var) ([A-Za-z_$][\w$]*)={BODY}{PATH}$"
    ))
});
static EXPECT: LazyLock<Regex> = LazyLock::new(|| re(r"^pm\.expect\((.+?)\)\.to\.(.+)$"));
static SETTER: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"^pm\.(environment|collectionVariables|globals|variables)\.set\({STR},(.+)\)$"
    ))
});
static JSON_SUBJECT: LazyLock<Regex> =
    LazyLock::new(|| re(&format!(r"^({BODY}|[A-Za-z_$][\w$]*){PATH}$")));
static HEADER_GET: LazyLock<Regex> =
    LazyLock::new(|| re(&format!(r"^pm\.response\.headers\.get\({STR}\)$")));
static COOKIE_GET: LazyLock<Regex> = LazyLock::new(|| re(&format!(r"^pm\.cookies\.get\({STR}\)$")));
static CHAI: LazyLock<Vec<(Regex, JsonOp)>> = LazyLock::new(|| {
    [
        (r"^(?:deep\.|be\.)?(?:eql|equal)\((.+)\)$", JsonOp::Equals),
        (
            r"^not\.(?:deep\.|be\.)?(?:eql|equal)\((.+)\)$",
            JsonOp::NotEquals,
        ),
        (r"^(?:be\.)?(?:above|greaterThan|gt)\((.+)\)$", JsonOp::Gt),
        (r"^(?:be\.)?(?:below|lessThan|lt)\((.+)\)$", JsonOp::Lt),
        (r"^(?:have\.)?(?:lengthOf|length)\((\d+)\)$", JsonOp::Length),
        (r"^(?:be\.)?an?\((.+)\)$", JsonOp::Type),
        (r"^(?:include|contain)\((.+)\)$", JsonOp::Contains),
        (r"^(exist)$", JsonOp::Exists),
    ]
    .into_iter()
    .map(|(p, op)| (re(p), op))
    .collect()
});

/// The JSON types a `type` assertion accepts.
const TYPES: &[&str] = &[
    "string", "number", "integer", "boolean", "array", "object", "null",
];

/// Classify the normalized statement `code`, given the names that hold
/// `pm.response.json()`.
pub fn classify(code: &str, aliases: &[Alias]) -> Outcome {
    let code = code.strip_suffix(';').unwrap_or(code);
    if let Some(m) = TEST.captures(code) {
        return match m.get(2) {
            Some(inner) => classify(inner.as_str(), aliases),
            None => Outcome::Wrapper,
        };
    }
    if let Some(m) = ALIAS.captures(code) {
        return Outcome::Alias(Alias {
            name: m[1].to_owned(),
            path: m[2].to_owned(),
        });
    }
    response_check(code)
        .or_else(|| legacy(code, aliases))
        .map(Outcome::Check)
        .or_else(|| expectation(code, aliases).map(Outcome::Check))
        .or_else(|| setter(code, aliases).map(Outcome::Capture))
        .unwrap_or(Outcome::Unmapped)
}

/// `pm.response.to.have.status(N)`, `.to.be.ok` and `.to.have.header(...)`.
fn response_check(code: &str) -> Option<Check> {
    if let Some(m) = STATUS.captures(code) {
        return status(&m[1]);
    }
    if OK.is_match(code) {
        return Some(Check::StatusClass(2));
    }
    let m = HEADER.captures(code)?;
    let name = unescape(&m[1])?;
    match m.name("value") {
        Some(value) => Some(Check::HeaderEquals {
            name,
            value: unescape(value.as_str())?,
        }),
        None => Some(Check::HeaderExists(name)),
    }
}

/// `tests["..."] = responseCode.code === 200`, `= responseBody.has("x")`
/// or `= <json path> === <literal>`.
fn legacy(code: &str, aliases: &[Alias]) -> Option<Check> {
    let m = LEGACY.captures(code)?;
    let expression = m.get(2)?.as_str();
    if let Some(s) = LEGACY_STATUS.captures(expression) {
        return status(&s[1]);
    }
    if let Some(h) = LEGACY_HAS.captures(expression) {
        return unescape(&h[1]).map(Check::BodyContains);
    }
    let e = LEGACY_EQUALS.captures(expression)?;
    json_check(&e[1], JsonOp::Equals, &e[2], aliases)
}

/// `pm.expect(<subject>).to.<chai op>`.
fn expectation(code: &str, aliases: &[Alias]) -> Option<Check> {
    let m = EXPECT.captures(code)?;
    let (subject, chain) = (&m[1], &m[2]);
    let (op, operand) = chai(chain)?;
    match subject {
        "pm.response.code" if op == JsonOp::Equals => status(operand),
        "pm.response.responseTime" if op == JsonOp::Lt => {
            operand.parse().ok().map(Check::LatencyBelow)
        }
        "pm.response.text()" if op == JsonOp::Contains => {
            let text = operand.strip_prefix('"')?.strip_suffix('"')?;
            unescape(text).map(Check::BodyContains)
        }
        _ => json_check(subject, op, operand, aliases),
    }
}

fn json_check(subject: &str, op: JsonOp, operand: &str, aliases: &[Alias]) -> Option<Check> {
    let mut path = json_path(subject, aliases)?;
    let mut op = op;
    // `x.length` is the array or string length, not a field.
    if let Some(base) = path.strip_suffix(".length") {
        if op != JsonOp::Equals {
            return None;
        }
        path = base.to_owned();
        op = JsonOp::Length;
    }
    let value = match op {
        JsonOp::Exists => String::new(),
        JsonOp::Type => {
            let name: String = serde_json::from_str(operand).ok()?;
            TYPES.contains(&name.as_str()).then_some(())?;
            serde_json::Value::String(name).to_string()
        }
        _ => serde_json::from_str::<serde_json::Value>(operand)
            .ok()?
            .to_string(),
    };
    Some(Check::Json { path, op, value })
}

/// The operator and operand text of a chai chain such as `eql(42)`.
fn chai(chain: &str) -> Option<(JsonOp, &str)> {
    CHAI.iter().find_map(|(pattern, op)| {
        let m = pattern.captures(chain)?;
        Some((*op, m.get(1)?.as_str()))
    })
}

/// The JSONPath of `pm.response.json().a[0]` or `<alias>.a[0]`.
fn json_path(subject: &str, aliases: &[Alias]) -> Option<String> {
    let m = JSON_SUBJECT.captures(subject)?;
    let base = &m[1];
    let prefix = if base.ends_with(')') {
        ""
    } else {
        // The latest definition of the name wins.
        &aliases.iter().rev().find(|a| a.name == base)?.path
    };
    Some(format!("${prefix}{}", &m[2]))
}

/// `pm.<scope>.set("v", <source>)` as a capture.
fn setter(code: &str, aliases: &[Alias]) -> Option<CaptureSpec> {
    let m = SETTER.captures(code)?;
    let var = unescape(&m[2])?;
    if var.is_empty()
        || !var
            .chars()
            .all(|c| c.is_alphanumeric() || "_.-".contains(c))
    {
        return None;
    }
    let from = capture_source(&m[3], aliases)?;
    Some(CaptureSpec {
        var,
        from,
        persist: matches!(&m[1], "environment" | "globals"),
    })
}

fn capture_source(source: &str, aliases: &[Alias]) -> Option<CaptureFrom> {
    if let Some(m) = HEADER_GET.captures(source) {
        return unescape(&m[1]).map(CaptureFrom::Header);
    }
    if let Some(m) = COOKIE_GET.captures(source) {
        return unescape(&m[1]).map(CaptureFrom::Cookie);
    }
    json_path(source, aliases).map(CaptureFrom::Json)
}

fn status(digits: &str) -> Option<Check> {
    digits
        .parse()
        .ok()
        .filter(|code| (100..=599).contains(code))
        .map(Check::Status)
}

/// The text of an escaped string literal body.
fn unescape(escaped: &str) -> Option<String> {
    serde_json::from_str(&format!("\"{escaped}\"")).ok()
}
