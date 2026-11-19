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
//! Secret values are also matched in the forms common JSON, URL and HTML
//! encoders write them in, the ways an echoing server most often re-encodes
//! them, and in their Rust `Debug`-escaped form, which `Debug` output of any
//! value holding a secret (a JSON body, auth fields) uses (see [`forms`]).

mod decode;
mod forms;
mod layer;
mod runs;

use std::borrow::Cow;
use std::fmt;
use std::io::{self, Read, Write};
use std::sync::{Arc, OnceLock, PoisonError, RwLock, Weak};

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};

use crate::model::{Auth, AwsCredentials};
use crate::secret::{Secret, placeholder};
use crate::vars::{Resolved, ResolvedRequest, SecretSpan};

use layer::Layer;
use runs::{Hit, Run};

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
///
/// It is a short list of shared [`Layer`]s: cloning it copies no values,
/// a clone that adds values adds a layer of its own, and every layer builds
/// its matcher once, on first use. Values added after a layer's matcher is
/// built go to a new layer, and adjacent layers of similar size are merged
/// (see [`Redactor::settle`]), so adding values one at a time between uses
/// rebuilds O(log n) times per value, not once per value over all of them.
#[derive(Clone, Default)]
pub struct Redactor {
    layers: Vec<Arc<Layer>>,
}

