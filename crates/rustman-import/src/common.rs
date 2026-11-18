//! Small helpers shared by the source parsers: methods, URLs, bodies by
//! MIME type, and JSON values as variable text.

use rustman_core::model::Method;
use serde_json::Value;

use crate::ir::{ImportedBody, Location, Note, NoteKind, Pair};

/// The method `raw`, upper-cased; `GET` when absent, and `GET` with an
/// `unsupported` note when it is not an HTTP token.
pub(crate) fn method(raw: Option<&str>, at: &Location, notes: &mut Vec<Note>) -> String {
    let Some(raw) = raw.map(str::trim).filter(|m| !m.is_empty()) else {
        return "GET".to_owned();
    };
    let upper = raw.to_ascii_uppercase();
    if Method::parse(&upper).is_ok() {
        return upper;
    }
    let message = format!("invalid method '{raw}' unsupported; GET used");
    notes.push(Note::new(NoteKind::Unsupported, message, at));
    "GET".to_owned()
}

/// The URL without its query string or fragment, and the query parameters.
pub(crate) fn split_url(raw: &str) -> (String, Vec<Pair>) {
    let without_fragment = raw.split('#').next().unwrap_or_default();
    match without_fragment.split_once('?') {
        Some((base, query)) => (base.to_owned(), query_pairs(query)),
        None => (without_fragment.to_owned(), Vec::new()),
    }
}

/// `a=1&b=2` as pairs, kept as written (not percent-decoded).
pub(crate) fn query_pairs(query: &str) -> Vec<Pair> {
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (name, value) = p.split_once('=').unwrap_or((p, ""));
            Pair::new(name, value)
        })
        .collect()
}

/// The path of a URL such as `https://x/a` or `{{host}}/a` (`/` when it
/// has none).
pub(crate) fn path_of(url: &str) -> &str {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    after_scheme.find('/').map_or("/", |i| &after_scheme[i..])
}

/// Whether the MIME type `mime` is JSON (`application/json`, `+json`).
pub(crate) fn is_json_mime(mime: &str) -> bool {
    mime.to_ascii_lowercase().contains("json")
}

/// A text body: JSON when `mime` says so, otherwise text; `None` when empty.
pub(crate) fn text_body(mime: &str, text: &str) -> Option<ImportedBody> {
    if text.is_empty() {
        return None;
    }
    let text = text.to_owned();
    Some(if is_json_mime(mime) {
        ImportedBody::Json { text }
    } else {
        ImportedBody::Text { text }
    })
}

/// A JSON value as variable text: strings as they are, other scalars as
/// written, and objects or arrays as compact JSON.
pub(crate) fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// The value of header `name` (case-insensitive) in `headers`.
pub(crate) fn header<'a>(headers: &'a [Pair], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn methods_are_upper_cased_or_replaced() {
        let mut notes = Vec::new();
        let at = Location::default();
        assert_eq!(method(Some("post"), &at, &mut notes), "POST");
        assert_eq!(method(None, &at, &mut notes), "GET");
        assert!(notes.is_empty());
        assert_eq!(method(Some("no way"), &at, &mut notes), "GET");
        assert_eq!(notes.len(), 1);
    }

    #[test]
    fn urls_split_into_base_and_query() {
        let (base, query) = split_url("https://x/a?b=1&c&d=e=f#frag");
        assert_eq!(base, "https://x/a");
        assert_eq!(
            query,
            [
                Pair::new("b", "1"),
                Pair::new("c", ""),
                Pair::new("d", "e=f")
            ]
        );
        assert_eq!(split_url("https://x/a").1, []);
        assert_eq!(path_of("https://x.test/a/b"), "/a/b");
        assert_eq!(path_of("{{host}}/a"), "/a");
        assert_eq!(path_of("https://x.test"), "/");
    }

    #[test]
    fn text_bodies_follow_the_mime_type() {
        assert!(matches!(
            text_body("application/vnd.api+json", "{}"),
            Some(ImportedBody::Json { .. })
        ));
        assert!(matches!(
            text_body("text/plain", "hi"),
            Some(ImportedBody::Text { .. })
        ));
        assert_eq!(text_body("text/plain", ""), None);
    }

    #[test]
    fn values_become_text() {
        assert_eq!(value_text(&serde_json::json!("a")), "a");
        assert_eq!(value_text(&serde_json::json!(3)), "3");
        assert_eq!(value_text(&serde_json::json!(null)), "");
        assert_eq!(value_text(&serde_json::json!({"a": 1})), "{\"a\":1}");
    }
}
