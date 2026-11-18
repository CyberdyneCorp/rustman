//! The last pass of extraction, over every text the import writes:
//!
//! 1. well-known tokens (GitHub, GitLab, Slack, Stripe, AWS, Google API
//!    keys, JWTs, PEM private keys), also when percent-encoded, are moved
//!    wherever they appear: URLs, headers, bodies, variables, names and
//!    descriptions;
//! 2. every value moved so far is looked for where it repeats, raw and
//!    (for secret-like values) percent-, JSON- or HTML-encoded. Since
//!    placeholders are substituted raw, only a change that sends the same
//!    bytes uses `{{name}}`:
//!    - a raw secret-like repeat ([`is_secret_like`]) that fills the field,
//!      sits on token boundaries, or has a token shape becomes `{{name}}`;
//!    - an encoded or in-word secret-like repeat becomes `«redacted:name»`,
//!      with a report note;
//!    - a repeat of a secret from a source environment file in a request
//!      becomes `«redacted:name»` with a note: `{{name}}` would resolve to
//!      another value under another environment;
//!    - any other repeat (`password` in `grant_type=password`) is left as
//!      written, with a `review` note naming the secret, never the value;
//!    - a field whose percent-, JSON- or HTML-decoded reading still shows a
//!      secret-like value becomes `«redacted:name»` as a whole.
//!
//! Environment files template only their own secrets (whole or on token
//! boundaries) and redact every other secret-like repeat.
//!
//! Fields that take templates get the `{{name}}` placeholder; free text
//! (names, descriptions, report notes and locations) gets
//! `«redacted:name»`.

use std::collections::HashSet;
use std::ops::Range;

use aho_corasick::{AhoCorasick, MatchKind};

use super::{Extractor, has_literal, is_known_token, is_redaction_marker, known_tokens, splice};
use crate::ir::{
    Folder, ImportedAuth, ImportedBody, ImportedCollection, ImportedRequest, Item, Location, Note,
    NoteKind, Pair, VisitStrings,
};

/// The shortest moved value looked for where it repeats: shorter values
/// (`1`, `true`, `admin`) would only produce noise.
pub const MIN_REPEAT: usize = 6;

/// The longest moved value looked for elsewhere: credentials are far
/// shorter, and matching megabyte-long values would cost memory for no
/// gain.
pub const MAX_REPEAT: usize = 16 * 1024;

/// The shortest value replaced where it repeats (see [`is_secret_like`]).
pub const MIN_SECRET_LIKE: usize = 8;

/// Values this long are secret-like whatever their characters (a
/// 16-digit key, a 32-letter token).
pub const LONG_SECRET: usize = 16;

/// Whether `value` looks like a credential rather than a word or number:
/// at least [`MIN_SECRET_LIKE`] characters, and either from at least two
/// of the classes letters, digits and symbols or at least [`LONG_SECRET`]
/// characters long. `password`, `123456`, `example` and `application` are
/// not; `p4ssw0rd!`, `sk-live-42ab` and `8473629184736291` are.
pub fn is_secret_like(value: &str) -> bool {
    let len = value.chars().count();
    if len < MIN_SECRET_LIKE {
        return false;
    }
    if len >= LONG_SECRET {
        return true;
    }
    let letters = value.chars().any(char::is_alphabetic);
    let digits = value.chars().any(|c| c.is_ascii_digit());
    let symbols = value
        .chars()
        .any(|c| !c.is_alphanumeric() && !c.is_whitespace());
    [letters, digits, symbols]
        .into_iter()
        .filter(|c| *c)
        .count()
        >= 2
}

/// How a field is written.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Takes `{{name}}` placeholders.
    Template,
    /// Shown as written: gets `«redacted:name»`.
    Free,
}

impl Kind {
    fn placeholder(self, name: &str) -> String {
        match self {
            Self::Template => format!("{{{{{name}}}}}"),
            Self::Free => format!("«redacted:{name}»"),
        }
    }
}

/// One text the import writes: what it is, for review notes.
struct Field<'a> {
    kind: Kind,
    /// `request`, `collection` or `environment`: whose field it is.
    owner: &'static str,
    /// `url`, `header X-Note`, `description`, ...
    label: String,
    at: &'a Location,
    /// The environment of an environment file's field.
    env: Option<&'a str>,
}

