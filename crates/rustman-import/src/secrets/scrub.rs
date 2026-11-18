//! The last pass of extraction, over every text the import writes:
//!
//! 1. well-known tokens (GitHub, GitLab, Slack, Stripe, AWS, Google API
//!    keys, JWTs, PEM private keys), also when percent-encoded, are moved
//!    wherever they appear: URLs, headers, bodies, variables, names and
//!    descriptions;
//! 2. every value moved so far is looked for where it repeats, and
//!    replaced only when that cannot change non-secret data:
//!    - a field whose whole value is a secret-like moved value
//!      ([`is_secret_like`]) becomes its placeholder;
//!    - a secret-like value inside a longer text is replaced only on token
//!      boundaries (no letter or digit right before or after it);
//!    - any other repeat (`password` in `grant_type=password`, `123456` in
//!      an order number) is left as written, and a `review` note names the
//!      secret and the field, never the value.
//!
//! Fields that take templates get the `{{name}}` placeholder; free text
//! (names, descriptions, report notes and locations) gets
//! `«redacted:name»`. Environment files only get whole-value replacements
//! by secrets of that same environment.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use aho_corasick::{AhoCorasick, MatchKind};

use super::{Extractor, has_literal, is_redaction_marker, known_tokens, splice};
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

/// Whether `value` looks like a credential rather than a word or number:
/// at least [`MIN_SECRET_LIKE`] characters, from at least two of the
/// classes letters, digits and symbols. `password`, `123456`, `example`
/// and `application` are not; `p4ssw0rd!` and `sk-live-42ab` are.
pub fn is_secret_like(value: &str) -> bool {
    if value.chars().count() < MIN_SECRET_LIKE {
        return false;
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
    /// `url`, `header X-Note`, `description`, ...
    label: String,
    at: &'a Location,
}

impl Extractor<'_> {
    pub(super) fn scrub(&mut self, collection: &mut ImportedCollection) {
        walk_folder(&mut collection.root, &mut |text, field| {
            self.move_tokens(text, field.kind, field.at);
        });
        let repeats = Repeats::new(self.moved.iter().map(|m| &m.secret));
        let mut review = Review::default();
        walk_folder(&mut collection.root, &mut |text, field| {
            repeats.scan(text, &field, Some(&mut review));
        });
        let name_field = Field {
            kind: Kind::Free,
            label: "name".to_owned(),
            at: &collection.root.location,
        };
        repeats.scan(&mut collection.name, &name_field, Some(&mut review));
        self.scrub_environments(collection);
        walk_locations(&mut collection.root, &mut |at| scrub_location(at, &repeats));
        for note in &mut collection.notes {
            scrub_note(note, &repeats);
        }
        for moved in &mut self.moved {
            scrub_location(&mut moved.location, &repeats);
        }
        collection.notes.extend(review.notes);
    }

    /// Plain variables of each environment whose whole value is one of
    /// that environment's secrets become its placeholder.
    fn scrub_environments(&self, collection: &mut ImportedCollection) {
        for env in &mut collection.environments {
            let name = super::environment_name(&env.name);
            let own = self
                .moved
                .iter()
                .filter(|m| m.environment == name)
                .map(|m| &m.secret);
            let whole: HashMap<&str, &str> = own
                .filter(|s| is_secret_like(s.expose()))
                .map(|s| (s.expose().as_str(), s.name()))
                .collect();
            for var in env.variables.iter_mut().filter(|v| !v.secret) {
                if let Some(secret) = whole.get(var.value.as_str()) {
                    var.value = Kind::Template.placeholder(secret);
                }
            }
        }
        let all = Repeats::new(self.moved.iter().map(|m| &m.secret));
        for env in &mut collection.environments {
            scrub_location(&mut env.location, &all);
        }
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

/// `review` notes for repeats left as written, one per secret and field.
#[derive(Default)]
struct Review {
    seen: HashSet<(String, String)>,
    notes: Vec<Note>,
}

impl Review {
    fn add(&mut self, secret: &str, field: &Field<'_>) {
        let place = format!("{}:{}", field.at.display(), field.label);
        if self.seen.insert((secret.to_owned(), place.clone())) {
            let message =
                format!("the value of secret {secret} also appears in {place}; review it");
            self.notes
                .push(Note::new(NoteKind::Review, message, field.at));
        }
    }
}

/// The moved values, found in one pass over a text.
struct Repeats {
    matcher: Option<AhoCorasick>,
    /// The secret name of each pattern, by pattern index.
    names: Vec<String>,
    /// Whether each pattern is secret-like.
    strong: Vec<bool>,
}

impl Repeats {
    fn new<'s>(secrets: impl Iterator<Item = &'s rustman_core::Secret<String>>) -> Self {
        let mut seen: HashSet<String> = HashSet::new();
        let mut values: Vec<String> = Vec::new();
        let mut names = Vec::new();
        let mut strong = Vec::new();
        for secret in secrets {
            let value = secret.expose().as_str();
            let usable = value.len() <= MAX_REPEAT
                && value.chars().count() >= MIN_REPEAT
                && !value.contains("{{")
                && !value.trim().is_empty();
            if !usable || !seen.insert(value.to_owned()) {
                continue;
            }
            let like = is_secret_like(value);
            values.push(value.to_owned());
            names.push(secret.name().to_owned());
            strong.push(like);
            // The forms a secret-like value takes in a URL or a JSON string.
            if like {
                for form in [percent_encoded(value), json_escaped(value)] {
                    if form != value && seen.insert(form.clone()) {
                        values.push(form);
                        names.push(secret.name().to_owned());
                        strong.push(true);
                    }
                }
            }
        }
        let matcher = (!values.is_empty()).then(|| {
            AhoCorasick::builder()
                .match_kind(MatchKind::LeftmostLongest)
                .build(&values)
                .expect("literal patterns build")
        });
        Self {
            matcher,
            names,
            strong,
        }
    }

    /// Replace the moved values of `text` that are safe to replace, and
    /// note the others in `review` (when given).
    fn scan(&self, text: &mut String, field: &Field<'_>, mut review: Option<&mut Review>) {
        let Some(matcher) = &self.matcher else {
            return;
        };
        let mut edits = Vec::new();
        for span in literal_spans(text) {
            for m in matcher.find_iter(&text[span.clone()]) {
                let range = span.start + m.start()..span.start + m.end();
                let i = m.pattern().as_usize();
                let whole = range.start == 0 && range.end == text.len();
                let replace = self.strong[i] && (whole || on_boundaries(text, &range));
                if replace {
                    edits.push((range, field.kind.placeholder(&self.names[i])));
                } else if let Some(review) = review.as_deref_mut() {
                    review.add(&self.names[i], field);
                }
            }
        }
        if !edits.is_empty() {
            *text = splice(text, edits);
        }
    }
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
        label: String::new(),
        at,
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
        label: label.into(),
        at,
    }
}

