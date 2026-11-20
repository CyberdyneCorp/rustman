//! Pact matching for provider verification (design D4): the actual
//! response is compared with the interaction's expected response under its
//! matching rules (`type`, `regex`, `equality`, `include`, `integer`,
//! `decimal`, `number`, `min`/`max`, `statusCode`; `eachLike` is a `type`
//! rule with a `min` on an array). Every mismatch names where it is:
//! `status`, `header <name>` or a JSON path such as `$.id`.

use std::fmt;

use regex::Regex;
use serde_json::Value;

use super::model::{Matcher, PactResponse, RuleSet};
use super::path::{Path, Step};

/// One difference between the expected and the actual response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mismatch {
    /// `status`, `header Content-Type`, `body` or a JSON path (`$.id`).
    pub at: String,
    pub message: String,
}

impl fmt::Display for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.at, self.message)
    }
}

/// What the provider answered.
#[derive(Clone, Debug, Default)]
pub struct Actual {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Actual {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Every mismatch of `actual` against `expected`; empty when it matches.
pub fn compare(expected: &PactResponse, actual: &Actual) -> Vec<Mismatch> {
    let mut out = Vec::new();
    if let Err(message) = status(expected, actual.status) {
        out.push(Mismatch {
            at: "status".into(),
            message,
        });
    }
    for (name, value) in &expected.headers {
        if let Err(message) = header(expected, name, value, actual.header(name)) {
            out.push(Mismatch {
                at: format!("header {name}"),
                message,
            });
        }
    }
    if let Some(body) = &expected.body {
        compare_body(expected, body, &actual.body, &mut out);
    }
    out
}

fn status(expected: &PactResponse, actual: u16) -> Result<(), String> {
    match &expected.rules.status {
        Some(set) => apply_set(set, &Value::from(expected.status), &Value::from(actual)),
        None if expected.status == actual => Ok(()),
        None => Err(format!("expected {}, got {actual}", expected.status)),
    }
}

fn header(
    expected: &PactResponse,
    name: &str,
    value: &str,
    actual: Option<&str>,
) -> Result<(), String> {
    let Some(actual) = actual else {
        return Err(format!("expected '{value}', got no header"));
    };
    match expected.rules.header(name) {
        Some(set) => apply_set(set, &Value::from(value), &Value::from(actual)),
        None if same_header(name, value, actual) => Ok(()),
        None => Err(format!("expected '{value}', got '{actual}'")),
    }
}

/// Values compare with the whitespace after commas ignored; a content type
/// matches with extra parameters (`application/json; charset=utf-8`).
fn same_header(name: &str, expected: &str, actual: &str) -> bool {
    let normalize = |v: &str| v.split(',').map(str::trim).collect::<Vec<_>>().join(",");
    if normalize(expected) == normalize(actual) {
        return true;
    }
    name.eq_ignore_ascii_case("content-type")
        && !expected.contains(';')
        && actual.split(';').next().map(str::trim) == Some(expected.trim())
}

fn compare_body(expected: &PactResponse, body: &Value, actual: &[u8], out: &mut Vec<Mismatch>) {
    let parsed = if actual.is_empty() {
        None
    } else {
        serde_json::from_slice::<Value>(actual).ok()
    };
    let actual = match (body, parsed) {
        (_, Some(value)) => value,
        (Value::String(_), None) => Value::String(String::from_utf8_lossy(actual).into_owned()),
        (_, None) => {
            out.push(Mismatch {
                at: "body".into(),
                message: format!("expected {}, got a body that is not JSON", describe(body)),
            });
            return;
        }
    };
    let walker = Walker {
        rules: &expected.rules.body,
        out,
    };
    walker.walk(&Path::default(), body, &actual, None);
}

/// Compares bodies under the body rules.
struct Walker<'a, 'o> {
    rules: &'a [(String, RuleSet)],
    out: &'o mut Vec<Mismatch>,
}

impl Walker<'_, '_> {
    /// The most specific rule set whose path matches `path`.
    fn rule_for(&self, path: &Path) -> Option<&RuleSet> {
        self.rules
            .iter()
            .filter_map(|(key, set)| Path::parse(key).ok().map(|p| (p, set)))
            .filter(|(p, _)| p.matches(path))
            .max_by_key(|(p, _)| p.weight())
            .map(|(_, set)| set)
    }