impl Extractor<'_> {
    pub(super) fn scrub(&mut self, collection: &mut ImportedCollection) {
        walk_folder(&mut collection.root, &mut |text, field| {
            self.move_tokens(text, field.kind, field.at);
        });
        let repeats = Repeats::new(&self.moved);
        let mut review = Review::default();
        walk_folder(&mut collection.root, &mut |text, field| {
            repeats.scan(text, &field, Some(&mut review));
        });
        let name_field = Field {
            kind: Kind::Free,
            owner: "collection",
            label: "name".to_owned(),
            at: &collection.root.location,
            env: None,
        };
        repeats.scan(&mut collection.name, &name_field, Some(&mut review));
        for env in &mut collection.environments {
            let name = super::environment_name(&env.name);
            let at = env.location.clone();
            for var in env.variables.iter_mut().filter(|v| !v.secret) {
                let field = Field {
                    kind: Kind::Template,
                    owner: "environment",
                    label: format!("variable {}", var.name),
                    at: &at,
                    env: Some(&name),
                };
                repeats.scan(&mut var.value, &field, Some(&mut review));
            }
            scrub_location(&mut env.location, &repeats);
        }
        walk_locations(&mut collection.root, &mut |at| scrub_location(at, &repeats));
        for note in &mut collection.notes {
            scrub_note(note, &repeats);
        }
        for moved in &mut self.moved {
            scrub_location(&mut moved.location, &repeats);
        }
        collection.notes.extend(review.notes);
    }

    /// Move the well-known tokens in the literal parts of `text`, also
    /// when percent-encoded (`-----BEGIN%20RSA%20PRIVATE%20KEY-----`).
    fn move_tokens(&mut self, text: &mut String, kind: Kind, at: &Location) {
        let mut edits = Vec::new();
        for span in literal_spans(text) {
            let literal = &text[span.clone()];
            let mut found: Vec<(Range<usize>, String, &'static str)> = known_tokens(literal)
                .into_iter()
                .map(|t| (t.range.clone(), literal[t.range].to_owned(), t.name))
                .collect();
            if found.is_empty()
                && let Some(decoded) = PercentDecoded::of(literal)
            {
                for t in known_tokens(&decoded.text) {
                    let value = decoded.text[t.range.clone()].to_owned();
                    found.push((decoded.original(t.range), value, t.name));
                }
            }
            for (range, value, kind_name) in found {
                if is_redaction_marker(&value) || !has_literal(&value) {
                    continue;
                }
                let name = self.move_token(kind_name, &value, at);
                let range = span.start + range.start..span.start + range.end;
                edits.push((range, kind.placeholder(&name)));
            }
        }
        if !edits.is_empty() {
            *text = splice(text, edits);
        }
    }
}

/// Percent-decoded text, with the original position of each byte.
struct PercentDecoded {
    text: String,
    /// `origin[i]`: where decoded byte `i` starts in the original; one
    /// more entry for the end.
    origin: Vec<usize>,
}

impl PercentDecoded {
    /// `None` when `text` has no `%XX` escape or does not decode to UTF-8.
    fn of(text: &str) -> Option<Self> {
        if !text.contains('%') {
            return None;
        }
        let bytes = text.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut origin = Vec::with_capacity(bytes.len() + 1);
        let mut i = 0;
        let mut escaped = false;
        while i < bytes.len() {
            let hex = (bytes[i] == b'%')
                .then(|| bytes.get(i + 1..i + 3))
                .flatten()
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok());
            origin.push(i);
            match hex {
                Some(byte) => {
                    out.push(byte);
                    i += 3;
                    escaped = true;
                }
                None => {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
        }
        origin.push(bytes.len());
        let text = String::from_utf8(out).ok()?;
        escaped.then_some(Self { text, origin })
    }

    /// The original byte range of decoded `range`.
    fn original(&self, range: Range<usize>) -> Range<usize> {
        self.origin[range.start]..self.origin[range.end]
    }
}

/// `text` with its well-known tokens replaced by `«redacted:<kind>»`, for
/// free text read before extraction (such as the collection name).
pub fn redact_tokens(text: &str) -> String {
    let edits: Vec<(Range<usize>, String)> = known_tokens(text)
        .into_iter()
        .map(|t| (t.range, Kind::Free.placeholder(t.name)))
        .collect();
    if edits.is_empty() {
        text.to_owned()
    } else {
        splice(text, edits)
    }
}

/// Report notes about repeats, one per secret, field and reason.
#[derive(Default)]
struct Review {
    seen: HashSet<(String, String, String)>,
    notes: Vec<Note>,
}

impl Review {
    fn push(&mut self, secret: &str, field: &Field<'_>, reason: &str, message: String) {
        let place = format!("{}:{}", field.at.display(), field.label);
        if self
            .seen
            .insert((secret.to_owned(), place, reason.to_owned()))
        {
            self.notes
                .push(Note::new(NoteKind::Review, message, field.at));
        }
    }

