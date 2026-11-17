//! The `Redactor`: the one way an output sink turns request and response data
//! into text (design D7, environments spec "Secret redaction invariant").
//!
//! It replaces, with `«redacted:<name>»`:
//! - the byte ranges a [`Resolved`] records as coming from a secret;
//! - the values of the `Authorization`, `Proxy-Authorization`, `Cookie` and
//!   `Set-Cookie` headers;
//! - every occurrence of a known secret value, in any text or bytes. This
//!   second net catches servers that echo a secret back, and secrets that
//!   reached a structure without provenance (a structured JSON body, auth).
//!
//! Secret values are also matched in their JSON-escaped (also with `\uXXXX`
//! escapes) and percent-encoded (upper- or lowercase hex) forms, the two
//! ways an echoing server most often re-encodes them, and in their Rust
//! `Debug`-escaped form, which `Debug` output of any value holding a secret
//! (a JSON body, auth fields) uses.

use std::borrow::Cow;
use std::fmt;
use std::ops::Range;

use aho_corasick::AhoCorasick;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use crate::model::Auth;
use crate::secret::{Secret, placeholder};
use crate::vars::{Resolved, ResolvedRequest, SecretSpan};

/// Headers whose values are always redacted, in their canonical spelling.
pub const SENSITIVE_HEADERS: [&str; 4] = [
    "Authorization",
    "Proxy-Authorization",
    "Cookie",
    "Set-Cookie",
];

/// The canonical name of `header` when its value is always redacted.
pub fn sensitive_header(header: &str) -> Option<&'static str> {
    SENSITIVE_HEADERS
        .into_iter()
        .find(|h| h.eq_ignore_ascii_case(header.trim()))
}

/// Redacts secrets and sensitive headers from everything a sink writes.
#[derive(Clone, Default)]
pub struct Redactor {
    /// Every searchable form of every secret value, without duplicates.
    needles: Vec<Vec<u8>>,
    /// The placeholder of each needle, by index.
    placeholders: Vec<String>,
    /// A matcher over `needles`, rebuilt when one is added.
    matcher: Option<AhoCorasick>,
    names: Vec<String>,
}

impl fmt::Debug for Redactor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Redactor")
            .field("secrets", &self.names)
            .finish()
    }
}

impl Redactor {
    /// A redactor that knows no secret values; it still redacts sensitive
    /// headers and secret spans.
    pub fn new() -> Self {
        Self::default()
    }

    /// A redactor for everything produced while sending `request`.
    pub fn for_request(request: &ResolvedRequest) -> Self {
        let mut redactor = Self::new();
        for secret in &request.secrets {
            redactor.add(secret);
        }
        redactor.add_basic_credential(request);
        redactor
    }

    /// The Basic auth header carries `user:password` base64-encoded, a form
    /// no secret value matches. When a secret went into it, redact the
    /// encoded credential under that secret's name, so an echoing server
    /// cannot leak it.
    fn add_basic_credential(&mut self, request: &ResolvedRequest) {
        let Some(Auth::Basic { username, password }) = &request.auth else {
            return;
        };
        let credential = format!("{username}:{password}");
        let Some(secret) = request
            .secrets
            .iter()
            .find(|s| !s.expose().is_empty() && credential.contains(s.expose()))
        else {
            return;
        };
        let encoded = STANDARD.encode(&credential);
        self.add_value(secret.name(), &encoded);
    }

    /// Builder form of [`Redactor::add`].
    pub fn with(mut self, secret: &Secret<String>) -> Self {
        self.add(secret);
        self
    }

    /// Redact every occurrence of `secret`'s value. Empty values are ignored.
    pub fn add(&mut self, secret: &Secret<String>) {
        self.add_value(secret.name(), secret.expose());
    }

    /// Redact every occurrence of `value` as `«redacted:<name>»`. Used for
    /// values derived from a secret, such as an encoded credential.
    pub fn add_value(&mut self, name: &str, value: &str) {
        if value.is_empty() {
            return;
        }
        if !self.names.iter().any(|n| n == name) {
            self.names.push(name.to_owned());
        }
        for form in encoded_forms(value) {
            if !self.needles.iter().any(|n| *n == form.as_bytes()) {
                self.needles.push(form.into_bytes());
                self.placeholders.push(placeholder(name));
            }
        }
        self.matcher = AhoCorasick::new(&self.needles).ok();
    }

