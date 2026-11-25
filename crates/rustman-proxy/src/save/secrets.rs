//! Secrets in saved captures (spec: Secrets become placeholders, design D5
//! step 5). Credential headers are parameterized here with the names the
//! spec gives (`Authorization: Bearer {{auth_token}}`, `Cookie:
//! {{cookie}}`); every other secret (`X-Api-Key`-style headers, query and
//! body fields named like `token`, `password`, `secret`, `api_key`, known
//! token formats) is left to the importer's extraction, which runs on the
//! same collection right after. JSON body fields that are secrets only by
//! the object they sit in (`{"credentials": {"value": ...}}`, which the
//! importer judges by their own name) are parameterized here too, named
//! after the object and the field (`{{credentials_value}}`). Values that
//! only look random are reported, never moved.

use std::collections::HashMap;

use rustman_core::text::printable_line;
use rustman_import::ir::{ImportedBody, ImportedRequest, Pair};
use rustman_import::slug::var_name;
use serde_json::Value;

use crate::named::{is_secret_field, is_secret_header, nested_secrets};

/// Values longer than this are checked for high entropy.
pub const ENTROPY_MIN_LEN: usize = 32;

/// Bits per character above which a long value looks random.
const ENTROPY_BITS: f64 = 3.5;

/// The secret values moved out of credential headers, by variable name.
#[derive(Default)]
pub struct HeaderSecrets {
    /// Name to value, in the order they were first seen.
    pub moved: Vec<(String, String)>,
    by_value: HashMap<String, String>,
    /// Names the environment already gives other values (path variables,
    /// base URLs), which a placeholder must not take.
    reserved: Vec<String>,
}

impl HeaderSecrets {
    /// Keep `name` for a variable declared elsewhere (a path variable, a
    /// base URL): no placeholder takes it. A secret `value` it holds is
    /// written as `{{name}}` wherever it shows up, without being moved
    /// again.
    pub fn reserve(&mut self, name: &str, secret_value: Option<&str>) {
        self.reserved.push(name.to_owned());
        if let Some(value) = secret_value {
            self.by_value
                .entry(value.to_owned())
                .or_insert_with(|| name.to_owned());
        }
    }

    /// Replace the credentials of `request`'s `Authorization` and `Cookie`
    /// headers, and the nested JSON body fields that are secrets by where
    /// they sit, by placeholders. (`Proxy-*` headers are never saved.) A
    /// text body holding JSON (sent as `text/plain` or without a
    /// `Content-Type`) is judged as JSON too.
    pub fn parameterize(&mut self, request: &mut ImportedRequest) {
        if let Some(ImportedBody::Json { text } | ImportedBody::Text { text }) = &mut request.body {
            self.json_body(text);
        }
        for header in &mut request.headers {
            let lower = header.name.to_ascii_lowercase();
            match lower.as_str() {
                "authorization" => self.credentials(&mut header.value, "auth_token"),
                "cookie" => {
                    let value = std::mem::take(&mut header.value);
                    header.value = self.placeholder("cookie", value);
                }
                _ => {}
            }
        }
    }

    /// Replace the [`nested_secrets`] of the JSON `text` by placeholders,
    /// keeping its layout; when a value is written with escapes the
    /// placeholder search does not match, the body is written again from
    /// its parsed form instead, so no raw value is ever kept.
    fn json_body(&mut self, text: &mut String) {
        let Ok(mut parsed) = serde_json::from_str::<Value>(text) else {
            return;
        };
        let nested = nested_secrets(&parsed);
        if nested.is_empty() {
            return;
        }
        let swaps: Vec<(String, String)> = nested
            .into_iter()
            .map(|n| {
                let placeholder = self.placeholder(&var_name(&n.name, "secret"), n.value.clone());
                (n.value, placeholder)
            })
            .collect();
        let mut replaced = text.clone();
        for (raw, placeholder) in &swaps {
            replaced = replaced.replace(&json_string(raw), &json_string(placeholder));
        }
        let leaks = |t: &str| {
            serde_json::from_str::<Value>(t)
                .map_or(true, |v| swaps.iter().any(|(raw, _)| has_string(&v, raw)))
        };
        if leaks(&replaced) {
            substitute(&mut parsed, &swaps);
            replaced = serde_json::to_string_pretty(&parsed).expect("a JSON value serializes");
        }
        *text = replaced;
    }

    /// `Bearer abc` keeps its scheme: `Bearer {{auth_token}}`.
    fn credentials(&mut self, value: &mut String, base: &str) {
        let (scheme, credentials) = match value.split_once(' ') {
            Some((scheme, rest)) if !scheme.is_empty() && !rest.trim().is_empty() => {
                (format!("{scheme} "), rest.trim().to_owned())
            }
            _ => (String::new(), value.trim().to_owned()),
        };
        if credentials.is_empty() {
            return;
        }
        *value = format!("{scheme}{}", self.placeholder(base, credentials));
    }

