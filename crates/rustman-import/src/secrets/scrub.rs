//! The last pass of extraction, over every text the import writes:
//!
//! 1. well-known tokens (GitHub, GitLab, Slack, Stripe, AWS, Google API
//!    keys, JWTs, PEM private keys) are moved wherever they appear: URLs,
//!    headers, bodies, variables, names and descriptions;
//! 2. every value moved so far is replaced wherever it repeats verbatim.
//!
//! Fields that take templates get the `{{name}}` placeholder; free text
//! (names, descriptions, report notes and locations) gets
//! `«redacted:name»`.

use std::collections::HashMap;
use std::ops::Range;

use aho_corasick::{AhoCorasick, MatchKind};

use super::{Extractor, has_literal, is_redaction_marker, known_tokens, splice};
use crate::ir::{Folder, ImportedCollection, ImportedRequest, Item, Location, Note, VisitStrings};

/// The shortest moved value replaced where it repeats: shorter values
/// (`1`, `true`, `admin`) would replace unrelated text.
pub const MIN_REPEAT: usize = 6;

/// The longest moved value looked for elsewhere: credentials are far
/// shorter, and matching megabyte-long values would cost memory for no
/// gain.
pub const MAX_REPEAT: usize = 16 * 1024;

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

impl Extractor<'_> {
    pub(super) fn scrub(&mut self, collection: &mut ImportedCollection) {
        walk_folder(&mut collection.root, &mut |text, kind, at| {
            self.move_tokens(text, kind, at);
        });
        let repeats = Repeats::new(self);
        walk_folder(&mut collection.root, &mut |text, kind, _| {
            repeats.replace(text, kind);
        });
        for env in &mut collection.environments {
            for var in env.variables.iter_mut().filter(|v| !v.secret) {
                repeats.replace(&mut var.value, Kind::Template);
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
    }

    /// Move the well-known tokens in the literal parts of `text`.
    fn move_tokens(&mut self, text: &mut String, kind: Kind, at: &Location) {
        let mut edits = Vec::new();
        for span in literal_spans(text) {
            for token in known_tokens(&text[span.clone()]) {
                let range = span.start + token.range.start..span.start + token.range.end;
                let value = &text[range.clone()];
                if is_redaction_marker(value) || !has_literal(value) {
                    continue;
                }
                let name = self.move_token(token.name, value, at);
                edits.push((range, kind.placeholder(&name)));
            }
        }
        if !edits.is_empty() {
            *text = splice(text, edits);
        }
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

/// The values moved so far, found in one pass over a text.
struct Repeats {
    matcher: Option<AhoCorasick>,
    /// The secret name of each pattern, by pattern index.
    names: Vec<String>,
}

impl Repeats {
    fn new(extractor: &Extractor<'_>) -> Self {
        let mut seen: HashMap<&str, ()> = HashMap::new();
        let mut values = Vec::new();
        let mut names = Vec::new();
        for moved in &extractor.moved {
            let value = moved.secret.expose().as_str();
            let usable = value.len() <= MAX_REPEAT
                && value.chars().count() >= MIN_REPEAT
                && !value.contains("{{")
                && !value.trim().is_empty();
            if usable && seen.insert(value, ()).is_none() {
                values.push(value);
                names.push(moved.secret.name().to_owned());
            }
        }
        let matcher = (!values.is_empty()).then(|| {
            AhoCorasick::builder()
                .match_kind(MatchKind::LeftmostLongest)
                .build(&values)
                .expect("literal patterns build")
        });
        Self { matcher, names }
    }

    /// Replace the moved values in the literal parts of `text`.
    fn replace(&self, text: &mut String, kind: Kind) {
        let Some(matcher) = &self.matcher else {
            return;
        };
        let mut edits = Vec::new();
        for span in literal_spans(text) {
            for m in matcher.find_iter(&text[span.clone()]) {
                let range = span.start + m.start()..span.start + m.end();
                edits.push((range, kind.placeholder(&self.names[m.pattern().as_usize()])));
            }
        }
        if !edits.is_empty() {
            *text = splice(text, edits);
        }
    }
}

fn scrub_free(text: &mut String, repeats: &Repeats) {
    let redacted = redact_tokens(text);
    if redacted != *text {
        *text = redacted;
    }
    repeats.replace(text, Kind::Free);
}

fn scrub_location(at: &mut Location, repeats: &Repeats) {
    if let Some(item) = &mut at.item {
        scrub_free(item, repeats);
    }
}

fn scrub_note(note: &mut Note, repeats: &Repeats) {
    scrub_free(&mut note.message, repeats);
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

type Visit<'f> = dyn FnMut(&mut String, Kind, &Location) + 'f;

/// Every text of `folder` and below that is written to a file.
fn walk_folder(folder: &mut Folder, f: &mut Visit<'_>) {
    let at = folder.location.clone();
    f(&mut folder.name, Kind::Free, &at);
    if let Some(description) = &mut folder.description {
        f(description, Kind::Free, &at);
    }
    folder
        .auth
        .visit_strings(&mut |text| f(text, Kind::Template, &at));
    folder
        .headers
        .visit_strings(&mut |text| f(text, Kind::Template, &at));
    folder
        .variables
        .visit_strings(&mut |text| f(text, Kind::Template, &at));
    for item in &mut folder.items {
        match item {
            Item::Folder(child) => walk_folder(child, f),
            Item::Request(request) => walk_request(request, f),
        }
    }
}

fn walk_request(request: &mut ImportedRequest, f: &mut Visit<'_>) {
    let at = request.location.clone();
    f(&mut request.name, Kind::Free, &at);
    if let Some(description) = &mut request.description {
        f(description, Kind::Free, &at);
    }
    request.visit_strings(&mut |text| f(text, Kind::Template, &at));
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

    #[test]
    fn spans_skip_placeholders() {
        let text = "a{{b}}c{{d";
        let spans: Vec<&str> = literal_spans(text).into_iter().map(|r| &text[r]).collect();
        assert_eq!(spans, ["a", "c{{d"]);
        assert!(literal_spans("{{x}}").is_empty());
    }
}
