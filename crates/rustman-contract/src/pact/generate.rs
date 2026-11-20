//! `rustman contract pact generate` (contract-testing "Pact generation from
//! a collection", "Pact specification versions"): one interaction per
//! `http` request, its request from the request file and its response from
//! the assertions ([`super::expect`]).
//!
//! Every request is first resolved with `rustman_core::vars::resolve_request`
//! (an unresolved variable stops generation). The Pact request is then
//! written from the templates, with each secret as `«redacted:<name>»` and
//! each dynamic value as `«dynamic:<placeholder>»`; every string that holds
//! one of those becomes a `regex` matcher (`Bearer {{token}}` gives
//! `^Bearer .+$`). Finally every string goes through the environment's
//! [`Redactor`], so no secret value is ever written.

use regex::Regex;
use rustman_core::environment::Environment;
use rustman_core::model::{ApiKeyLocation, Auth, Body, HttpRequest, JsonContent, NameValue};
use rustman_core::settings::Settings;
use rustman_core::vars::source::{Lookup, SecretLookup};
use rustman_core::vars::{ResolveError, Resolver, Segment, Template, resolve_request};
use rustman_core::{Redactor, RequestRef};
use serde_json::Value;

use super::expect::{ExpectContext, expectation};
use super::model::{Interaction, Matcher, Pact, PactRequest, PactVersion, RuleSet};
use super::path::{Path, Step};

/// What generation reads besides the requests.
pub struct Generator<'a> {
    pub settings: &'a Settings,
    pub environment: Option<&'a Environment>,
    pub secrets: &'a dyn SecretLookup,
    /// The active environment's secret values.
    pub redactor: &'a Redactor,
    pub version: PactVersion,
    /// The status last recorded for a request file, for `status: 2xx` in v3.
    pub recorded_status: &'a dyn Fn(&str) -> Option<u16>,
}

/// A generated Pact and what was left out of it.
#[derive(Clone, Debug)]
pub struct Generated {
    pub pact: Pact,
    pub warnings: Vec<String>,
}

/// A request that cannot be resolved; nothing is written.
#[derive(Debug, thiserror::Error)]
#[error("{file}: {error}")]
pub struct GenerateError {
    pub file: String,
    pub error: ResolveError,
}

impl Generator<'_> {
    /// The Pact between `consumer` and `provider` from `requests`, in order.
    pub fn generate(
        &self,
        consumer: &str,
        provider: &str,
        requests: &[RequestRef<'_>],
    ) -> Result<Generated, GenerateError> {
        let mut warnings = Vec::new();
        let mut interactions = Vec::new();
        for request in requests {
            let file = request.entry.path.to_string_lossy().replace('\\', "/");
            let rustman_core::model::Request::Http(http) = request.request() else {
                let kind = request.request().kind();
                warnings.push(format!(
                    "{file}: kind {kind} skipped (only http requests become Pact interactions)"
                ));
                continue;
            };
            let interaction = self.interaction(request, http, &file, &mut warnings)?;
            interactions.push(interaction);
        }
        let pact = Pact {
            consumer: consumer.to_owned(),
            provider: provider.to_owned(),
            version: self.version,
            interactions,
        };
        Ok(Generated { pact, warnings })
    }

    fn interaction(
        &self,
        request: &RequestRef<'_>,
        http: &HttpRequest,
        file: &str,
        warnings: &mut Vec<String>,
    ) -> Result<Interaction, GenerateError> {
        let resolver =
            Resolver::for_request(self.settings, request, self.environment, self.secrets);
        let resolved = resolve_request(request, &resolver).map_err(|error| GenerateError {
            file: file.to_owned(),
            error,
        })?;
        let mut redactor = Redactor::for_request(&resolved);
        redactor.merge(self.redactor);
        let render = Render {
            resolver: &resolver,
        };
        let mut pact_request = render.request(request, http, &resolved.method, file, warnings);
        redact_request(&mut pact_request, &redactor);
        add_placeholder_rules(&mut pact_request);
        let context = ExpectContext {
            file,
            version: self.version,
            recorded_status: (self.recorded_status)(file),
        };
        let mut expected = expectation(http.assertions.as_deref().unwrap_or_default(), context);
        warnings.append(&mut expected.warnings);
        let mut response = expected.response;
        for (_, value) in &mut response.headers {
            *value = redactor.text(value).into_owned();
        }
        if let Some(body) = &mut response.body {
            redactor.json(body);
        }
        Ok(Interaction {
            description: http.name.clone(),
            provider_states: Vec::new(),
            request: pact_request,
            response,
        })
    }
}