    /// A weak value left as written.
    fn kept(&mut self, secret: &str, field: &Field<'_>) {
        let place = format!("{}:{}", field.at.display(), field.label);
        let message = format!("the value of secret {secret} also appears in {place}; review it");
        self.push(secret, field, "kept", message);
    }

    /// A secret-like value redacted where `{{name}}` would send different
    /// bytes (`how`: `in encoded form`, `inside other text`).
    fn redacted(&mut self, secret: &str, field: &Field<'_>, how: &str) {
        let message = format!(
            "{} {}: field {} contained secret {secret} {how}; it was redacted, edit the field to use {{{{{secret}}}}}",
            field.owner,
            field.at.display(),
            field.label
        );
        self.push(secret, field, how, message);
    }

    /// A secret of another scope redacted: `{{name}}` would resolve to
    /// another value under another environment.
    fn other_scope(&mut self, secret: &str, env: &str, field: &Field<'_>) {
        let message = format!(
            "{} {}: field {} repeated secret {secret} of environment {env}; it was redacted, edit the field",
            field.owner,
            field.at.display(),
            field.label
        );
        self.push(secret, field, "scope", message);
    }
}

/// How a pattern of [`Repeats`] relates to its secret.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Form {
    /// The value's exact bytes.
    Raw,
    /// Percent-encoded, JSON-escaped or HTML-escaped.
    Encoded,
}

/// One pattern: a moved value in one form.
struct Pattern {
    name: String,
    form: Form,
    /// Secret-like ([`is_secret_like`]): replaced; otherwise only noted.
    strong: bool,
    /// A well-known token shape: matched anywhere, not only on token
    /// boundaries.
    token: bool,
    /// The environment the secret is stored in.
    env: String,
    /// Moved from a source environment file: it resolves only under that
    /// environment, so the collection may not use it.
    env_scoped: bool,
}

/// The moved values, found in one pass over a text.
struct Repeats {
    matcher: Option<AhoCorasick>,
    patterns: Vec<Pattern>,
    /// The secret-like values as written, for decoded variants of a field.
    strong: Option<AhoCorasick>,
    /// The pattern index of each `strong` value.
    strong_patterns: Vec<usize>,
}

impl Repeats {
    fn new(moved: &[super::MovedSecret]) -> Self {
        let mut seen: HashSet<String> = HashSet::new();
        let mut values: Vec<String> = Vec::new();
        let mut patterns = Vec::new();
        let mut strong_values: Vec<String> = Vec::new();
        let mut strong_patterns = Vec::new();
        for m in moved {
            let value = m.secret.expose().as_str();
            let usable = value.len() <= MAX_REPEAT
                && value.chars().count() >= MIN_REPEAT
                && !value.trim().is_empty();
            if !usable || !seen.insert(value.to_owned()) {
                continue;
            }
            let strong = is_secret_like(value);
            let pattern = |form| Pattern {
                name: m.secret.name().to_owned(),
                form,
                strong,
                token: is_known_token(value),
                env: m.environment.clone(),
                env_scoped: m.from_environment,
            };
            if strong {
                strong_values.push(value.to_owned());
                strong_patterns.push(patterns.len());
            }
            values.push(value.to_owned());
            patterns.push(pattern(Form::Raw));
            if strong {
                for form in encoded_forms(value) {
                    if seen.insert(form.clone()) {
                        values.push(form);
                        patterns.push(pattern(Form::Encoded));
                    }
                }
            }
        }
        let build = |values: &[String]| {
            (!values.is_empty()).then(|| {
                AhoCorasick::builder()
                    .match_kind(MatchKind::LeftmostLongest)
                    .build(values)
                    .expect("literal patterns build")
            })
        };
        Self {
            matcher: build(&values),
            patterns,
            strong: build(&strong_values),
            strong_patterns,
        }
    }