impl fmt::Debug for Redactor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Redactor")
            .field("secrets", &self.secret_names())
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
        redactor.add_request(request);
        redactor
    }

    /// Also redact what [`Redactor::for_request`] would for `request`. On a
    /// clone of a larger redactor (the run's), only what it lacks is added.
    pub fn add_request(&mut self, request: &ResolvedRequest) {
        for secret in &request.secrets {
            self.add(secret);
        }
        self.add_basic_credential(request);
        for header in &request.headers {
            self.add_header_value(&header.name.text, &header.value.text);
        }
        if let Some(auth) = &request.auth {
            self.add_auth(auth);
        }
    }

    /// Redact every occurrence of the credential a sensitive header value
    /// carries, as `«redacted:<Header-Name>»`: the value after the scheme
    /// (`Bearer`, `Basic`, ...) or each cookie value. Other headers, and
    /// credentials shorter than [`MIN_HEADER_CREDENTIAL`], and credentials
    /// that are already a placeholder, are ignored.
    pub fn add_header_value(&mut self, header: &str, value: &str) {
        let Some(name) = sensitive_header(header) else {
            return;
        };
        for credential in header_credentials(name, value) {
            if is_credential(credential) {
                self.add_value(name, credential);
            }
        }
    }

    /// The literal credentials of `auth`, as the `Authorization` header (or
    /// the API key's header) carries them.
    pub fn add_auth(&mut self, auth: &Auth) {
        match auth {
            Auth::Basic { username, password } => {
                let encoded = STANDARD.encode(format!("{username}:{password}"));
                self.add_header_value("Authorization", &format!("Basic {encoded}"));
            }
            Auth::Bearer { token } => {
                self.add_header_value("Authorization", &format!("Bearer {token}"));
            }
            Auth::ApiKey { name, value, .. } => self.add_header_value(name, value),
            Auth::Digest { password, .. } => self.add_credential("password", password),
            Auth::AwsSigV4(sigv4) => {
                if let AwsCredentials::Fields {
                    secret_key,
                    session_token,
                    ..
                } = &sigv4.credentials
                {
                    self.add_credential("aws_secret_key", secret_key);
                    let token = session_token.as_deref().unwrap_or_default();
                    self.add_credential("aws_session_token", token);
                }
            }
            Auth::Jwt(jwt) => {
                self.add_credential("jwt_secret", jwt.secret.as_deref().unwrap_or_default());
                let key = jwt.private_key.as_deref().unwrap_or_default();
                self.add_credential("jwt_private_key", key);
            }
            Auth::None | Auth::OAuth2(_) => {}
        }
    }

    /// Redact a literal credential of an auth type, unless it is shorter
    /// than [`MIN_HEADER_CREDENTIAL`] (a secret variable is redacted
    /// whatever its length).
    fn add_credential(&mut self, name: &str, value: &str) {
        if is_credential(value) {
            self.add_value(name, value);
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

    /// Redact every occurrence of `secret`'s value, and of the forms a
    /// server echoing it inside a token or an error message may write
    /// ([`echo_forms`]). Empty values are ignored.
    pub fn add(&mut self, secret: &Secret<String>) {
        let value = secret.expose();
        self.add_value(secret.name(), value);
        if value.is_empty() {
            return;
        }
        for form in echo_forms(value) {
            self.add_value(secret.name(), &form);
        }
    }

    /// Redact every occurrence of `value` as `«redacted:<name>»`. Used for
    /// values derived from a secret, such as an encoded credential.
    pub fn add_value(&mut self, name: &str, value: &str) {
        if value.is_empty() || self.layers.iter().any(|l| l.has_value(value)) {
            return;
        }
        self.own_layer().add(name, value);
        self.settle();
    }

    /// The last layer, when no clone shares it and its matcher is not
    /// built yet; otherwise a new one.
    fn own_layer(&mut self) -> &mut Layer {
        let open = self
            .layers
            .last_mut()
            .and_then(Arc::get_mut)
            .is_some_and(|l| !l.is_built());
        if !open {
            self.layers.push(Arc::default());
        }
        let last = self.layers.last_mut().and_then(Arc::get_mut);
        last.expect("the last layer was just made unshared")
    }

    /// Merge the last two layers while the last is at least half the size
    /// of the one before: sizes then shrink geometrically, so there are
    /// O(log n) layers and each value is merged O(log n) times.
    fn settle(&mut self) {
        while let [.., before, last] = self.layers.as_slice() {
            if before.size() > 2 * last.size() {
                return;
            }
            let at = self.layers.len() - 2;
            let merged = Layer::compacted(&self.layers[at..]);
            self.layers.truncate(at);
            self.layers.push(Arc::new(merged));
        }
    }

    /// Also redact every value `other` knows, under the same names. Shares
    /// `other`'s layers instead of copying them.
    pub fn merge(&mut self, other: &Redactor) {
        for layer in &other.layers {
            if !self.layers.iter().any(|l| Arc::ptr_eq(l, layer)) {
                self.layers.push(layer.clone());
            }
        }
        self.settle();
    }

    /// The names of the secrets this redactor knows.
    pub fn secret_names(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for name in self.layers.iter().flat_map(|l| l.names()) {
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
        names
    }

    /// Whether this redactor knows no secret value.
    pub fn is_empty(&self) -> bool {
        self.layers.iter().all(|l| l.is_empty())
    }

    /// Whether `text` holds a known secret value, in any form it is matched in.
    pub fn finds_secret(&self, text: &str) -> bool {
        self.occurs_in(text.as_bytes())
    }

    /// Whether `text` holds a known secret value in any form, or any piece
    /// of one at least [`MIN_SECRET_PIECE`] characters long (a capture that
    /// matched part of an echoed secret, straddled one or kept the end of a
    /// masked one), as is or once percent-, JSON- or HTML-decoded.
    pub fn reveals_secret(&self, text: &str) -> bool {
        let has_piece = |t: &str| self.layers.iter().any(|l| l.has_piece_in(t));
        has_piece(text)
            || self.finds_secret(text)
            || decode::variants(text).iter().any(|t| has_piece(t))
    }

    /// Redact every string, key and number of `value` that holds a known
    /// secret value; a number that held one becomes a string.
    pub fn json(&self, value: &mut serde_json::Value) {
        use serde_json::Value;
        match value {
            Value::String(s) => {
                if let Cow::Owned(clean) = self.text(s) {
                    *s = clean;
                }
            }
            Value::Number(n) => {
                let text = n.to_string();
                if self.finds_secret(&text) {
                    *value = Value::String(self.text(&text).into_owned());
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|v| self.json(v)),
            Value::Object(map) => {
                for (key, mut v) in std::mem::take(map) {
                    self.json(&mut v);
                    map.insert(self.text(&key).into_owned(), v);
                }
            }
            Value::Null | Value::Bool(_) => {}
        }
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
        let runs = self.runs(input, &[]);
        for run in runs.iter().take_while(|run| run.range.start < keep) {
            out.extend_from_slice(&input[copied..run.range.start]);
            out.extend_from_slice(run.replacement().as_bytes());
            copied = run.range.end;
        }
        if copied < keep {
            out.extend_from_slice(&input[copied..keep]);
        }
        out
    }

    /// Copy `reader` to `writer` with every known secret value replaced,
    /// in chunks, so a secret split across two chunks is still replaced
    /// whole (see [`Stream`]). The output is what [`Redactor::bytes`] gives
    /// for the whole input.
    pub fn copy(&self, mut reader: impl Read, writer: &mut dyn Write) -> io::Result<()> {
        if self.is_empty() {
            return io::copy(&mut reader, writer).map(|_| ());
        }
        let mut stream = Stream::default();
        let mut chunk = vec![0; COPY_CHUNK];
        loop {
            let read = match reader.read(&mut chunk) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                result => result?,
            };
            stream.pending.extend_from_slice(&chunk[..read]);
            if read > 0 && stream.pending.len() < stream.wait_for {
                continue;
            }
            writer.write_all(&stream.flush(self, read == 0))?;
            if read == 0 {
                return Ok(());
            }
        }
    }

    /// The length in bytes of the longest form of a known secret value.
    pub fn longest(&self) -> usize {
        self.layers.iter().map(|l| l.longest()).max().unwrap_or(0)
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
        self.layers
            .iter()
            .any(|l| l.matcher().is_some_and(|m| m.is_match(input)))
    }

    /// Replace every byte covered by a span or by any occurrence of a known
    /// value, overlapping ones included. Overlapping occurrences merge into
    /// one run, shown as the placeholders of the secrets that make it up.
    fn scan(&self, input: &[u8], spans: &[SecretSpan]) -> Vec<u8> {
        let mut out = Vec::with_capacity(input.len());
        let mut copied = 0;
        for run in self.runs(input, spans) {
            out.extend_from_slice(&input[copied..run.range.start]);
            out.extend_from_slice(run.replacement().as_bytes());
            copied = run.range.end;
        }
        out.extend_from_slice(&input[copied..]);
        out
    }

    /// The runs of `input`: every span and every (possibly overlapping)
    /// value occurrence, folded as they are found (see [`runs`]).
    fn runs(&self, input: &[u8], spans: &[SecretSpan]) -> Vec<Run> {
        let mut spans: Vec<Hit> = spans
            .iter()
            .map(|s| Hit {
                range: s.range.start.min(input.len())..s.range.end.min(input.len()),
                placeholder: Arc::from(placeholder(&s.name)),
            })
            .collect();
        spans.sort_by_key(|h| h.range.end);
        let mut sources: Vec<Box<dyn Iterator<Item = Hit> + '_>> =
            vec![Box::new(spans.into_iter())];
        for layer in &self.layers {
            if let Some(matcher) = layer.matcher() {
                sources.push(Box::new(matcher.find_overlapping_iter(input).map(|m| {
                    Hit {
                        range: m.range(),
                        placeholder: layer.placeholder(m.pattern().as_usize()).clone(),
                    }
                })));
            }
        }
        runs::by_end(sources).into_vec()
    }
}

/// The state of [`Redactor::copy`] between chunks: bytes not written yet,
/// and how much of their start a placeholder already written covers.
#[derive(Default)]
struct Stream {
    pending: Vec<u8>,
    /// The first `covered` bytes of `pending` are part of a run whose
    /// placeholders (`shown`) are written; an occurrence starting there
    /// continues that run instead of starting a new one.
    covered: usize,
    shown: Vec<Arc<str>>,
    /// While a run is held back, how many pending bytes to read before
    /// scanning again: doubling each time, so a run held up to
    /// [`HOLD_LIMIT`] is scanned O(log) times, not once per read.
    wait_for: usize,
}

impl Stream {
    /// Write what no later chunk can change. The last `longest - 1` bytes
    /// are held back: a match starting there may be incomplete. A run
    /// starting at or after `keep` (a longer secret around it may still
    /// complete) or reaching past it (a later occurrence may extend it) is
    /// held back whole, with everything after it, so the output is exactly
    /// what [`Redactor::bytes`] gives for the whole input. Only a run held
    /// back past [`HOLD_LIMIT`] bytes (a periodic body matching everywhere)
    /// is written early: its placeholders now, its last `longest - 1` bytes
    /// kept so an overlapping occurrence continues it without a new
    /// placeholder.
    fn flush(&mut self, redactor: &Redactor, at_end: bool) -> Vec<u8> {
        let tail = redactor.longest().saturating_sub(1);
        let len = self.pending.len();
        let keep = if at_end {
            len
        } else {
            len.saturating_sub(tail)
        };
        let mut out = Vec::with_capacity(keep);
        let mut copied = self.covered;
        let mut stop = None;
        self.wait_for = 0;
        for run in redactor.runs(&self.pending, &[]) {
            if run.range.end <= copied {
                continue;
            }
            let continues = run.range.start < self.covered;
            let open = run.range.end > keep;
            if !continues && (run.range.start >= keep || (open && len < HOLD_LIMIT)) {
                if open && run.range.start == 0 {
                    self.wait_for = (len * 2).min(HOLD_LIMIT);
                }
                stop = Some(Stop::Before(run.range.start));
                break;
            }
            if !continues {
                out.extend_from_slice(&self.pending[copied..run.range.start]);
                self.shown.clear();
            }
            self.show_new(&mut out, &run.placeholders);
            copied = run.range.end;
            if open {
                let at = run.range.end.saturating_sub(tail).max(run.range.start);
                stop = Some(Stop::Within(at));
                break;
            }
        }
        if copied > keep && !matches!(stop, Some(Stop::Within(_))) {
            // A run written early still reaches past `keep` (small reads):
            // keep its tail covered so an occurrence can still continue it.
            stop = Some(Stop::Within(copied.saturating_sub(tail)));
        }
        let next = match stop {
            Some(Stop::Within(at)) => {
                self.covered = copied - at;
                at
            }
            Some(Stop::Before(at)) => self.write_raw(&mut out, copied, at.min(keep)),
            None => self.write_raw(&mut out, copied, keep.max(copied)),
        };
        self.pending.drain(..next);
        out
    }

    /// Write `pending[from..to]` as is; nothing written is covered any more.
    fn write_raw(&mut self, out: &mut Vec<u8>, from: usize, to: usize) -> usize {
        out.extend_from_slice(&self.pending[from..to]);
        self.covered = 0;
        self.shown.clear();
        to
    }

    fn show_new(&mut self, out: &mut Vec<u8>, placeholders: &[Arc<str>]) {
        for p in placeholders {
            if !self.shown.contains(p) {
                out.extend_from_slice(p.as_bytes());
                self.shown.push(p.clone());
            }
        }
    }
}

/// Where [`Stream::flush`] stopped writing.
enum Stop {
    /// Before a run held back whole: write raw bytes up to it, keep the rest.
    Before(usize),
    /// Inside a run written early: keep from here, as covered.
    Within(usize),
}

/// How many pending bytes [`Stream::flush`] holds back for one run before
/// writing it early.
const HOLD_LIMIT: usize = 1024 * 1024;

/// The shortest piece of a secret value that counts as revealing it; shorter
/// ones are too common to tell.
pub const MIN_SECRET_PIECE: usize = 4;

/// How much [`Redactor::copy`] reads at a time.
const COPY_CHUNK: usize = 64 * 1024;

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

/// Replacing whole UTF-8 sequences in valid UTF-8 keeps it valid: a match
/// always starts on a character boundary because a valid sequence never
/// starts with a continuation byte.
fn into_string(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// Every secret value this process has resolved, for sinks that are not
/// tied to one request: logs (`-v`, `RUST_LOG`) go through
/// [`redact_logs`] as a safety net over events that are already redacted.
fn log_secrets() -> &'static RwLock<LogSecrets> {
    static SECRETS: OnceLock<RwLock<LogSecrets>> = OnceLock::new();
    SECRETS.get_or_init(RwLock::default)
}

/// The values logs redact, and the layers they were taken from.
#[derive(Default)]
struct LogSecrets {
    redactor: Redactor,
    /// Layers already absorbed. A run's redactor shares its layers with
    /// every request's, so each is read once, not once per request (which
    /// made a run quadratic in its number of secrets). Holding a `Weak`
    /// also keeps a seen layer from growing: `Arc::get_mut` refuses it, so
    /// new values go to a new layer, which is absorbed in turn.
    seen: Vec<Weak<Layer>>,
}

impl LogSecrets {
    /// Absorb the values of every layer of `redactor` not seen yet; how
    /// many values were read.
    fn remember(&mut self, redactor: &Redactor) -> usize {
        self.seen.retain(|layer| layer.strong_count() > 0);
        let mut read = 0;
        for layer in &redactor.layers {
            let ptr = Arc::as_ptr(layer);
            if self.seen.iter().any(|seen| seen.as_ptr() == ptr) {
                continue;
            }
            for (name, value) in layer.entries() {
                self.redactor.add_value(name, value);
            }
            read += layer.entries().len();
            self.seen.push(Arc::downgrade(layer));
        }
        read
    }
}

/// Make [`redact_logs`] redact every value `redactor` knows. Call it as soon
/// as secrets are resolved, before anything about the request is logged.
/// Only layers not remembered yet are read, and their values go to layers
/// of the logs' own, whose matcher is built when a log line is written.
pub fn remember_for_logs(redactor: &Redactor) {
    if redactor.is_empty() {
        return;
    }
    log_secrets()
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .remember(redactor);
}

/// `line` with every secret value remembered by [`remember_for_logs`]
/// replaced.
pub fn redact_logs(line: &str) -> String {
    log_secrets()
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .redactor
        .text(line)
        .into_owned()
}

/// Whether a literal credential is worth redacting: long enough, and not
/// a `«redacted:<name>»` placeholder or a `{{name}}` marker, as
/// placeholder-mode exports (which never read secrets) write them.
fn is_credential(value: &str) -> bool {
    let wrapped = |open: &str, close: &str| {
        value.starts_with(open) && value.ends_with(close) && value.matches(open).count() == 1
    };
    let marker = wrapped("«redacted:", "»") || wrapped("{{", "}}");
    !marker && value.chars().count() >= MIN_HEADER_CREDENTIAL
}

/// The forms of a secret `value` an echo may carry besides those every
/// value is matched in (`redact/forms.rs`): base64 (standard and
/// URL-safe, padded or not), for a secret echoed inside a token, and hex
/// in lower and upper case, for a secret echoed as bytes (JSON-RPC nodes
/// write data as hex).
fn echo_forms(value: &str) -> Vec<String> {
    let mut forms: Vec<String> = [&STANDARD, &STANDARD_NO_PAD, &URL_SAFE, &URL_SAFE_NO_PAD]
        .iter()
        .map(|engine| engine.encode(value))
        .collect();
    let hex: String = value.bytes().map(|b| format!("{b:02x}")).collect();
    forms.push(hex.to_uppercase());
    forms.push(hex);
    forms
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
    fn a_credential_that_is_already_a_placeholder_is_kept() {
        let mut r = Redactor::new();
        r.add_header_value("Authorization", "Bearer «redacted:api_token»");
        assert!(r.is_empty());
        r.add_header_value("Authorization", "Bearer «redacted:a»x-literal-token");
        assert!(!r.is_empty());
        assert!(is_credential("literal-token"));
        assert!(!is_credential("{{api_token}}"));
        assert!(!is_credential("short"));
    }

    #[test]
    fn json_values_are_redacted_in_strings_keys_and_numbers() {
        let r = Redactor::new().with(&Secret::new("pin", "90817263".to_owned()));
        let mut value = serde_json::json!({"90817263": [90817263, "x90817263", 7, true, null]});
        r.json(&mut value);
        assert_eq!(
            value,
            serde_json::json!({"«redacted:pin»": ["«redacted:pin»", "x«redacted:pin»", 7, true, null]})
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
    fn base64_echoes_of_a_secret_are_replaced() {
        // Regression: a JSON-RPC error message echoing a secret in base64.
        let r = Redactor::new().with(&Secret::new("api_key", "CANARY-s3cr3t/+=x".to_owned()));
        let standard = STANDARD.encode("CANARY-s3cr3t/+=x");
        assert_eq!(standard, "Q0FOQVJZLXMzY3IzdC8rPXg=");
        assert_eq!(
            r.text(&format!("bad key b64={standard}")),
            "bad key b64=«redacted:api_key»"
        );
        let url_safe = URL_SAFE_NO_PAD.encode("CANARY-s3cr3t/+=x");
        assert_eq!(r.text(&url_safe), "«redacted:api_key»");
    }

    #[test]
    fn hex_echoes_of_a_secret_are_replaced() {
        // Regression: an error message echoing the secret as hex kept it.
        let r = Redactor::new().with(&Secret::new("api_key", "CANARY-s3cr3t/+=x".to_owned()));
        for echo in [
            "43414e4152592d7333637233742f2b3d78",
            "43414E4152592D7333637233742F2B3D78",
        ] {
            assert_eq!(
                r.text(&format!("hex=0x{echo}.")),
                "hex=0x«redacted:api_key».",
                "{echo}"
            );
        }
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
        // The longest value is the secret's hex form.
        assert_eq!(r.longest(), "simpleCANARY".len() * 2);
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
    fn echoes_from_common_encoders_are_replaced() {
        let r = Redactor::new().with(&Secret::new("pw", "a/b&c<d>'e\"f!(é)*".to_owned()));
        for echo in [
            r#"a\/b&c<d>'e\"f!(\u00e9)*"#,
            r#"a/b\u0026c\u003cd\u003e'e\"f!(é)*"#,
            "a/b%26c%3Cd%3E%27e%22f%21%28%C3%A9%29%2A",
            "a%2Fb%26c%3Cd%3E'e%22f!(%C3%A9)*",
            "a/b&amp;c&lt;d&gt;&#x27;e&quot;f!(é)*",
            "a/b&amp;c&lt;d&gt;&#39;e&#34;f!(é)*",
            "a&#47;b&#38;c&#60;d&#62;&#39;e&#34;f&#33;&#40;é&#41;&#42;",
        ] {
            assert_eq!(r.text(&format!("[{echo}]")), "[«redacted:pw»]", "{echo}");
        }
    }

    #[test]
    fn captures_straddling_or_masking_a_secret_reveal_it() {
        let r = Redactor::new().with(&Secret::new("tok", "QJX7vwP3K".to_owned()));
        assert!(r.reveals_secret(r#"v":"QJX7v"#));
        assert!(r.reveals_secret("****wP3K"));
        assert!(!r.reveals_secret("****P3K"));
        let mut merged = Redactor::new();
        merged.merge(&r);
        assert!(merged.reveals_secret("x7vwP"));
        assert!(!merged.reveals_secret("x7vwp"));
    }

    #[test]
    fn copies_are_redacted_across_chunk_boundaries() {
        let secret = "Kq8é\"Vb-m0nk3y";
        let r = Redactor::new().with(&Secret::new("tok", secret.to_owned()));
        let json = r#"Kq8\u00e9\"Vb-m0nk3y"#;
        for pad in [
            0,
            1,
            COPY_CHUNK - 30,
            COPY_CHUNK - 7,
            COPY_CHUNK,
            3 * COPY_CHUNK - 3,
        ] {
            let body = format!(
                "{}{secret}|{json}|{}",
                "A".repeat(pad),
                "B".repeat(pad % 97)
            );
            let mut out = Vec::new();
            r.copy(body.as_bytes(), &mut out).unwrap();
            let out = String::from_utf8(out).unwrap();
            let expected = r.text(&body).into_owned();
            assert_eq!(out, expected, "pad {pad}");
            assert!(!out.contains("m0nk3y"), "pad {pad}");
        }
        let mut out = Vec::new();
        Redactor::new().copy(&b"plain"[..], &mut out).unwrap();
        assert_eq!(out, b"plain");
    }

    #[test]
    fn encoded_pieces_of_a_secret_reveal_it() {
        let r = Redactor::new()
            .with(&Secret::new("path", "a/b/c/d/e/f/g".to_owned()))
            .with(&Secret::new("quoted", "q\"r\"s\"t".to_owned()))
            .with(&Secret::new("spaced", "y z w v u".to_owned()))
            .with(&Secret::new("html", "t&en<ok".to_owned()));
        for piece in [
            "%2Fc%2Fd%2Fe%2F",
            r#"q\"r\"s\""#,
            "y+z+w+v",
            "&amp;en&lt;ok",
        ] {
            assert!(r.reveals_secret(piece), "{piece}");
        }
        assert!(!r.reveals_secret("%2Fx%2Fy%2F"));
        let long = "x/".repeat(4200);
        let r = Redactor::new().with(&Secret::new("long", long));
        assert!(r.reveals_secret(&"x%2F".repeat(10)));
    }

    #[test]
    fn a_merged_layer_keeps_one_entry_per_name() {
        let mut r = Redactor::new().with(&Secret::new("one", "first-value".to_owned()));
        let shared = r.clone();
        r.add(&Secret::new("one", "second-value".to_owned()));
        r.merge(&shared);
        r.add(&Secret::new("one", "third-value".to_owned()));
        assert_eq!(r.secret_names(), ["one"]);
        for layer in &r.layers {
            assert!(layer.names().len() <= 1, "{:?}", layer.names());
        }
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

    /// A data run remembers its many secrets once, then each request's
    /// redactor (the run's plus a few values): only the new values are read,
    /// so remembering stays linear in the run's secrets, not quadratic.
    /// How many values a secret adds: itself and its distinct echo forms.
    fn values_of(secret: &str) -> usize {
        let mut values = std::collections::BTreeSet::from([secret.to_owned()]);
        values.extend(echo_forms(secret));
        values.len()
    }

    #[test]
    fn logs_read_each_shared_layer_once() {
        let mut run = Redactor::new();
        for i in 0..1000 {
            run.add(&Secret::new("password", format!("pw-row-{i:05}")));
        }
        let mut logs = LogSecrets::default();
        let values: usize = (0..1000)
            .map(|i| values_of(&format!("pw-row-{i:05}")))
            .sum();
        assert_eq!(logs.remember(&run), values);
        for i in 0..50 {
            let mut request = run.clone();
            let token = format!("tok-req-{i:05}");
            request.add(&Secret::new("api_token", token.clone()));
            assert_eq!(logs.remember(&request), values_of(&token), "request {i}");
        }
        assert_eq!(logs.remember(&run), 0);
        let text = logs.redactor.text("pw-row-00999 tok-req-00049");
        assert_eq!(text, "«redacted:password» «redacted:api_token»");
    }

    /// A layer the logs have read never grows: a value added later still
    /// reaches the logs.
    #[test]
    fn values_added_after_remembering_reach_the_logs() {
        let mut run = Redactor::new().with(&Secret::new("one", "first-secret".to_owned()));
        let mut logs = LogSecrets::default();
        logs.remember(&run);
        run.add(&Secret::new("two", "second-secret".to_owned()));
        assert!(logs.remember(&run) >= 1);
        assert_eq!(logs.redactor.text("second-secret"), "«redacted:two»");
    }
}