/// Writes templates with placeholders for secrets and dynamic values.
struct Render<'r, 'a> {
    resolver: &'r Resolver<'a>,
}

impl Render<'_, '_> {
    fn text(&self, template: &str) -> String {
        let Ok(template) = Template::parse(template) else {
            return template.to_owned();
        };
        let mut out = String::new();
        for segment in template.segments() {
            match segment {
                Segment::Literal(text) => out.push_str(text),
                Segment::Var(name) => out.push_str(&self.variable(name)),
                Segment::Dynamic(dynamic) => out.push_str(&format!("«dynamic:{dynamic}»")),
                Segment::Secret(reference) => out.push_str(&format!("«redacted:{reference}»")),
            }
        }
        out
    }

    fn variable(&self, name: &str) -> String {
        match self.resolver.lookup(name) {
            Some(Lookup::Value(value)) => value,
            _ => format!("«redacted:{name}»"),
        }
    }

    fn request(
        &self,
        request: &RequestRef<'_>,
        http: &HttpRequest,
        method: &str,
        file: &str,
        warnings: &mut Vec<String>,
    ) -> PactRequest {
        let url = self.text(&http.url);
        let (path, query) = split_url(&url);
        let mut out = PactRequest {
            method: method.to_owned(),
            path,
            query,
            ..PactRequest::default()
        };
        for pair in http.query.iter().flatten().filter(|p| p.enabled) {
            push_query(
                &mut out.query,
                self.text(&pair.name),
                self.text(&pair.value),
            );
        }
        out.headers = self.pairs(&request.effective_headers());
        self.auth(request.effective_auth(), &mut out, file, warnings);
        self.body(http.body.as_ref(), &mut out, file, warnings);
        out
    }

    fn pairs(&self, pairs: &[NameValue]) -> Vec<(String, String)> {
        pairs
            .iter()
            .filter(|p| p.enabled)
            .map(|p| (self.text(&p.name), self.text(&p.value)))
            .collect()
    }

    fn auth(
        &self,
        auth: Option<&Auth>,
        out: &mut PactRequest,
        file: &str,
        warnings: &mut Vec<String>,
    ) {
        let header = match auth {
            None | Some(Auth::None) => return,
            Some(Auth::Bearer { token }) => format!("Bearer {}", self.text(token)),
            Some(Auth::Basic { .. }) => "Basic «redacted:Authorization»".to_owned(),
            Some(Auth::ApiKey {
                name,
                value,
                location,
            }) => {
                let (name, value) = (self.text(name), self.text(value));
                if *location == Some(ApiKeyLocation::Query) {
                    return push_query(&mut out.query, name, value);
                }
                return set_header(&mut out.headers, &name, value);
            }
            Some(_) => {
                return warnings.push(format!(
                    "{file}: this auth type has no Pact equivalent; the Authorization header is left out"
                ));
            }
        };
        set_header(&mut out.headers, "Authorization", header);
    }

    fn body(
        &self,
        body: Option<&Body>,
        out: &mut PactRequest,
        file: &str,
        warnings: &mut Vec<String>,
    ) {
        let (value, content_type) = match body {
            None | Some(Body::None) => return,
            Some(Body::Json(JsonContent::Value(value))) => (self.json(value), "application/json"),
            Some(Body::Json(JsonContent::Text(text))) => {
                let text = self.text(text);
                let value = serde_json::from_str(&text).unwrap_or(Value::String(text));
                (value, "application/json")
            }
            Some(Body::Text(text)) => (Value::String(self.text(text)), "text/plain"),
            Some(Body::Form(fields)) => {
                let fields = self.pairs(fields);
                let encoded = form_urlencoded::Serializer::new(String::new())
                    .extend_pairs(fields)
                    .finish();
                (Value::String(encoded), "application/x-www-form-urlencoded")
            }
            Some(_) => {
                return warnings.push(format!(
                    "{file}: multipart and file bodies have no Pact equivalent; the body is left out"
                ));
            }
        };
        if !out
            .headers
            .iter()
            .any(|(n, _)| n.eq_ignore_ascii_case("content-type"))
        {
            out.headers
                .push(("Content-Type".to_owned(), content_type.to_owned()));
        }
        out.body = Some(value);
    }