    /// Replace the repeats of `text` (see the module documentation), then
    /// redact the whole field when a decoded variant of it still holds a
    /// secret-like value. Notes go to `review` when given.
    fn scan(&self, text: &mut String, field: &Field<'_>, mut review: Option<&mut Review>) {
        let Some(matcher) = &self.matcher else {
            return;
        };
        let placeholders = placeholder_spans(text);
        let mut edits = Vec::new();
        for m in matcher.find_iter(text.as_str()) {
            let range = m.start()..m.end();
            if placeholders.iter().any(|p| splits(p, &range)) {
                continue;
            }
            let p = &self.patterns[m.pattern().as_usize()];
            if let Some(replacement) =
                self.replacement(p, text, &range, field, review.as_deref_mut())
            {
                edits.push((range, replacement));
            }
        }
        if !edits.is_empty() {
            *text = splice(text, edits);
        }
        self.decoded(text, field, review);
    }

    /// What replaces the repeat `range` of pattern `p`, if anything.
    fn replacement(
        &self,
        p: &Pattern,
        text: &str,
        range: &Range<usize>,
        field: &Field<'_>,
        review: Option<&mut Review>,
    ) -> Option<String> {
        if !p.strong {
            if let Some(review) = review {
                review.kept(&p.name, field);
            }
            return None;
        }
        let same_scope = match field.env {
            Some(env) => p.env == env,
            None => !p.env_scoped,
        };
        if !same_scope {
            if let Some(review) = review {
                review.other_scope(&p.name, &p.env, field);
            }
            return Some(Kind::Free.placeholder(&p.name));
        }
        let whole = range.start == 0 && range.end == text.len();
        let placed = whole || p.token || on_boundaries(text, range);
        if p.form == Form::Raw && placed {
            return Some(field.kind.placeholder(&p.name));
        }
        if let Some(review) = review {
            let how = match p.form {
                Form::Encoded => "in encoded form",
                Form::Raw => "inside other text",
            };
            review.redacted(&p.name, field, how);
        }
        Some(Kind::Free.placeholder(&p.name))
    }

    /// When the percent-, JSON- or HTML-decoded `text` holds a secret-like
    /// value its raw form does not (a partly or differently encoded
    /// repeat), the whole field becomes `«redacted:name»`.
    fn decoded(&self, text: &mut String, field: &Field<'_>, review: Option<&mut Review>) {
        let Some(strong) = &self.strong else {
            return;
        };
        for variant in decoded_variants(text) {
            let Some(m) = strong.find(variant.as_str()) else {
                continue;
            };
            let p = &self.patterns[self.strong_patterns[m.pattern().as_usize()]];
            if let Some(review) = review {
                review.redacted(&p.name, field, "in encoded form");
            }
            *text = Kind::Free.placeholder(&p.name);
            return;
        }
    }
}

/// The byte ranges of the `{{...}}` placeholders of `text`.
fn placeholder_spans(text: &str) -> Vec<Range<usize>> {
    let mut spans = Vec::new();
    let mut at = 0;
    while let Some(open) = text[at..].find("{{").map(|i| at + i) {
        let Some(close) = text[open + 2..].find("}}").map(|i| open + 2 + i) else {
            break;
        };
        spans.push(open..close + 2);
        at = close + 2;
    }
    spans
}

/// Whether `range` cuts into the placeholder `p` without covering it: a
/// match inside a placeholder's name, or across its edge.
fn splits(p: &Range<usize>, range: &Range<usize>) -> bool {
    let overlaps = range.start < p.end && p.start < range.end;
    let covers = range.start <= p.start && p.end <= range.end;
    overlaps && !covers
}

/// `text` percent-decoded (`+` as a space), JSON-unescaped and
/// HTML-unescaped, each when it differs from `text`.
fn decoded_variants(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for variant in [
        percent_decoded(text),
        json_unescaped(text),
        html_unescaped(text),
    ] {
        if variant != text && !out.contains(&variant) {
            out.push(variant);
        }
    }
    out
}

