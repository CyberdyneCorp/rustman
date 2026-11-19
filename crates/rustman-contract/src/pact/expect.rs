//! The expected response of a generated interaction, from the request's
//! assertions (design D5):
//!
//! | Rustman assertion | Pact |
//! |---|---|
//! | `status: 200` | response status 200 |
//! | `status: "2xx"` | v4: a `statusCode` rule (`success`); v3: the recorded status |
//! | `status: [200, 201]` | v4: a `statusCode` rule with the codes; v3: the first code |
//! | `header: {equals}` / `{matches}` / `{contains}` | `equality` / `regex` / `include` |
//! | `json: {path, equals}` | the value at the path, with `equality` |
//! | `json: {path, type}` | `type` (`integer` for integers) |
//! | `json: {path, matches}` | `regex`, matching anywhere in the value (`full_match`) |
//! | `json: {path, contains}` (text) | `include` |
//! | `json: {path, length}` | `type` with `min` = `max` = length (`eachLike`) |
//!
//! Every other assertion (`schema`, `latency`, `body`, comparisons, ...)
//! is omitted with a warning naming it and the request file.

use rustman_core::model::{Assertion, HeaderAssertion, JsonAssertion, StatusExpectation};
use serde_json::{Value, json};

use super::model::{Matcher, PactResponse, PactVersion, RuleSet};
use super::path::{Path, Step};

/// What the response of one interaction is expected to be.
#[derive(Clone, Debug, Default)]
pub struct Expectation {
    pub response: PactResponse,
    pub warnings: Vec<String>,
}

/// Inputs besides the assertions.
#[derive(Clone, Copy, Debug)]
pub struct ExpectContext<'a> {
    /// The request file, as warnings name it.
    pub file: &'a str,
    pub version: PactVersion,
    /// The status recorded for this request, for a status class in v3.
    pub recorded_status: Option<u16>,
}

/// The expected response for `assertions`.
pub fn expectation(assertions: &[Assertion], context: ExpectContext<'_>) -> Expectation {
    let mut builder = Builder {
        context,
        out: Expectation::default(),
        status: None,
    };
    for assertion in assertions {
        builder.assertion(assertion);
    }
    let mut out = builder.out;
    out.response.status = builder.status.unwrap_or(200);
    out
}

struct Builder<'a> {
    context: ExpectContext<'a>,
    out: Expectation,
    status: Option<u16>,
}