    /// `{{name}}` for `value`: the same name for the same value, a new
    /// suffixed one (`auth_token_2`) for each new value.
    fn placeholder(&mut self, base: &str, value: String) -> String {
        if let Some(name) = self.by_value.get(&value) {
            return format!("{{{{{name}}}}}");
        }
        let taken = |n: &str| {
            self.moved.iter().any(|(m, _)| m == n) || self.reserved.iter().any(|r| r == n)
        };
        let name = (1..)
            .map(|n| match n {
                1 => base.to_owned(),
                n => format!("{base}_{n}"),
            })
            .find(|candidate| !taken(candidate))
            .expect("some suffix is free");
        self.by_value.insert(value.clone(), name.clone());
        self.moved.push((name.clone(), value));
        format!("{{{{{name}}}}}")
    }
}

/// `value` as a JSON string literal, quotes included.
fn json_string(value: &str) -> String {
    serde_json::to_string(value).expect("a string serializes")
}

/// Whether some string of `value` (at any depth) is `wanted`.
fn has_string(value: &Value, wanted: &str) -> bool {
    match value {
        Value::String(s) => s == wanted,
        Value::Array(items) => items.iter().any(|v| has_string(v, wanted)),
        Value::Object(map) => map.values().any(|v| has_string(v, wanted)),
        _ => false,
    }
}

