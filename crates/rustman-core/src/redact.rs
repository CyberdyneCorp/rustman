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
use std::sync::{OnceLock, PoisonError, RwLock};

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

    /// A redactor for everything produced while sending `request`: its
    /// secrets, and the credentials its sensitive headers and auth carry
    /// even when they are literals, so an echoing server cannot leak them.
    pub fn for_request(request: &ResolvedRequest) -> Self {
        let mut redactor = Self::new();
        for secret in &request.secrets {
            redactor.add(secret);
        }
        redactor.add_basic_credential(request);
        for header in &request.headers {
            redactor.add_header_value(&header.name.text, &header.value.text);
        }
        if let Some(auth) = &request.auth {
            redactor.add_auth(auth);
        }
        redactor
    }

    /// Redact every occurrence of the credential a sensitive header value
    /// carries, as `«redacted:<Header-Name>»`: the value after the scheme
    /// (`Bearer`, `Basic`, ...) or each cookie value. Other headers, and
    /// credentials shorter than [`MIN_HEADER_CREDENTIAL`], are ignored.
    pub fn add_header_value(&mut self, header: &str, value: &str) {
        let Some(name) = sensitive_header(header) else {
            return;
        };
        for credential in header_credentials(name, value) {
            if credential.chars().count() >= MIN_HEADER_CREDENTIAL {
                self.add_value(name, credential);
            }
        }
    }

    /// The literal credentials of `auth`, as the `Authorization` header (or
    /// the API key's header) carries them.
    fn add_auth(&mut self, auth: &Auth) {
        match auth {
            Auth::Basic { username, password } => {
                let encoded = STANDARD.encode(format!("{username}:{password}"));
                self.add_header_value("Authorization", &format!("Basic {encoded}"));
            }
            Auth::Bearer { token } => {
                self.add_header_value("Authorization", &format!("Bearer {token}"));
            }
            Auth::ApiKey { name, value, .. } => self.add_header_value(name, value),
            Auth::None | Auth::OAuth2(_) => {}
        }
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

    /// Also redact every value `other` knows, under the same names.
    pub fn merge(&mut self, other: &Redactor) {
        let mut added = false;
        for (needle, placeholder) in other.needles.iter().zip(&other.placeholders) {
            if !self.needles.contains(needle) {
                self.needles.push(needle.clone());
                self.placeholders.push(placeholder.clone());
                added = true;
            }
        }
        for name in &other.names {
            if !self.names.contains(name) {
                self.names.push(name.clone());
            }
        }
        if added {
            self.matcher = AhoCorasick::new(&self.needles).ok();
        }
    }

    /// The names of the secrets this redactor knows.
    pub fn secret_names(&self) -> &[String] {
        &self.names
    }

    /// Whether `text` holds a known secret value, in any form it is matched in.
    pub fn finds_secret(&self, text: &str) -> bool {
        self.occurs_in(text.as_bytes())
    }

    /// Whether `text` holds a known secret value or is a piece of one at
    /// least [`MIN_SECRET_PIECE`] characters long (a capture that matched
    /// part of an echoed secret).
    pub fn reveals_secret(&self, text: &str) -> bool {
        let piece = text.chars().count() >= MIN_SECRET_PIECE
            && self
                .needles
                .iter()
                .any(|n| n.windows(text.len()).any(|w| w == text.as_bytes()));
        piece || self.finds_secret(text)
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

    /// The first `keep` bytes of `input`, with every known secret value
    /// replaced. A secret that starts before `keep` is replaced whole, so
    /// the cut never leaves a part of it: pass an `input` that runs at least
    /// [`Redactor::longest`] bytes past `keep` when one exists.
    pub fn bytes_prefix(&self, input: &[u8], keep: usize) -> Vec<u8> {
        let keep = keep.min(input.len());
        let mut out = Vec::with_capacity(keep);
        let mut copied = 0;
        let runs = merge(self.matches(input, &[]));
        for run in runs.into_iter().take_while(|run| run.range.start < keep) {
            out.extend_from_slice(&input[copied..run.range.start]);
            out.extend_from_slice(run.placeholders.concat().as_bytes());
            copied = run.range.end;
        }
        if copied < keep {
            out.extend_from_slice(&input[copied..keep]);
        }
        out
    }

    /// The length in bytes of the longest form of a known secret value.
    pub fn longest(&self) -> usize {
        self.needles.iter().map(Vec::len).max().unwrap_or(0)
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

/// The shortest piece of a secret value that counts as revealing it; shorter
/// ones are too common to tell.
pub const MIN_SECRET_PIECE: usize = 4;

/// The shortest header credential (cookie value, token) redacted wherever
/// it occurs. Shorter ones (`theme=1`) would mangle unrelated text and are
/// only redacted in the header itself.
pub const MIN_HEADER_CREDENTIAL: usize = 6;

/// The credentials in the value of the sensitive header `name`: each cookie
/// value for `Cookie`, the cookie value for `Set-Cookie`, otherwise the
/// value after its scheme, or the whole value when it has none.
fn header_credentials<'v>(name: &str, value: &'v str) -> Vec<&'v str> {
    let cookie_value = |pair: &'v str| pair.split_once('=').map(|(_, v)| unquote(v.trim()));
    match name {
        "Cookie" => value.split(';').filter_map(cookie_value).collect(),
        "Set-Cookie" => value
            .split(';')
            .next()
            .and_then(cookie_value)
            .into_iter()
            .collect(),
        _ => {
            let value = value.trim();
            vec![value.split_once(' ').map_or(value, |(_, c)| c.trim())]
        }
    }
}

fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value)
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