/// Every `%XX` decoded and `+` read as a space; invalid UTF-8 is replaced.
fn percent_decoded(text: &str) -> String {
    if !text.contains(['%', '+']) {
        return text.to_owned();
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = (bytes[i] == b'%')
            .then(|| bytes.get(i + 1..i + 3))
            .flatten()
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (_, Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (b'+', None) => {
                out.push(b' ');
                i += 1;
            }
            (byte, None) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// JSON string escapes (`\"`, `\\`, `\/`, `\n`, `\uXXXX`, ...) decoded
/// anywhere in `text`.
fn json_unescaped(text: &str) -> String {
    if !text.contains('\\') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('b') => out.push('\u{8}'),
            Some('f') => out.push('\u{c}'),
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    Some(decoded) => out.push(decoded),
                    None => {
                        out.push_str("\\u");
                        out.push_str(&hex);
                    }
                }
            }
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// HTML entities (`&amp;`, `&quot;`, `&#39;`, `&#x27;`, ...) decoded.
fn html_unescaped(text: &str) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let entity = tail
            .find(';')
            .filter(|&end| end <= 10)
            .map(|end| &tail[1..end]);
        let decoded = entity.and_then(|e| match e {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => {
                let number = e.strip_prefix('#')?;
                let code = match number.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                    None => number.parse().ok()?,
                };
                char::from_u32(code)
            }
        });
        match (decoded, entity) {
            (Some(c), Some(e)) => {
                out.push(c);
                rest = &tail[e.len() + 2..];
            }
            _ => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The forms `value` takes once encoded, other than itself: percent-encoded
/// (upper and lower case hex), JSON-escaped (with and without `\/`) and
/// HTML-escaped.
fn encoded_forms(value: &str) -> Vec<String> {
    let json = json_escaped(value);
    let forms = [
        percent_encoded(value),
        lower_hex(&percent_encoded(value)),
        json.replace('/', "\\/"),
        json,
        html_escaped(value),
    ];
    let mut out: Vec<String> = Vec::new();
    for form in forms {
        if form != value && !out.contains(&form) {
            out.push(form);
        }
    }
    out
}

/// `text` with the hex digits of its `%XX` escapes in lower case.
fn lower_hex(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        let escape = rest.get(i..i + 3).unwrap_or(&rest[i..]);
        out.push_str(&escape.to_ascii_lowercase());
        rest = &rest[i + escape.len()..];
    }
    out.push_str(rest);
    out
}

/// `value` with `&`, `<`, `>`, `"` and `'` as HTML entities.
fn html_escaped(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// `value` with every byte outside the URL-unreserved set percent-encoded.
fn percent_encoded(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// `value` as the inside of a JSON string literal.
fn json_escaped(value: &str) -> String {
    let quoted = serde_json::to_string(value).unwrap_or_default();
    quoted
        .get(1..quoted.len().saturating_sub(1))
        .unwrap_or_default()
        .to_owned()
}

/// Whether the characters right before and after `range` of `text` (if
/// any) are not letters or digits.
fn on_boundaries(text: &str, range: &Range<usize>) -> bool {
    let before = text[..range.start].chars().next_back();
    let after = text[range.end..].chars().next();
    [before, after]
        .into_iter()
        .flatten()
        .all(|c| !c.is_alphanumeric())
}

fn scrub_free(text: &mut String, repeats: &Repeats, at: &Location) {
    let redacted = redact_tokens(text);
    if redacted != *text {
        *text = redacted;
    }
    let field = Field {
        kind: Kind::Free,
        owner: "report",
        label: String::new(),
        at,
        env: None,
    };
    repeats.scan(text, &field, None);
}

fn scrub_location(at: &mut Location, repeats: &Repeats) {
    let copy = at.clone();
    if let Some(item) = &mut at.item {
        scrub_free(item, repeats, &copy);
    }
}

fn scrub_note(note: &mut Note, repeats: &Repeats) {
    let at = note.location.clone();
    scrub_free(&mut note.message, repeats, &at);
    scrub_location(&mut note.location, repeats);
}

/// The byte ranges of `text` outside `{{...}}` placeholders.
fn literal_spans(text: &str) -> Vec<Range<usize>> {
    let mut spans = Vec::new();
    let mut at = 0;
    while let Some(open) = text[at..].find("{{").map(|i| at + i) {
        let Some(close) = text[open + 2..].find("}}").map(|i| open + 2 + i) else {
            break;
        };
        if open > at {
            spans.push(at..open);
        }
        at = close + 2;
    }
    if at < text.len() {
        spans.push(at..text.len());
    }
    spans
}

type Visit<'f> = dyn FnMut(&mut String, Field<'_>) + 'f;

fn field<'a>(kind: Kind, label: impl Into<String>, at: &'a Location) -> Field<'a> {
    Field {
        kind,
        owner: "request",
        label: label.into(),
        at,
        env: None,
    }
}