/// Replace every string of `value` that `swaps` lists by its placeholder.
fn substitute(value: &mut Value, swaps: &[(String, String)]) {
    match value {
        Value::String(s) => {
            if let Some((_, placeholder)) = swaps.iter().find(|(raw, _)| raw == s) {
                *s = placeholder.clone();
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|v| substitute(v, swaps)),
        Value::Object(map) => map.values_mut().for_each(|v| substitute(v, swaps)),
        _ => {}
    }
}

/// Warnings for header, query and JSON body values of `request` that look
/// random but that the secret extraction keeps: neither their names nor
/// their values (a well-known token, an auth scheme) mark them as secrets.
/// Path segments kept in the URL are judged too, by position. Values are
/// never shown, and names are written escaped, since JSON keys from the
/// traffic may hold control characters (terminal escape sequences).
pub fn entropy_warnings(request: &ImportedRequest, shown: &str) -> Vec<String> {
    let headers = request
        .headers
        .iter()
        .filter(|h| !is_secret_header(&h.name, &h.value) && !is_descriptive_header(&h.name))
        .filter(|h| looks_random(&h.value))
        .map(|h| ("header", h));
    let query = request
        .query
        .iter()
        .filter(|p| !is_secret_field(&p.name, &p.value))
        .filter(|p| looks_random(&p.value))
        .map(|p| ("query parameter", p));
    let mut kept: Vec<(&str, String)> = headers
        .chain(query)
        .map(|(what, pair): (&str, &Pair)| (what, pair.name.clone()))
        .collect();
    if let Some(ImportedBody::Json { text } | ImportedBody::Text { text }) = &request.body
        && let Ok(body) = serde_json::from_str::<Value>(text)
    {
        let mut fields = Vec::new();
        random_json_fields(&body, "", "", &mut fields);
        kept.extend(fields.into_iter().map(|path| ("body field", path)));
    }
    // `{{base_url}}/a/b`: the segments after the base URL, counted from 1.
    let path = request.url.split_once("}}").map_or("", |(_, path)| path);
    kept.extend(
        path.split('/')
            .enumerate()
            .skip(1)
            .filter(|(_, segment)| !segment.contains("{{") && looks_random(segment))
            .map(|(index, _)| ("path segment", index.to_string())),
    );
    kept.into_iter()
        .map(|(what, name)| {
            format!(
                "warning: {what} '{}' of {} looks like a secret (high entropy) and was kept; \
                 review it before committing",
                printable_line(&name),
                printable_line(shown),
            )
        })
        .collect()
}

/// The dotted paths (`nested.refresh`) of the random-looking strings of a
/// JSON body that no secret rule catches; array items are judged under the
/// array's name.
fn random_json_fields(value: &Value, name: &str, path: &str, out: &mut Vec<String>) {
    match value {
        Value::String(s) => {
            if looks_random(s) && !is_secret_field(name, s) && !out.iter().any(|p| p == path) {
                out.push(path.to_owned());
            }
        }
        Value::Object(map) => {
            for (key, field) in map {
                let inner = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                random_json_fields(field, key, &inner, out);
            }
        }
        Value::Array(items) => items
            .iter()
            .for_each(|item| random_json_fields(item, name, path, out)),
        _ => {}
    }
}

/// Headers whose values describe the message (media types, encodings,
/// languages): standard vocabulary, never a secret, however varied.
const DESCRIPTIVE_HEADERS: [&str; 9] = [
    "accept",
    "accept-encoding",
    "accept-language",
    "content-encoding",
    "content-language",
    "content-type",
    "host",
    "transfer-encoding",
    "user-agent",
];

fn is_descriptive_header(name: &str) -> bool {
    DESCRIPTIVE_HEADERS
        .iter()
        .any(|known| known.eq_ignore_ascii_case(name))
}

/// A single token (no spaces or separators such as `,` and `;`) longer
/// than [`ENTROPY_MIN_LEN`], with high Shannon entropy per character.
pub fn looks_random(value: &str) -> bool {
    let token = value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_.~+/=".contains(c));
    token && value.chars().count() > ENTROPY_MIN_LEN && entropy(value) >= ENTROPY_BITS
}

fn entropy(value: &str) -> f64 {
    let mut counts: HashMap<char, usize> = HashMap::new();
    for c in value.chars() {
        *counts.entry(c).or_default() += 1;
    }
    let len = value.chars().count() as f64;
    counts
        .values()
        .map(|&n| {
            let p = n as f64 / len;
            -p * p.log2()
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(headers: &[(&str, &str)]) -> ImportedRequest {
        ImportedRequest {
            headers: headers.iter().map(|(n, v)| Pair::new(*n, *v)).collect(),
            ..ImportedRequest::default()
        }
    }

    #[test]
    fn credential_headers_become_named_placeholders() {
        let mut secrets = HeaderSecrets::default();
        let mut r = request(&[
            ("Authorization", "Bearer eyJhbGciOiJIUzI1NiJ9.e30.sig"),
            ("Cookie", "sid=abc; theme=dark"),
            ("Accept", "application/json"),
        ]);
        secrets.parameterize(&mut r);
        let values: Vec<&str> = r.headers.iter().map(|h| h.value.as_str()).collect();
        assert_eq!(
            values,
            ["Bearer {{auth_token}}", "{{cookie}}", "application/json"]
        );
        let mut again = request(&[("authorization", "Bearer eyJhbGciOiJIUzI1NiJ9.e30.sig")]);
        secrets.parameterize(&mut again);
        assert_eq!(again.headers[0].value, "Bearer {{auth_token}}");
        let mut other = request(&[("Authorization", "token-without-scheme")]);
        secrets.parameterize(&mut other);
        assert_eq!(other.headers[0].value, "{{auth_token_2}}");
        let names: Vec<&str> = secrets.moved.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["auth_token", "cookie", "auth_token_2"]);
    }

    #[test]
    fn long_random_values_under_unknown_names_are_reported_without_the_value() {
        let random = "q8ZK3vLw0pTn6RxYb2HsJm9CdFgA5eUi7oQ1";
        let r = ImportedRequest {
            headers: vec![
                Pair::new("X-Trace", random),
                Pair::new("X-Api-Key", random),
                Pair::new("Accept", "application/json, text/plain, */*;q=0.8"),
            ],
            query: vec![Pair::new("ref", random), Pair::new("token", random)],
            ..ImportedRequest::default()
        };
        let warnings = entropy_warnings(&r, "GET /a");
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(
            warnings[0].contains("header 'X-Trace' of GET /a"),
            "{}",
            warnings[0]
        );
        assert!(
            warnings[1].contains("query parameter 'ref'"),
            "{}",
            warnings[1]
        );
        assert!(warnings.iter().all(|w| !w.contains(random)));
        assert!(!looks_random("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
        assert!(!looks_random("short"));
    }

    #[test]
    fn standard_content_types_are_not_reported_as_secrets() {
        let r = request(&[
            ("Content-Type", "application/x-www-form-urlencoded"),
            ("content-type", "application/vnd.api+json"),
            ("Accept-Encoding", "gzip-deflate-br-zstd-identity"),
        ]);
        assert_eq!(entropy_warnings(&r, "POST /form"), Vec::<String>::new());
    }

    fn json_request(body: &str) -> ImportedRequest {
        ImportedRequest {
            body: Some(ImportedBody::Json { text: body.into() }),
            ..ImportedRequest::default()
        }
    }

    fn json_text(r: &ImportedRequest) -> &str {
        match &r.body {
            Some(ImportedBody::Json { text }) => text,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn nested_fields_of_credential_objects_become_placeholders() {
        let mut secrets = HeaderSecrets::default();
        let mut r = json_request(
            r#"{"credentials": {"type": "password", "value": "hunter2-nested"},
  "authentication": {"current": "c0rrect-h0rse", "user": "alice"},
  "profile": {"value": "blue", "name": "Alice"}}"#,
        );
        secrets.parameterize(&mut r);
        let text = json_text(&r);
        assert!(
            text.contains(r#""value": "{{credentials_value}}""#),
            "{text}"
        );
        assert!(
            text.contains(r#""current": "{{authentication_current}}""#),
            "{text}"
        );
        for kept in [
            r#""type": "password""#,
            r#""user": "alice""#,
            r#""value": "blue""#,
        ] {
            assert!(text.contains(kept), "{kept} in {text}");
        }
        assert!(
            text.starts_with("{\"credentials\": {"),
            "layout kept: {text}"
        );
        let names: Vec<&str> = secrets.moved.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["credentials_value", "authentication_current"]);
    }

    #[test]
    fn escaped_nested_secrets_are_still_replaced() {
        let mut secrets = HeaderSecrets::default();
        let mut r = json_request(r#"{"token": {"value": "a\/b-escaped"}}"#);
        secrets.parameterize(&mut r);
        let text = json_text(&r);
        assert!(!text.contains("escaped"), "{text}");
        assert!(text.contains("{{token_value}}"), "{text}");
    }

    #[test]
    fn random_json_values_under_neutral_names_are_reported() {
        let random = "q8ZK3vLw0pTn6RxYb2HsJm9CdFgA5eUi7oQ1";
        let r = json_request(&format!(
            r#"{{"nested": {{"blob": "{random}", "password": "{random}"}}, "items": ["{random}"], "note": "hello world"}}"#
        ));
        let warnings = entropy_warnings(&r, "POST /a");
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(
            warnings[0].contains("body field 'nested.blob'"),
            "{warnings:?}"
        );
        assert!(warnings[1].contains("body field 'items'"), "{warnings:?}");
        assert!(warnings.iter().all(|w| !w.contains(random)));
    }

    #[test]
    fn json_keys_in_warnings_cannot_reach_the_terminal_raw() {
        let random = "q8ZK3vLw0pTn6RxYb2HsJm9CdFgA5eUi7oQ1";
        let r = json_request(&format!(
            r#"{{"\u001b]52;c;aGk=\u0007x\u202e": "{random}"}}"#
        ));
        let warnings = entropy_warnings(&r, "POST /a");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            !warnings[0].contains(['\u{1b}', '\u{7}', '\u{202e}']),
            "{warnings:?}"
        );
        assert!(warnings[0].contains(r"\u{1b}]52;c;aGk=\u{7}x\u{202e}"));
    }

    #[test]
    fn json_in_a_text_body_is_judged_as_json() {
        let mut secrets = HeaderSecrets::default();
        let random = "q8ZK3vLw0pTn6RxYb2HsJm9CdFgA5eUi7oQ1";
        let mut r = ImportedRequest {
            body: Some(ImportedBody::Text {
                text: format!(
                    r#"{{"credentials":{{"type":"password","value":"n3sted"}},"blob":"{random}"}}"#
                ),
            }),
            ..ImportedRequest::default()
        };
        secrets.parameterize(&mut r);
        let Some(ImportedBody::Text { text }) = &r.body else {
            panic!("{:?}", r.body);
        };
        assert!(
            text.contains(r#""value":"{{credentials_value}}""#),
            "{text}"
        );
        let warnings = entropy_warnings(&r, "POST /login");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("body field 'blob'"), "{warnings:?}");
    }

    #[test]
    fn placeholders_skip_reserved_names_and_reuse_reserved_secrets() {
        let mut secrets = HeaderSecrets::default();
        secrets.reserve("token_id", Some("path-uuid-value"));
        secrets.reserve("base_url", None);
        // `id` is no value holder: its random-looking value makes it secret.
        let mut r = json_request(
            r#"{"token":{"id":"c7e1a9f2-58b3-4d6e-9a0f-3b2d8e4c7f15","value":"path-uuid-value"}}"#,
        );
        secrets.parameterize(&mut r);
        let text = json_text(&r);
        assert!(text.contains(r#""id":"{{token_id_2}}""#), "{text}");
        assert!(text.contains(r#""value":"{{token_id}}""#), "{text}");
        let names: Vec<&str> = secrets.moved.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["token_id_2"]);
    }

    #[test]
    fn random_path_segments_kept_in_the_url_are_reported_by_position() {
        let sha = "da39a3ee5e6b4b0d3255bfef95601890afd80709";
        let r = ImportedRequest {
            url: format!("{{{{base_url}}}}/commits/{sha}/{{{{x_secret}}}}"),
            ..ImportedRequest::default()
        };
        let warnings = entropy_warnings(&r, "GET /commits/x");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("path segment '2'"), "{warnings:?}");
    }
}