impl Builder<'_> {
    fn assertion(&mut self, assertion: &Assertion) {
        match assertion {
            Assertion::Status(status) => self.status(status),
            Assertion::Header(header) => self.header(header),
            Assertion::Json(json) => self.json(json),
            other => self.omit(assertion_name(other)),
        }
    }

    fn omit(&mut self, what: &str) {
        let file = self.context.file;
        self.out
            .warnings
            .push(format!("{what} assertion has no Pact equivalent ({file})"));
    }

    fn status(&mut self, status: &StatusExpectation) {
        let v4 = self.context.version == PactVersion::V4;
        match status {
            StatusExpectation::Code(code) => self.status = Some(*code),
            StatusExpectation::AnyOf(codes) => {
                self.status = codes.first().copied();
                if v4 && codes.len() > 1 {
                    self.status_rule(json!(codes));
                }
            }
            StatusExpectation::Class(digit) => self.status_class(*digit, v4),
        }
    }

    fn status_class(&mut self, digit: u8, v4: bool) {
        let class = class_name(digit);
        let fallback = u16::from(digit) * 100;
        let recorded = self
            .context
            .recorded_status
            .filter(|s| s / 100 == u16::from(digit));
        self.status = Some(recorded.unwrap_or(fallback));
        if v4 {
            self.status_rule(json!(class));
        } else if recorded.is_none() {
            let file = self.context.file;
            self.out.warnings.push(format!(
                "status {digit}xx has no Pact v3 equivalent and no status is recorded; expecting {fallback} ({file})"
            ));
        }
    }

    fn status_rule(&mut self, status: Value) {
        let matcher = Matcher {
            status: Some(status),
            ..Matcher::new("statusCode")
        };
        self.out.response.rules.status = Some(RuleSet::one(matcher));
    }

    fn header(&mut self, header: &HeaderAssertion) {
        let (value, matcher) = if let Some(value) = &header.equals {
            (value.clone(), Matcher::new("equality"))
        } else if let Some(pattern) = &header.matches {
            (pattern.clone(), Matcher::regex(&full_match(pattern)))
        } else if let Some(part) = &header.contains {
            let include = Matcher {
                value: Some(json!(part)),
                ..Matcher::new("include")
            };
            (part.clone(), include)
        } else if header.exists == Some(true) {
            (String::new(), Matcher::regex(".*"))
        } else {
            return self.omit("header exists: false");
        };
        let response = &mut self.out.response;
        response.headers.push((header.name.clone(), value));
        response
            .rules
            .header
            .push((header.name.clone(), RuleSet::one(matcher)));
    }

    fn json(&mut self, json: &JsonAssertion) {
        let Ok(path) = Path::parse(&json.path) else {
            let file = self.context.file;
            let path = &json.path;
            return self.out.warnings.push(format!(
                "json assertion path '{path}' has no Pact equivalent ({file})"
            ));
        };
        match json_rule(json) {
            Ok((example, matcher)) => {
                let response = &mut self.out.response;
                insert(response.body.get_or_insert(Value::Null), &path.0, example);
                response
                    .rules
                    .body
                    .push((path.to_string(), RuleSet::one(matcher)));
            }
            Err(operator) => self.omit(&format!("json {operator}")),
        }
    }
}

/// The example value and matcher of a `json` assertion, or the name of
/// its operator when Pact has no equivalent.
fn json_rule(json: &JsonAssertion) -> Result<(Value, Matcher), &'static str> {
    if let Some(value) = &json.equals {
        return Ok((value.clone(), Matcher::new("equality")));
    }
    if let Some(kind) = &json.value_type {
        return type_rule(kind).ok_or("type");
    }
    if let Some(pattern) = &json.matches {
        return Ok((json!(pattern), Matcher::regex(&full_match(pattern))));
    }
    if let Some(Value::String(part)) = &json.contains {
        let include = Matcher {
            value: Some(json!(part)),
            ..Matcher::new("include")
        };
        return Ok((json!(part), include));
    }
    if let Some(length) = json.length.as_ref().and_then(Value::as_u64) {
        let length = usize::try_from(length).unwrap_or(usize::MAX);
        let each_like = Matcher {
            min: Some(length),
            max: Some(length),
            ..Matcher::new("type")
        };
        return Ok((json!([]), each_like));
    }
    Err(operator(json))
}

/// A Rustman `matches` pattern as a Pact `regex`. Rustman finds the
/// pattern anywhere in the value; a Pact regex must match the whole value.
/// A pattern anchored at both ends (`^\d+$`) means the same either way and
/// is kept; any other one is wrapped so it may match anywhere.
fn full_match(pattern: &str) -> String {
    let anchored = pattern.starts_with('^')
        && pattern.ends_with('$')
        && !pattern.ends_with("\\$")
        && !pattern.contains('|');
    if anchored {
        pattern.to_owned()
    } else {
        format!("[\\s\\S]*(?:{pattern})[\\s\\S]*")
    }
}

fn type_rule(kind: &str) -> Option<(Value, Matcher)> {
    let (example, matcher) = match kind {
        "integer" => (json!(0), "integer"),
        "number" => (json!(0), "type"),
        "string" => (json!("string"), "type"),
        "boolean" => (json!(true), "type"),
        "object" => (json!({}), "type"),
        "array" => (json!([]), "type"),
        "null" => (Value::Null, "type"),
        _ => return None,
    };
    Some((example, Matcher::new(matcher)))
}