/// Every text of `folder` and below that is written to a file.
fn walk_folder(folder: &mut Folder, f: &mut Visit<'_>) {
    let at = folder.location.clone();
    {
        let mut own = |text: &mut String, mut field: Field<'_>| {
            field.owner = "collection";
            f(text, field);
        };
        own(&mut folder.name, field(Kind::Free, "name", &at));
        if let Some(description) = &mut folder.description {
            own(description, field(Kind::Free, "description", &at));
        }
        walk_auth(&mut folder.auth, &at, &mut own);
        walk_pairs(&mut folder.headers, "header", &at, &mut own);
        for var in &mut folder.variables {
            let label = format!("variable {}", var.name);
            own(&mut var.value, field(Kind::Template, label, &at));
        }
    }
    for item in &mut folder.items {
        match item {
            Item::Folder(child) => walk_folder(child, f),
            Item::Request(request) => walk_request(request, f),
        }
    }
}

fn walk_request(request: &mut ImportedRequest, f: &mut Visit<'_>) {
    let at = request.location.clone();
    f(&mut request.name, field(Kind::Free, "name", &at));
    if let Some(description) = &mut request.description {
        f(description, field(Kind::Free, "description", &at));
    }
    f(&mut request.url, field(Kind::Template, "url", &at));
    walk_pairs(&mut request.query, "query", &at, f);
    walk_pairs(&mut request.headers, "header", &at, f);
    if let Some(body) = &mut request.body {
        walk_body(body, &at, f);
    }
    walk_auth(&mut request.auth, &at, f);
    for var in &mut request.variables {
        let label = format!("variable {}", var.name);
        f(&mut var.value, field(Kind::Template, label, &at));
    }
}

fn walk_pairs(pairs: &mut [Pair], what: &str, at: &Location, f: &mut Visit<'_>) {
    for pair in pairs {
        let label = format!("{what} {}", pair.name);
        f(&mut pair.name, field(Kind::Template, label.clone(), at));
        f(&mut pair.value, field(Kind::Template, label, at));
    }
}

fn walk_body(body: &mut ImportedBody, at: &Location, f: &mut Visit<'_>) {
    match body {
        ImportedBody::Form { fields } => walk_pairs(fields, "form field", at, f),
        other => other.visit_strings(&mut |text| f(text, field(Kind::Template, "body", at))),
    }
}

fn walk_auth(auth: &mut Option<ImportedAuth>, at: &Location, f: &mut Visit<'_>) {
    auth.visit_strings(&mut |text| f(text, field(Kind::Template, "auth", at)));
}