/// Every secret value this process has resolved, for sinks that are not
/// tied to one request: logs (`-v`, `RUST_LOG`) go through
/// [`redact_logs`] as a safety net over events that are already redacted.
fn log_secrets() -> &'static RwLock<Redactor> {
    static SECRETS: OnceLock<RwLock<Redactor>> = OnceLock::new();
    SECRETS.get_or_init(RwLock::default)
}

/// Make [`redact_logs`] redact every value `redactor` knows. Call it as soon
/// as secrets are resolved, before anything about the request is logged.
pub fn remember_for_logs(redactor: &Redactor) {
    if redactor.matcher.is_none() {
        return;
    }
    log_secrets()
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .merge(redactor);
}

/// `line` with every secret value remembered by [`remember_for_logs`]
/// replaced.
pub fn redact_logs(line: &str) -> String {
    log_secrets()
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .text(line)
        .into_owned()
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

    fn request_with(headers: &[(&str, &str)], auth: Option<Auth>) -> ResolvedRequest {
        ResolvedRequest {
            method: "GET".into(),
            url: Resolved::literal("http://h/"),
            query: Vec::new(),
            headers: headers
                .iter()
                .map(|(n, v)| crate::vars::ResolvedPair {
                    name: Resolved::literal(*n),
                    value: Resolved::literal(*v),
                })
                .collect(),
            body: crate::vars::ResolvedBody::None,
            auth,
            settings: crate::model::RequestSettings::default(),
            tls: crate::vars::TlsFiles::default(),
            secrets: Vec::new(),
        }
    }

    #[test]
    fn echoed_literal_sensitive_headers_are_redacted() {
        let request = request_with(
            &[
                ("authorization", "Bearer LITERALabc123"),
                ("Cookie", "sid=\"CANARYs3cr3tXYZ\"; theme=1"),
                ("X-Trace", "trace-value-1"),
            ],
            None,
        );
        let r = Redactor::for_request(&request);
        assert_eq!(
            r.text(r#"{"auth":"Bearer LITERALabc123","cookie":"sid=CANARYs3cr3tXYZ; theme=1"}"#),
            r#"{"auth":"Bearer «redacted:Authorization»","cookie":"sid=«redacted:Cookie»; theme=1"}"#
        );
        assert_eq!(r.text("trace-value-1"), "trace-value-1");
    }

    #[test]
    fn literal_auth_credentials_are_redacted_when_echoed() {
        let bearer = request_with(
            &[],
            Some(Auth::Bearer {
                token: "tok-LITERAL-9".into(),
            }),
        );
        assert_eq!(
            Redactor::for_request(&bearer).text("got tok-LITERAL-9"),
            "got «redacted:Authorization»"
        );
        let basic = Auth::Basic {
            username: "user".into(),
            password: "pass".into(),
        };
        let encoded = STANDARD.encode("user:pass");
        assert_eq!(
            Redactor::for_request(&request_with(&[], Some(basic))).text(&encoded),
            "«redacted:Authorization»"
        );
    }

    #[test]
    fn header_credentials_skip_short_and_other_values() {
        let mut r = Redactor::new();
        r.add_header_value("Set-Cookie", "sid=abcdef123; Path=/; HttpOnly");
        r.add_header_value("Cookie", "a=1");
        r.add_header_value("Accept", "application/json");
        assert_eq!(
            r.text("abcdef123 a=1 application/json Path=/"),
            "«redacted:Set-Cookie» a=1 application/json Path=/"
        );
    }

    #[test]
    fn a_prefix_never_ends_inside_a_secret() {
        let r = Redactor::new().with(&Secret::new("tok", "simpleCANARY".to_owned()));
        let input = format!("{}simpleCANARY tail", "A".repeat(10));
        for keep in 0..=input.len() {
            let out = into_string(r.bytes_prefix(input.as_bytes(), keep));
            assert!(!out.contains("simple"), "{keep}: {out}");
            if keep > 10 {
                assert!(
                    out.ends_with("«redacted:tok»") || out.contains("» "),
                    "{keep}: {out}"
                );
            }
        }
        assert_eq!(r.bytes_prefix(b"AAsimpleCANARY", 1), b"A");
        assert_eq!(r.longest(), "simpleCANARY".len());
    }

    #[test]
    fn pieces_of_a_secret_reveal_it() {
        let r = Redactor::new().with(&Secret::new("tok", "simpleCANARY".to_owned()));
        assert!(r.reveals_secret("simpleCAN"));
        assert!(r.reveals_secret("x simpleCANARY y"));
        assert!(r.reveals_secret("NARY"));
        assert!(!r.reveals_secret("ARY"));
        assert!(!r.reveals_secret("other"));
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

    #[test]
    fn merged_redactors_redact_both_sets_of_values() {
        let mut a = Redactor::new().with(&Secret::new("one", "first-value".to_owned()));
        let b = Redactor::new().with(&Secret::new("two", "second-value".to_owned()));
        a.merge(&b);
        assert_eq!(
            a.text("first-value second-value"),
            "«redacted:one» «redacted:two»"
        );
        assert_eq!(a.secret_names(), ["one", "two"]);
    }

    #[test]
    fn remembered_secrets_are_redacted_from_logs() {
        let redactor = Redactor::new().with(&Secret::new("log_canary", "c4n4ry-l0g".to_owned()));
        assert_eq!(redact_logs("token=c4n4ry-l0g"), "token=c4n4ry-l0g");
        remember_for_logs(&redactor);
        assert_eq!(
            redact_logs("token=c4n4ry-l0g"),
            "token=«redacted:log_canary»"
        );
        remember_for_logs(&Redactor::new());
        assert_eq!(redact_logs("plain"), "plain");
    }
}