    /// The names of the secrets this redactor knows.
    pub fn secret_names(&self) -> &[String] {
        &self.names
    }

    /// `input` with every known secret value replaced.
    pub fn text<'a>(&self, input: &'a str) -> Cow<'a, str> {
        match self.bytes(input.as_bytes()) {
            Cow::Borrowed(_) => Cow::Borrowed(input),
            Cow::Owned(bytes) => Cow::Owned(into_string(bytes)),
        }
    }

    /// `input` with every known secret value replaced. Works on any bytes,
    /// such as a binary response body.
    pub fn bytes<'a>(&self, input: &'a [u8]) -> Cow<'a, [u8]> {
        if !self.occurs_in(input) {
            return Cow::Borrowed(input);
        }
        Cow::Owned(self.scan(input, &[]))
    }

    /// Resolved text with its secret spans and every known secret replaced.
    pub fn resolved(&self, resolved: &Resolved) -> String {
        if resolved.secret_spans.is_empty() {
            return self.text(&resolved.text).into_owned();
        }
        into_string(self.scan(resolved.text.as_bytes(), &resolved.secret_spans))
    }

    /// A header value as it may be shown: sensitive headers are replaced
    /// whole by `«redacted:<Header-Name>»`.
    pub fn header_value(&self, name: &str, value: &str) -> String {
        match sensitive_header(name) {
            Some(canonical) => placeholder(canonical),
            None => self.text(value).into_owned(),
        }
    }

    /// A resolved header value as it may be shown.
    pub fn resolved_header_value(&self, name: &str, value: &Resolved) -> String {
        match sensitive_header(name) {
            Some(canonical) => placeholder(canonical),
            None => self.resolved(value),
        }
    }

    fn occurs_in(&self, input: &[u8]) -> bool {
        self.matcher.as_ref().is_some_and(|m| m.is_match(input))
    }

    /// Replace every byte covered by a span or by any occurrence of a known
    /// value, overlapping ones included. Overlapping matches merge into one
    /// run, shown as the placeholders of the secrets that make it up.
    fn scan(&self, input: &[u8], spans: &[SecretSpan]) -> Vec<u8> {
        let mut out = Vec::with_capacity(input.len());
        let mut copied = 0;
        for run in merge(self.matches(input, spans)) {
            out.extend_from_slice(&input[copied..run.range.start]);
            out.extend_from_slice(run.placeholders.concat().as_bytes());
            copied = run.range.end;
        }
        out.extend_from_slice(&input[copied..]);
        out
    }

    /// Every span and every (possibly overlapping) value occurrence, sorted
    /// by start, longest first.
    fn matches(&self, input: &[u8], spans: &[SecretSpan]) -> Vec<Match> {
        let mut found: Vec<Match> = spans
            .iter()
            .map(|s| Match {
                range: s.range.start.min(input.len())..s.range.end.min(input.len()),
                placeholder: placeholder(&s.name),
            })
            .filter(|m| !m.range.is_empty())
            .collect();
        if let Some(matcher) = &self.matcher {
            found.extend(matcher.find_overlapping_iter(input).map(|m| Match {
                range: m.range(),
                placeholder: self.placeholders[m.pattern().as_usize()].clone(),
            }));
        }
        found.sort_by_key(|m| (m.range.start, std::cmp::Reverse(m.range.end)));
        found
    }
}

/// One secret occurrence in the input.
struct Match {
    range: Range<usize>,
    placeholder: String,
}

/// A run of overlapping matches, replaced as a whole.
struct Run {
    range: Range<usize>,
    placeholders: Vec<String>,
}

/// Merge sorted matches into disjoint runs. A match inside a run's current
/// extent adds nothing; one reaching past it extends the run and adds its
/// placeholder.
fn merge(matches: Vec<Match>) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    for m in matches {
        match runs.last_mut() {
            Some(run) if m.range.start < run.range.end => {
                if m.range.end > run.range.end {
                    run.range.end = m.range.end;
                    if !run.placeholders.contains(&m.placeholder) {
                        run.placeholders.push(m.placeholder);
                    }
                }
            }
            _ => runs.push(Run {
                range: m.range,
                placeholders: vec![m.placeholder],
            }),
        }
    }
    runs
}