    /// `inherited`: a `type` rule of an ancestor, which cascades.
    fn walk(mut self, path: &Path, expected: &Value, actual: &Value, inherited: Option<&RuleSet>) {
        self.node(path, expected, actual, inherited);
    }

    fn node(&mut self, path: &Path, expected: &Value, actual: &Value, inherited: Option<&RuleSet>) {
        let own = self.rule_for(path).cloned();
        match own.as_ref().or(inherited) {
            Some(set) => self.ruled(path, set, expected, actual),
            None => self.plain(path, expected, actual),
        }
    }

    fn ruled(&mut self, path: &Path, set: &RuleSet, expected: &Value, actual: &Value) {
        if let Err(message) = apply_set(set, expected, actual) {
            self.mismatch(path, message);
            return;
        }
        if cascades(set) {
            let inherited = RuleSet::one(Matcher::new("type"));
            self.children(path, expected, actual, Some(&inherited));
        }
    }

    fn plain(&mut self, path: &Path, expected: &Value, actual: &Value) {
        let same_shape = matches!(
            (expected, actual),
            (Value::Object(_), Value::Object(_)) | (Value::Array(_), Value::Array(_))
        );
        if same_shape {
            if let (Value::Array(e), Value::Array(a)) = (expected, actual)
                && e.len() != a.len()
            {
                let message = format!("expected {} items, got {}", e.len(), a.len());
                self.mismatch(path, message);
                return;
            }
            self.children(path, expected, actual, None);
        } else if !equal(expected, actual) {
            let message = format!("expected {}, got {}", describe(expected), describe(actual));
            self.mismatch(path, message);
        }
    }

    /// Object members by key; array items by index, or against the first
    /// expected item under a cascading rule (`eachLike`).
    fn children(
        &mut self,
        path: &Path,
        expected: &Value,
        actual: &Value,
        inherited: Option<&RuleSet>,
    ) {
        match (expected, actual) {
            (Value::Object(e), Value::Object(a)) => {
                for (key, value) in e {
                    let child = path.child(Step::Key(key.clone()));
                    match a.get(key) {
                        Some(found) => self.node(&child, value, found, inherited),
                        None => self
                            .mismatch(&child, format!("expected {}, got nothing", describe(value))),
                    }
                }
            }
            (Value::Array(e), Value::Array(a)) => {
                for (index, found) in a.iter().enumerate() {
                    let template = if inherited.is_some() {
                        e.first()
                    } else {
                        e.get(index)
                    };
                    if let Some(template) = template {
                        self.node(&path.child(Step::Index(index)), template, found, inherited);
                    }
                }
            }
            _ => {}
        }
    }