fn operator(json: &JsonAssertion) -> &'static str {
    let given = [
        (json.not_equals.is_some(), "not_equals"),
        (json.contains.is_some(), "contains"),
        (json.gt.is_some(), "gt"),
        (json.gte.is_some(), "gte"),
        (json.lt.is_some(), "lt"),
        (json.lte.is_some(), "lte"),
        (json.exists.is_some(), "exists"),
        (json.length.is_some(), "length"),
    ];
    given
        .iter()
        .find(|(set, _)| *set)
        .map_or("assertion", |(_, name)| name)
}

fn class_name(digit: u8) -> &'static str {
    match digit {
        1 => "info",
        2 => "success",
        3 => "redirect",
        4 => "clientError",
        _ => "serverError",
    }
}

fn assertion_name(assertion: &Assertion) -> &'static str {
    match assertion {
        Assertion::Schema(_) => "schema",
        Assertion::Body(_) => "body",
        Assertion::Latency(_) => "latency",
        Assertion::Snapshot(_) => "snapshot",
        Assertion::Status(_) => "status",
        Assertion::Header(_) => "header",
        Assertion::Json(_) => "json",
        _ => "this",
    }
}

/// Put `value` at `steps` below `node`, creating objects and arrays (padded
/// with `null`) on the way.
pub fn insert(node: &mut Value, steps: &[Step], value: Value) {
    let Some((step, rest)) = steps.split_first() else {
        *node = value;
        return;
    };
    let child = match step {
        Step::Key(key) => member(node, key),
        Step::Index(index) => item(node, *index),
        Step::Any => item(node, 0),
    };
    insert(child, rest, value);
}

fn member<'v>(node: &'v mut Value, key: &str) -> &'v mut Value {
    if !node.is_object() {
        *node = json!({});
    }
    let map = node.as_object_mut().expect("just made an object");
    map.entry(key.to_owned()).or_insert(Value::Null)
}