    /// A JSON body with every string (keys included) written from its
    /// template.
    fn json(&self, value: &Value) -> Value {
        match value {
            Value::String(text) => Value::String(self.text(text)),
            Value::Array(items) => Value::Array(items.iter().map(|v| self.json(v)).collect()),
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(k, v)| (self.text(k), self.json(v)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }
}

/// The path and query of a URL (`http://host/users/42?a=1` gives
/// `/users/42` and `a=1`).
fn split_url(url: &str) -> (String, Vec<(String, Vec<String>)>) {
    let without_origin = match url.split_once("://") {
        Some((_, rest)) => rest.find('/').map_or("/", |at| &rest[at..]),
        None => url,
    };
    let (path, query) = without_origin
        .split_once('?')
        .unwrap_or((without_origin, ""));
    let mut pairs = Vec::new();
    for (name, value) in form_urlencoded::parse(query.as_bytes()).into_owned() {
        push_query(&mut pairs, name, value);
    }
    let path = if path.is_empty() { "/" } else { path };
    (path.to_owned(), pairs)
}

fn push_query(query: &mut Vec<(String, Vec<String>)>, name: String, value: String) {
    match query.iter_mut().find(|(n, _)| *n == name) {
        Some((_, values)) => values.push(value),
        None => query.push((name, vec![value])),
    }
}

fn set_header(headers: &mut Vec<(String, String)>, name: &str, value: String) {
    headers.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
    headers.push((name.to_owned(), value));
}

fn redact_request(request: &mut PactRequest, redactor: &Redactor) {
    let clean = |text: &mut String| *text = redactor.text(text).into_owned();
    clean(&mut request.path);
    for (_, values) in &mut request.query {
        values.iter_mut().for_each(clean);
    }
    for (_, value) in &mut request.headers {
        clean(value);
    }
    if let Some(body) = &mut request.body {
        redactor.json(body);
    }
}

/// `«redacted:...»` and `«dynamic:...»` markers.
fn placeholder() -> Regex {
    Regex::new("«(redacted|dynamic):[^»]*»").expect("a valid pattern")
}

/// The regex a string with placeholders must match: the text around them
/// literally, each placeholder as `.+`.
pub fn placeholder_regex(text: &str) -> Option<String> {
    let markers = placeholder();
    if !markers.is_match(text) {
        return None;
    }
    let mut out = String::from("^");
    let mut last = 0;
    for found in markers.find_iter(text) {
        out.push_str(&regex::escape(&text[last..found.start()]));
        out.push_str(".+");
        last = found.end();
    }
    out.push_str(&regex::escape(&text[last..]));
    out.push('$');
    Some(out)
}

fn add_placeholder_rules(request: &mut PactRequest) {
    let rule = |text: &str| placeholder_regex(text).map(|r| RuleSet::one(Matcher::regex(&r)));
    request.rules.path = rule(&request.path);
    for (name, values) in &request.query {
        if let Some(set) = values.iter().find_map(|v| rule(v)) {
            request.rules.query.push((name.clone(), set));
        }
    }
    for (name, value) in &request.headers {
        if let Some(set) = rule(value) {
            request.rules.header.push((name.clone(), set));
        }
    }
    if let Some(body) = &request.body {
        let mut found = Vec::new();
        body_rules(&Path::default(), body, &mut found);
        request.rules.body = found;
    }
}

fn body_rules(path: &Path, value: &Value, out: &mut Vec<(String, RuleSet)>) {
    match value {
        Value::String(text) => {
            if let Some(regex) = placeholder_regex(text) {
                out.push((path.to_string(), RuleSet::one(Matcher::regex(&regex))));
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                body_rules(&path.child(Step::Index(index)), item, out);
            }
        }
        Value::Object(map) => {
            for (key, item) in map {
                body_rules(&path.child(Step::Key(key.clone())), item, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_become_anchored_regexes() {
        assert_eq!(
            placeholder_regex("Bearer «redacted:token»").as_deref(),
            Some("^Bearer .+$")
        );
        assert_eq!(
            placeholder_regex("a.«dynamic:$uuid»?«redacted:x»").as_deref(),
            Some(r"^a\..+\?.+$")
        );
        assert_eq!(placeholder_regex("plain"), None);
    }

    #[test]
    fn urls_split_into_path_and_query() {
        assert_eq!(
            split_url("http://h:1/users/42?a=1&a=2&b=x%20y"),
            (
                "/users/42".to_owned(),
                vec![
                    ("a".to_owned(), vec!["1".to_owned(), "2".to_owned()]),
                    ("b".to_owned(), vec!["x y".to_owned()]),
                ]
            )
        );
        assert_eq!(split_url("https://h").0, "/");
        assert_eq!(split_url("/only/path").0, "/only/path");
    }
}