    fn mismatch(&mut self, path: &Path, message: String) {
        self.out.push(Mismatch {
            at: path.to_string(),
            message,
        });
    }
}

/// Whether children inherit the rule: a `type` match (with `min`/`max`).
fn cascades(set: &RuleSet) -> bool {
    set.matchers
        .iter()
        .any(|m| matches!(m.kind.as_str(), "type" | "min" | "max"))
}

/// Apply every matcher of `set` (`AND`), or any of them (`OR`).
fn apply_set(set: &RuleSet, expected: &Value, actual: &Value) -> Result<(), String> {
    let mut results = set.matchers.iter().map(|m| apply(m, expected, actual));
    if set.is_or() {
        let all: Vec<Result<(), String>> = results.collect();
        if all.iter().any(Result::is_ok) {
            return Ok(());
        }
        return all.into_iter().find_map(Result::err).map_or(Ok(()), Err);
    }
    results.find(Result::is_err).unwrap_or(Ok(()))
}

/// One matcher.
pub fn apply(matcher: &Matcher, expected: &Value, actual: &Value) -> Result<(), String> {
    match matcher.kind.as_str() {
        "type" | "min" | "max" => {
            same_type(expected, actual).and_then(|()| bounds(matcher, actual))
        }
        "equality" if equal(expected, actual) => Ok(()),
        "equality" => Err(format!(
            "expected {}, got {}",
            describe(expected),
            describe(actual)
        )),
        "regex" => regex(matcher, actual),
        "include" => include(matcher, expected, actual),
        "integer" | "decimal" | "number" => number(&matcher.kind, actual),
        "boolean" if actual.is_boolean() => Ok(()),
        "boolean" => Err(format!("expected a boolean, got {}", describe(actual))),
        "statusCode" => status_code(matcher, actual),
        other => Err(format!("unsupported matcher '{other}'")),
    }
}

fn same_type(expected: &Value, actual: &Value) -> Result<(), String> {
    if kind(expected) == kind(actual) {
        Ok(())
    } else {
        Err(format!(
            "expected {}, got {}",
            describe(expected),
            describe(actual)
        ))
    }
}

/// `min`/`max` on an array's length.
fn bounds(matcher: &Matcher, actual: &Value) -> Result<(), String> {
    let Some(items) = actual.as_array() else {
        return Ok(());
    };
    if let Some(min) = matcher.min.filter(|min| items.len() < *min) {
        return Err(format!(
            "expected at least {min} items, got {}",
            items.len()
        ));
    }
    if let Some(max) = matcher.max.filter(|max| items.len() > *max) {
        return Err(format!("expected at most {max} items, got {}", items.len()));
    }
    Ok(())
}

fn regex(matcher: &Matcher, actual: &Value) -> Result<(), String> {
    let pattern = matcher.regex.as_deref().unwrap_or_default();
    // Pact matches the whole value (Pact JVM's `String.matches`), not a
    // substring, so the consumer's pattern is anchored at both ends.
    let anchored = format!("^(?:{pattern})$");
    let re = Regex::new(&anchored).map_err(|_| format!("invalid regex '{pattern}'"))?;
    let text = match actual {
        Value::String(text) => text.clone(),
        Value::Number(_) | Value::Bool(_) => actual.to_string(),
        other => {
            return Err(format!(
                "expected a value matching '{pattern}', got {}",
                describe(other)
            ));
        }
    };
    if re.is_match(&text) {
        Ok(())
    } else {
        Err(format!(
            "expected a value matching '{pattern}', got {}",
            describe(actual)
        ))
    }
}

fn include(matcher: &Matcher, expected: &Value, actual: &Value) -> Result<(), String> {
    let wanted = match matcher.value.as_ref().unwrap_or(expected) {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    match actual {
        Value::String(text) if text.contains(&wanted) => Ok(()),
        other => Err(format!(
            "expected a string including '{wanted}', got {}",
            describe(other)
        )),
    }
}

fn number(kind: &str, actual: &Value) -> Result<(), String> {
    let ok = match (kind, actual) {
        ("integer", Value::Number(n)) => n.is_i64() || n.is_u64(),
        ("decimal", Value::Number(n)) => n.is_f64(),
        ("number", Value::Number(_)) => true,
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        let article = if kind == "integer" { "an" } else { "a" };
        Err(format!(
            "expected {article} {kind}, got {}",
            describe(actual)
        ))
    }
}

/// `statusCode`: a class (`info`, `success`, `redirect`,
/// `clientError`, `serverError`, `nonError`, `error`) or a list of codes.
fn status_code(matcher: &Matcher, actual: &Value) -> Result<(), String> {
    let code = actual.as_u64().unwrap_or_default();
    let ok = match &matcher.status {
        Some(Value::String(class)) => in_class(class, code),
        Some(Value::Array(codes)) => codes.iter().any(|c| c.as_u64() == Some(code)),
        _ => false,
    };
    if ok {
        return Ok(());
    }
    let wanted = matcher
        .status
        .as_ref()
        .map_or_else(String::new, |s| match s {
            Value::String(class) => class.clone(),
            other => other.to_string(),
        });
    Err(format!("expected {wanted}, got {code}"))
}

fn in_class(class: &str, code: u64) -> bool {
    match class {
        "information" | "info" => (100..200).contains(&code),
        "success" | "successful" => (200..300).contains(&code),
        "redirect" => (300..400).contains(&code),
        "clientError" => (400..500).contains(&code),
        "serverError" => (500..600).contains(&code),
        "nonError" => code < 400,
        "error" => code >= 400,
        _ => false,
    }
}

/// Numbers compare by value (`1` equals `1.0`).
fn equal(expected: &Value, actual: &Value) -> bool {
    match (expected, actual) {
        (Value::Number(e), Value::Number(a)) => e.as_f64() == a.as_f64(),
        _ => expected == actual,
    }
}

fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// `number 42`, `string "42"`, `object`, `array of 2 items`.
pub fn describe(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Object(_) => "object".to_owned(),
        Value::Array(items) => format!("array of {} items", items.len()),
        other => format!("{} {}", kind(other), shorten(&other.to_string())),
    }
}

fn shorten(text: &str) -> String {
    const MAX: usize = 80;
    match text.char_indices().nth(MAX) {
        Some((end, _)) => format!("{}...", &text[..end]),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn response(body: Value, rules: &[(&str, Matcher)]) -> PactResponse {
        let mut expected = PactResponse {
            status: 200,
            body: Some(body),
            ..PactResponse::default()
        };
        expected.rules.body = rules
            .iter()
            .map(|(p, m)| ((*p).to_owned(), RuleSet::one(m.clone())))
            .collect();
        expected
    }

    fn actual(body: Value) -> Actual {
        Actual {
            status: 200,
            headers: Vec::new(),
            body: body.to_string().into_bytes(),
        }
    }

    fn messages(expected: &PactResponse, body: Value) -> Vec<String> {
        compare(expected, &actual(body))
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn equality_names_the_path_and_both_types() {
        let expected = response(json!({"id": 42}), &[]);
        assert_eq!(
            messages(&expected, json!({"id": "42"})),
            vec![r#"$.id: expected number 42, got string "42""#]
        );
        assert!(messages(&expected, json!({"id": 42, "extra": true})).is_empty());
        let ruled = response(json!({"id": 42}), &[("$.id", Matcher::new("equality"))]);
        assert_eq!(
            messages(&ruled, json!({"id": "42"})),
            vec![r#"$.id: expected number 42, got string "42""#]
        );
        assert_eq!(
            messages(&expected, json!({})),
            vec!["$.id: expected number 42, got nothing"]
        );
    }

    #[test]
    fn type_matches_any_value_of_the_type_and_cascades() {
        let expected = response(
            json!({"user": {"name": "Ana", "age": 3}}),
            &[("$.user", Matcher::new("type"))],
        );
        assert!(messages(&expected, json!({"user": {"name": "Bo", "age": 9}})).is_empty());
        assert_eq!(
            messages(&expected, json!({"user": {"name": 1, "age": 9}})),
            vec!["$.user.name: expected string \"Ana\", got number 1"]
        );
    }

    #[test]
    fn regex_include_and_numbers() {
        let regex = response(
            json!({"d": "2026-01-01"}),
            &[("$.d", Matcher::regex(r"^\d{4}-\d\d-\d\d$"))],
        );
        assert!(messages(&regex, json!({"d": "1999-12-31"})).is_empty());
        assert_eq!(
            messages(&regex, json!({"d": "soon"})),
            vec![r#"$.d: expected a value matching '^\d{4}-\d\d-\d\d$', got string "soon""#]
        );
        let include = Matcher {
            value: Some(json!("lo")),
            ..Matcher::new("include")
        };
        let include = response(json!({"s": "hello"}), &[("$.s", include)]);
        assert!(messages(&include, json!({"s": "below"})).is_empty());
        assert_eq!(messages(&include, json!({"s": "x"})).len(), 1);
        let ints = response(json!({"n": 1}), &[("$.n", Matcher::new("integer"))]);
        assert!(messages(&ints, json!({"n": 7})).is_empty());
        assert_eq!(
            messages(&ints, json!({"n": 1.5})),
            vec!["$.n: expected an integer, got number 1.5"]
        );
        let decimals = response(json!({"n": 1.5}), &[("$.n", Matcher::new("decimal"))]);
        assert!(messages(&decimals, json!({"n": 2.25})).is_empty());
        assert_eq!(messages(&decimals, json!({"n": 2})).len(), 1);
    }

    /// Regression: the regex matcher accepted a value containing a match
    /// anywhere; Pact requires the whole value to match.
    #[test]
    fn a_regex_must_match_the_whole_value() {
        let regex = response(
            json!({"id": "ab-12"}),
            &[("$.id", Matcher::regex(r"[a-z]+-\d+"))],
        );
        assert!(messages(&regex, json!({"id": "xy-99"})).is_empty());
        assert_eq!(
            messages(&regex, json!({"id": "id: xy-99!"})),
            vec![r#"$.id: expected a value matching '[a-z]+-\d+', got string "id: xy-99!""#]
        );
        let alternation = response(json!({"s": "a"}), &[("$.s", Matcher::regex("a|b"))]);
        assert!(messages(&alternation, json!({"s": "b"})).is_empty());
        assert_eq!(messages(&alternation, json!({"s": "ab"})).len(), 1);
    }

    #[test]
    fn each_like_checks_every_item_and_the_bounds() {
        let each_like = Matcher {
            min: Some(1),
            max: Some(2),
            ..Matcher::new("type")
        };
        let expected = response(json!({"items": [{"id": 1}]}), &[("$.items", each_like)]);
        assert!(messages(&expected, json!({"items": [{"id": 5}, {"id": 6}]})).is_empty());
        assert_eq!(
            messages(&expected, json!({"items": []})),
            vec!["$.items: expected at least 1 items, got 0"]
        );
        assert_eq!(
            messages(
                &expected,
                json!({"items": [{"id": 1}, {"id": 2}, {"id": 3}]})
            ),
            vec!["$.items: expected at most 2 items, got 3"]
        );
        assert_eq!(
            messages(&expected, json!({"items": [{"id": "x"}]})),
            vec![r#"$.items[0].id: expected number 1, got string "x""#]
        );
        let wildcard = response(
            json!({"items": [{"id": 1}]}),
            &[
                ("$.items", Matcher::new("type")),
                ("$.items[*].id", Matcher::new("integer")),
            ],
        );
        assert_eq!(
            messages(&wildcard, json!({"items": [{"id": 2}, {"id": 2.5}]})),
            vec!["$.items[1].id: expected an integer, got number 2.5"]
        );
    }

    /// The specification compares the actual length with `min`/`max`
    /// whatever the example holds; `pact_matching` skips list matchers on
    /// an empty example (docs/contract.md, "Interop with the official
    /// verifier"). A generated `length` assertion has an empty example.
    #[test]
    fn bounds_apply_to_the_actual_length_when_the_example_is_empty() {
        let length = Matcher {
            min: Some(2),
            max: Some(2),
            ..Matcher::new("type")
        };
        let expected = response(json!({"tags": []}), &[("$.tags", length)]);
        assert!(messages(&expected, json!({"tags": ["a", 1]})).is_empty());
        assert_eq!(
            messages(&expected, json!({"tags": []})),
            vec!["$.tags: expected at least 2 items, got 0"]
        );
        assert_eq!(
            messages(&expected, json!({"tags": [1, 2, 3]})),
            vec!["$.tags: expected at most 2 items, got 3"]
        );
    }

    #[test]
    fn arrays_without_rules_must_have_the_same_items() {
        let expected = response(json!([1, 2]), &[]);
        assert_eq!(
            messages(&expected, json!([1])),
            vec!["$: expected 2 items, got 1"]
        );
    }

    #[test]
    fn status_and_headers() {
        let mut expected = PactResponse {
            status: 200,
            headers: vec![("Content-Type".into(), "application/json".into())],
            ..PactResponse::default()
        };
        let got = |status, headers: Vec<(&str, &str)>| Actual {
            status,
            headers: headers
                .into_iter()
                .map(|(n, v)| (n.to_owned(), v.to_owned()))
                .collect(),
            body: Vec::new(),
        };
        let ok = got(
            200,
            vec![("content-type", "application/json; charset=utf-8")],
        );
        assert!(compare(&expected, &ok).is_empty());
        let wrong = compare(&expected, &got(404, vec![]));
        assert_eq!(
            wrong.iter().map(ToString::to_string).collect::<Vec<_>>(),
            vec![
                "status: expected 200, got 404",
                "header Content-Type: expected 'application/json', got no header"
            ]
        );
        expected.headers.clear();
        expected.rules.status = Some(RuleSet::one(Matcher {
            status: Some(json!("success")),
            ..Matcher::new("statusCode")
        }));
        assert!(compare(&expected, &got(201, vec![])).is_empty());
        assert_eq!(
            compare(&expected, &got(500, vec![]))[0].to_string(),
            "status: expected success, got 500"
        );
    }

    #[test]
    fn or_combines_any_matcher() {
        let set = RuleSet {
            combine: "OR".into(),
            matchers: vec![Matcher::new("integer"), Matcher::regex("^n/a$")],
        };
        assert!(apply_set(&set, &json!(1), &json!("n/a")).is_ok());
        assert!(apply_set(&set, &json!(1), &json!(3)).is_ok());
        assert!(apply_set(&set, &json!(1), &json!("x")).is_err());
        assert_eq!(
            apply(&Matcher::new("semver"), &json!(1), &json!(1)).unwrap_err(),
            "unsupported matcher 'semver'"
        );
    }
}
