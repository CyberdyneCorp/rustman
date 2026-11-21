//! Secrets in saved captures (spec: Secrets become placeholders, design D5
//! step 5). Credential headers are parameterized here with the names the
//! spec gives (`Authorization: Bearer {{auth_token}}`, `Cookie:
//! {{cookie}}`); every other secret (`X-Api-Key`-style headers, query and
//! body fields named like `token`, `password`, `secret`, `api_key`, known
//! token formats) is left to the importer's extraction, which runs on the
//! same collection right after. Values that only look random are reported,
//! never moved.

use std::collections::HashMap;

use rustman_import::ir::{ImportedRequest, Pair};

use crate::named::{is_secret_field, is_secret_header};

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
}

impl HeaderSecrets {
    /// Replace the credentials of `request`'s `Authorization` and `Cookie`
    /// headers by placeholders. (`Proxy-*` headers are never saved.)
    pub fn parameterize(&mut self, request: &mut ImportedRequest) {
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
        let taken = |n: &str| self.moved.iter().any(|(m, _)| m == n);
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

/// Warnings for header and query values of `request` that look random but
/// that the secret extraction keeps: neither their names nor their values
/// (a well-known token, an auth scheme) mark them as secrets. Values are
/// never shown.
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
    headers
        .chain(query)
        .map(|(what, pair): (&str, &Pair)| {
            format!(
                "warning: {what} '{}' of {shown} looks like a secret (high entropy) and was kept; \
                 review it before committing",
                pair.name
            )
        })
        .collect()
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
}