/// Every text of `folder` and below that is written to a file.
fn walk_folder(folder: &mut Folder, f: &mut Visit<'_>) {
    let at = folder.location.clone();
    f(&mut folder.name, field(Kind::Free, "name", &at));
    if let Some(description) = &mut folder.description {
        f(description, field(Kind::Free, "description", &at));
    }
    walk_auth(&mut folder.auth, &at, f);
    walk_pairs(&mut folder.headers, "header", &at, f);
    for var in &mut folder.variables {
        let label = format!("variable {}", var.name);
        f(&mut var.value, field(Kind::Template, label, &at));
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
        for value in ["p4ssw0rd!", "sk-live-42ab", "CNRY2_SAMEVAL", "abc-defgh"] {
            assert!(is_secret_like(value), "{value}");
        }
        for value in [
            "password",
            "123456",
            "example",
            "application",
            "abcdefghij",
            "12345678",
        ] {
            assert!(!is_secret_like(value), "{value}");
        }
    }

    fn scan(secrets: &[(&str, &str)], text: &str) -> (String, usize) {
        let secrets: Vec<Secret<String>> = secrets
            .iter()
            .map(|(n, v)| Secret::new(*n, (*v).to_owned()))
            .collect();
        let repeats = Repeats::new(secrets.iter());
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
        // Not on a boundary: kept, noted.
        assert_eq!(
            scan(&[("k", "SAME-VAL-42")], "xSAME-VAL-42"),
            ("xSAME-VAL-42".into(), 1)
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
        let (out, _) = scan(&[("p", "p@ss/w0rd!")], "?p=p%40ss%2Fw0rd%21&q=\"p@ss\"");
        assert_eq!(out, "?p={{p}}&q=\"p@ss\"");
        let (out, _) = scan(&[("q", "ab\"c-1234")], r#"{"x": "ab\"c-1234"}"#);
        assert_eq!(out, r#"{"x": "{{q}}"}"#);
    }

    #[test]
    fn percent_decoding_maps_back_to_the_original() {
        let decoded = PercentDecoded::of("a%20b%2Fc").unwrap();
        assert_eq!(decoded.text, "a b/c");
        assert_eq!(decoded.original(1..4), 1..8);
        assert!(PercentDecoded::of("plain").is_none());
    }
}