/// Replacing whole UTF-8 sequences in valid UTF-8 keeps it valid: a match
/// always starts on a character boundary because a valid sequence never
/// starts with a continuation byte.
fn into_string(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// The raw value plus its JSON-escaped, `Debug`-escaped, percent-encoded and
/// form-encoded forms, without duplicates. Percent-encoding comes in upper-
/// and lowercase hex, and the JSON form also with every non-ASCII character
/// as `\uXXXX` (UTF-16, so a surrogate pair beyond the BMP), in both cases.
fn encoded_forms(value: &str) -> Vec<String> {
    let json = serde_json::to_string(value).unwrap_or_default();
    let json = json[1..json.len() - 1].to_owned();
    let debug = format!("{value:?}");
    let debug = debug[1..debug.len() - 1].to_owned();
    let mut forms = vec![value.to_owned()];
    for upper in [true, false] {
        for form in [
            ascii_json(&json, upper),
            percent_encode(value, "%20", upper),
            percent_encode(value, "+", upper),
        ] {
            if !forms.contains(&form) {
                forms.push(form);
            }
        }
    }
    for form in [json, debug] {
        if !forms.contains(&form) {
            forms.push(form);
        }
    }
    forms
}

/// `json` with every non-ASCII character written as `\uXXXX` escapes.
fn ascii_json(json: &str, upper: bool) -> String {
    let mut out = String::with_capacity(json.len());
    let mut units = [0; 2];
    for ch in json.chars() {
        if ch.is_ascii() {
            out.push(ch);
            continue;
        }
        for unit in ch.encode_utf16(&mut units) {
            out.push_str(&if upper {
                format!("\\u{unit:04X}")
            } else {
                format!("\\u{unit:04x}")
            });
        }
    }
    out
}

/// RFC 3986 percent-encoding of everything but unreserved characters, in
/// upper- or lowercase hex; a space becomes `space`.
fn percent_encode(value: &str, space: &str, upper: bool) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(char::from(byte));
            }
            b' ' => out.push_str(space),
            _ if upper => out.push_str(&format!("%{byte:02X}")),
            _ => out.push_str(&format!("%{byte:02x}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    fn token() -> Secret<String> {
        Secret::new("api_token", "s3cr3t".to_owned())
    }

    #[test]
    fn known_values_are_replaced_everywhere() {
        let r = Redactor::new().with(&token());
        assert_eq!(
            r.text(r#"{"headers":{"x-token":"s3cr3t"},"again":"s3cr3ts3cr3t"}"#),
            r#"{"headers":{"x-token":"«redacted:api_token»"},"again":"«redacted:api_token»«redacted:api_token»"}"#
        );
    }

    #[test]
    fn untouched_input_is_borrowed() {
        let r = Redactor::new().with(&token());
        assert!(matches!(r.text("nothing here"), Cow::Borrowed(_)));
        assert!(matches!(Redactor::new().bytes(b"x"), Cow::Borrowed(_)));
    }

    #[test]
    fn binary_bodies_are_scanned() {
        let r = Redactor::new().with(&token());
        let body = [&[0xff, 0x00][..], b"s3cr3t", &[0xfe]].concat();
        let out = r.bytes(&body);
        assert!(!contains(&out, b"s3cr3t"));
        assert!(contains(&out, "«redacted:api_token»".as_bytes()));
    }

    #[test]
    fn longest_secret_wins() {
        let r = Redactor::new()
            .with(&Secret::new("short", "abc".to_owned()))
            .with(&Secret::new("long", "abcdef".to_owned()));
        assert_eq!(r.text("abcdef abc"), "«redacted:long» «redacted:short»");
    }

    #[test]
    fn a_value_running_into_a_span_cannot_expose_the_span() {
        let r = Redactor::new().with(&Secret::new("a", "bX".to_owned()));
        let resolved = Resolved {
            text: "abXYZW!".into(),
            secret_spans: vec![SecretSpan {
                name: "tok".into(),
                range: 2..6,
            }],
        };
        assert_eq!(r.resolved(&resolved), "a«redacted:a»«redacted:tok»!");
    }

    #[test]
    fn overlapping_values_are_both_covered() {
        let r = Redactor::new()
            .with(&Secret::new("x", "abc".to_owned()))
            .with(&Secret::new("y", "cde".to_owned()));
        assert_eq!(r.text("_abcde_"), "_«redacted:x»«redacted:y»_");
        assert_eq!(r.text("abcabc"), "«redacted:x»«redacted:x»");
    }

    #[test]
    fn empty_values_are_ignored() {
        let r = Redactor::new().with(&Secret::new("empty", String::new()));
        assert_eq!(r.text("text"), "text");
        assert!(r.secret_names().is_empty());
    }

    #[test]
    fn encoded_echoes_are_replaced() {
        let r = Redactor::new().with(&Secret::new("pw", "a b\"/&é".to_owned()));
        assert_eq!(r.text(r#"{"pw":"a b\"/&é"}"#), r#"{"pw":"«redacted:pw»"}"#);
        assert_eq!(r.text("q=a%20b%22%2F%26%C3%A9"), "q=«redacted:pw»");
        assert_eq!(r.text("q=a+b%22%2F%26%C3%A9"), "q=«redacted:pw»");
    }

    #[test]
    fn lowercase_percent_encoding_is_replaced() {
        let r = Redactor::new().with(&Secret::new("pw", "a b/é😀".to_owned()));
        assert_eq!(r.text("q=a%20b%2f%c3%a9%f0%9f%98%80"), "q=«redacted:pw»");
        assert_eq!(r.text("q=a+b%2f%c3%a9%f0%9f%98%80&x"), "q=«redacted:pw»&x");
        assert_eq!(r.text("q=a+b%2F%C3%A9%F0%9F%98%80"), "q=«redacted:pw»");
    }

    #[test]
    fn json_unicode_escapes_are_replaced() {
        let r = Redactor::new().with(&Secret::new("pw", "é\"😀x".to_owned()));
        assert_eq!(
            r.text(r#"{"pw":"\u00e9\"\ud83d\ude00x"}"#),
            r#"{"pw":"«redacted:pw»"}"#
        );
        assert_eq!(
            r.text(r#"{"pw":"\u00E9\"\uD83D\uDE00x"}"#),
            r#"{"pw":"«redacted:pw»"}"#
        );
        assert_eq!(r.text(r#"{"pw":"é\"😀x"}"#), r#"{"pw":"«redacted:pw»"}"#);
    }

    #[test]
    fn secret_spans_are_replaced_even_for_unknown_values() {
        let resolved = Resolved {
            text: "Bearer xyz!".into(),
            secret_spans: vec![SecretSpan {
                name: "tok".into(),
                range: 7..10,
            }],
        };
        assert_eq!(
            Redactor::new().resolved(&resolved),
            "Bearer «redacted:tok»!"
        );
    }

    #[test]
    fn sensitive_headers_are_redacted_by_name() {
        let r = Redactor::new();
        assert_eq!(
            r.header_value("Authorization", "Bearer abc123"),
            "«redacted:Authorization»"
        );
        assert_eq!(
            r.header_value("set-cookie", "sid=1"),
            "«redacted:Set-Cookie»"
        );
        assert_eq!(
            r.header_value("PROXY-AUTHORIZATION", "Basic x"),
            "«redacted:Proxy-Authorization»"
        );
        assert_eq!(r.header_value("Cookie", "a=b"), "«redacted:Cookie»");
        assert_eq!(r.header_value("Accept", "text/plain"), "text/plain");
        assert_eq!(
            Redactor::new()
                .with(&token())
                .header_value("X-Api-Key", "s3cr3t"),
            "«redacted:api_token»"
        );
    }

    #[test]
    fn debug_lists_names_only() {
        let r = Redactor::new().with(&token());
        let debug = format!("{r:?}");
        assert!(
            debug.contains("api_token") && !debug.contains("s3cr3t"),
            "{debug}"
        );
    }
}