fn item(node: &mut Value, index: usize) -> &mut Value {
    if !node.is_array() {
        *node = json!([]);
    }
    let items = node.as_array_mut().expect("just made an array");
    if items.len() <= index {
        items.resize(index + 1, Value::Null);
    }
    &mut items[index]
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustman_core::model::Assertion;

    fn context(version: PactVersion) -> ExpectContext<'static> {
        ExpectContext {
            file: "get-user.rman.yaml",
            version,
            recorded_status: None,
        }
    }

    fn parse(yaml: &str) -> Vec<Assertion> {
        rustman_core::format::from_yaml(std::path::Path::new("a.yaml"), yaml).unwrap()
    }

    #[test]
    fn status_and_json_equals_become_an_equality_body() {
        let out = expectation(
            &parse("- status: 200\n- json: {path: \"$.id\", equals: 42}\n"),
            context(PactVersion::V4),
        );
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
        assert_eq!(out.response.status, 200);
        assert_eq!(out.response.body, Some(json!({"id": 42})));
        assert_eq!(out.response.rules.body[0].0, "$.id");
        assert_eq!(out.response.rules.body[0].1.matchers[0].kind, "equality");
    }

    /// Regression: with the Pact regex matcher matching the whole value, a
    /// Rustman `matches: "^A"` (found anywhere) must not become `^A` as is.
    #[test]
    fn search_patterns_become_whole_value_regexes() {
        assert_eq!(full_match("^\\d+$"), "^\\d+$");
        assert_eq!(full_match("^A"), "[\\s\\S]*(?:^A)[\\s\\S]*");
        assert_eq!(full_match("^a|b$"), "[\\s\\S]*(?:^a|b$)[\\s\\S]*");
        assert_eq!(full_match("x\\$"), "[\\s\\S]*(?:x\\$)[\\s\\S]*");
        let whole = |pattern: &str, text: &str| {
            regex::Regex::new(&format!("^(?:{})$", full_match(pattern)))
                .unwrap()
                .is_match(text)
        };
        assert!(whole("^A", "Ana"));
        assert!(!whole("^A", "Bo"));
        assert!(whole("n", "Ana\nB"));
        assert!(whole("^\\d+$", "42"));
        assert!(!whole("^\\d+$", "4a"));
    }

    #[test]
    fn every_mapping_row() {
        let out = expectation(
            &parse(
                "- header: {name: X-Id, matches: \"^r-\"}\n\
                 - header: {name: Content-Type, equals: application/json}\n\
                 - header: {name: Server, contains: nginx}\n\
                 - json: {path: \"$.n\", type: integer}\n\
                 - json: {path: \"$.s\", type: string}\n\
                 - json: {path: \"$.d\", matches: \"^\\\\d+$\"}\n\
                 - json: {path: \"$.t\", contains: abc}\n\
                 - json: {path: \"$.items\", length: 2}\n\
                 - json: {path: \"$.a[1].b\", equals: x}\n",
            ),
            context(PactVersion::V4),
        );
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
        let kinds: Vec<(&str, &str)> = out
            .response
            .rules
            .body
            .iter()
            .map(|(p, r)| (p.as_str(), r.matchers[0].kind.as_str()))
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("$.n", "integer"),
                ("$.s", "type"),
                ("$.d", "regex"),
                ("$.t", "include"),
                ("$.items", "type"),
                ("$.a[1].b", "equality"),
            ]
        );
        assert_eq!(out.response.rules.body[4].1.matchers[0].min, Some(2));
        assert_eq!(out.response.rules.body[4].1.matchers[0].max, Some(2));
        let body = out.response.body.unwrap();
        assert_eq!(body["a"], json!([null, {"b": "x"}]));
        let headers: Vec<&str> = out
            .response
            .rules
            .header
            .iter()
            .map(|(_, r)| r.matchers[0].kind.as_str())
            .collect();
        assert_eq!(headers, vec!["regex", "equality", "include"]);
    }

    #[test]
    fn status_classes_per_version() {
        let v4 = expectation(&parse("- status: 2xx\n"), context(PactVersion::V4));
        assert_eq!(v4.response.status, 200);
        let rule = v4.response.rules.status.unwrap();
        assert_eq!(rule.matchers[0].status, Some(json!("success")));
        let mut recorded = context(PactVersion::V3);
        recorded.recorded_status = Some(201);
        let v3 = expectation(&parse("- status: 2xx\n"), recorded);
        assert_eq!(v3.response.status, 201);
        assert!(v3.response.rules.status.is_none());
        assert!(v3.warnings.is_empty());
        let unrecorded = expectation(&parse("- status: 2xx\n"), context(PactVersion::V3));
        assert_eq!(unrecorded.warnings.len(), 1);
        let any = expectation(&parse("- status: [200, 201]\n"), context(PactVersion::V4));
        assert_eq!(
            any.response.rules.status.unwrap().matchers[0].status,
            Some(json!([200, 201]))
        );
    }

    #[test]
    fn assertions_without_an_equivalent_are_omitted_with_a_warning() {
        let assertions = parse(
            "- schema: {inline: {type: object}}\n- json: {path: \"$.n\", gt: 1}\n- json: {path: \"$..n\", equals: 1}\n- latency: {total_lt: 300}\n",
        );
        let out = expectation(&assertions, context(PactVersion::V4));
        assert_eq!(
            out.warnings,
            vec![
                "schema assertion has no Pact equivalent (get-user.rman.yaml)",
                "json gt assertion has no Pact equivalent (get-user.rman.yaml)",
                "json assertion path '$..n' has no Pact equivalent (get-user.rman.yaml)",
                "latency assertion has no Pact equivalent (get-user.rman.yaml)",
            ]
        );
        assert_eq!(out.response.body, None);
    }
}