/// Every location of `folder` and below (shown in the report).
fn walk_locations(folder: &mut Folder, f: &mut dyn FnMut(&mut Location)) {
    f(&mut folder.location);
    for script in &mut folder.scripts {
        f(&mut script.location);
    }
    for item in &mut folder.items {
        match item {
            Item::Folder(child) => walk_locations(child, f),
            Item::Request(request) => {
                f(&mut request.location);
                for script in &mut request.scripts {
                    f(&mut script.location);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustman_core::Secret;

    #[test]
    fn spans_skip_placeholders() {
        let text = "a{{b}}c{{d";
        let spans: Vec<&str> = literal_spans(text).into_iter().map(|r| &text[r]).collect();
        assert_eq!(spans, ["a", "c{{d"]);
        assert!(literal_spans("{{x}}").is_empty());
    }

    #[test]
    fn secret_like_values_need_length_and_two_classes() {
        for value in [
            "p4ssw0rd!",
            "sk-live-42ab",
            "CNRY2_SAMEVAL",
            "abc-defgh",
            "8473629184736291",
            "AbCdEfGhIjKlMnOpQrStUvWxYzAbCdEf",
        ] {
            assert!(is_secret_like(value), "{value}");
        }
        for value in [
            "password",
            "123456",
            "example",
            "application",
            "abcdefghij",
            "12345678",
            "123456789012345",
        ] {
            assert!(!is_secret_like(value), "{value}");
        }
    }

    fn scan(secrets: &[(&str, &str)], text: &str) -> (String, usize) {
        let moved: Vec<super::super::MovedSecret> = secrets
            .iter()
            .map(|(n, v)| super::super::MovedSecret {
                secret: Secret::new(*n, (*v).to_owned()),
                environment: "local".into(),
                location: Location::default(),
                from_environment: false,
            })
            .collect();
        let repeats = Repeats::new(&moved);
        let at = Location::item("c.json", "r");
        let mut review = Review::default();
        let mut out = text.to_owned();
        repeats.scan(
            &mut out,
            &field(Kind::Template, "url", &at),
            Some(&mut review),
        );
        (out, review.notes.len())
    }

    #[test]
    fn only_secret_like_values_on_boundaries_are_replaced() {
        // (a) whole value.
        assert_eq!(
            scan(&[("k", "SAME-VAL-42")], "SAME-VAL-42"),
            ("{{k}}".into(), 0)
        );
        // (b) embedded on token boundaries.
        assert_eq!(
            scan(&[("k", "SAME-VAL-42")], "/keys/SAME-VAL-42?x=1"),
            ("/keys/{{k}}?x=1".into(), 0)
        );
        // Not on a boundary: redacted, noted.
        assert_eq!(
            scan(&[("k", "SAME-VAL-42")], "xSAME-VAL-42"),
            ("x«redacted:k»".into(), 1)
        );
        // A token shape is matched anywhere, raw: same bytes sent.
        let gh = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        assert_eq!(
            scan(&[("g", gh)], &format!("abc{gh}")),
            ("abc{{g}}".into(), 0)
        );
        // (c) not secret-like: kept, noted, whole or embedded.
        assert_eq!(
            scan(&[("p", "password")], "password"),
            ("password".into(), 1)
        );
        assert_eq!(
            scan(&[("p", "123456")], "https://api.123456.com/orders/1234567"),
            ("https://api.123456.com/orders/1234567".into(), 1)
        );
    }

    #[test]
    fn encoded_forms_of_secret_like_values_are_found_too() {
        // A partly or differently encoded repeat: the whole field.
        let (out, notes) = scan(&[("p", "p@ss&w0rd=1")], "note=p%40ss&w0rd=1");
        assert_eq!((out.as_str(), notes), ("«redacted:p»", 1));
        let (out, _) = scan(&[("p", "p@ss&w0rd=1")], "note=p%40ss%26w0rd%3d1");
        assert!(
            out.contains("«redacted:p»") && !out.contains("w0rd"),
            "{out}"
        );
        let (out, _) = scan(&[("p", "my secret pass 42")], "pw=my+secret+pass+42");
        assert_eq!(out, "«redacted:p»");
        let (out, _) = scan(&[("p", "p@ss&w0rd=1")], r#"{"p":"p\u0040ss\u0026w0rd=1"}"#);
        assert_eq!(out, "«redacted:p»");
        let (out, _) = scan(&[("p", "p@ss&w0rd=1")], "p&#64;ss&amp;w0rd=1");
        assert_eq!(out, "«redacted:p»");
        // A value holding `{{` is still matched as text.
        assert_eq!(
            scan(&[("b", "ab{{cd}}ef-123")], "ab{{cd}}ef-123"),
            ("{{b}}".into(), 0)
        );
        // Encoded repeats would change the bytes sent as `{{name}}`: they
        // are redacted, with a note.
        let (out, notes) = scan(
            &[("p", "p@ss/w0rd!")],
            "?p=p%40ss%2Fw0rd%21&l=p%40ss%2fw0rd%21",
        );
        assert_eq!(out, "?p=«redacted:p»&l=«redacted:p»");
        assert_eq!(notes, 1);
        let (out, _) = scan(&[("q", "ab\"c-1234")], r#"{"x": "ab\"c-1234"}"#);
        assert_eq!(out, r#"{"x": "«redacted:q»"}"#);
        let (out, _) = scan(&[("s", "ab/cd-1234")], r#"{"s": "ab\/cd-1234"}"#);
        assert_eq!(out, r#"{"s": "«redacted:s»"}"#);
        let (out, _) = scan(&[("h", "a&b<c>-1234")], "x=a&amp;b&lt;c&gt;-1234");
        assert_eq!(out, "x=«redacted:h»");
    }

    #[test]
    fn percent_decoding_maps_back_to_the_original() {
        let decoded = PercentDecoded::of("a%20b%2Fc").unwrap();
        assert_eq!(decoded.text, "a b/c");
        assert_eq!(decoded.original(1..4), 1..8);
        assert!(PercentDecoded::of("plain").is_none());
    }
}
