//! The prebuilt search index (design D4): one entry per endpoint with its
//! name, method, path, kind, description and parameter names. Redaction
//! placeholders are left out, so no redacted value is indexed.

use serde::Serialize;

use crate::model::{DocSite, Endpoint};
use crate::rendered::Rendered;

/// The index format version, read by the search script.
pub const INDEX_VERSION: u32 = 1;

#[derive(Debug, Serialize)]
pub struct SearchIndex {
    pub version: u32,
    pub entries: Vec<SearchEntry>,
}

/// One endpoint. Short keys keep a 2,000-endpoint index small.
#[derive(Debug, Serialize)]
pub struct SearchEntry {
    /// Name.
    pub t: String,
    /// Method (empty for kinds without one).
    pub m: String,
    /// Path or URL template.
    pub p: String,
    /// Kind.
    pub k: String,
    /// Description text.
    pub d: String,
    /// Parameter, header and body field names.
    pub w: Vec<String>,
    /// The page, relative to the site root.
    pub u: String,
    /// Deprecated.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub x: bool,
}

/// The index of every endpoint of `site`, in navigation order.
pub fn index(site: &DocSite) -> SearchIndex {
    SearchIndex {
        version: INDEX_VERSION,
        entries: site.endpoints().into_iter().map(entry).collect(),
    }
}

fn entry(e: &Endpoint) -> SearchEntry {
    let words = [&e.query, &e.headers, &e.body_fields, &e.fields]
        .into_iter()
        .flatten()
        .map(|row| clean(row.name.as_str()))
        .filter(|w| !w.is_empty())
        .collect();
    SearchEntry {
        t: clean(e.name.as_str()),
        m: optional(e.method.as_ref()),
        p: optional(e.url.as_ref()),
        k: clean(e.kind.as_str()),
        d: description(e),
        w: words,
        u: format!("endpoints/{}.html", e.slug),
        x: e.deprecated,
    }
}

fn optional(text: Option<&Rendered>) -> String {
    text.map(|t| clean(t.as_str())).unwrap_or_default()
}

/// The summary and the description's text (its HTML without tags).
fn description(e: &Endpoint) -> String {
    let html = e.description.as_ref().map(|d| text_of(d.as_str()));
    let parts: Vec<String> = [optional(e.summary.as_ref()), html.unwrap_or_default()]
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect();
    clean(&parts.join(" "))
}

/// The text of `html`: tags dropped, the escapes [`crate::commonmark`]
/// writes decoded.
fn text_of(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// `text` without its `«redacted:name»` placeholders.
fn clean(text: &str) -> String {
    const START: &str = "«redacted:";
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find(START) {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        rest = tail
            .find('»')
            .map_or("", |end| &tail[end + '»'.len_utf8()..]);
    }
    out.push_str(rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rendered::DocsRedactor;

    #[test]
    fn placeholders_are_not_indexed() {
        let r = DocsRedactor::default();
        assert_eq!(clean("a «redacted:token» b"), "a b");
        assert_eq!(clean("«redacted:x"), "");
        assert_eq!(clean("/users/{id}"), "/users/{id}");
        assert_eq!(optional(Some(&r.render("x «redacted:t»"))), "x");
        assert_eq!(clean(&text_of("<p>a &amp; <em>b</em></p>\n")), "a & b");
    }
}
