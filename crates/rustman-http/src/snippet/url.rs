//! URLs, form bodies and file paths as snippets show them. In placeholder
//! mode `{{name}}` markers are kept as they are instead of being encoded.

use std::path::{Component, Path};

use rustman_core::model::{ApiKeyLocation, Auth};
use rustman_core::vars::{
    Resolved, ResolvedFile, ResolvedPair, ResolvedRequest, Segment, Template,
};

use crate::wire;

/// The URL as sent: query (and an API key sent in the query) appended, no
/// fragment.
pub(crate) fn request_url(request: &ResolvedRequest, keep_markers: bool) -> String {
    let mut query = request.query.clone();
    if let Some(Auth::ApiKey {
        name,
        value,
        location: Some(ApiKeyLocation::Query),
    }) = &request.auth
    {
        query.push(ResolvedPair {
            name: Resolved::literal(name.clone()),
            value: Resolved::literal(value.clone()),
        });
    }
    if !keep_markers {
        return wire::uri(&request.url.text, &query)
            .map_or_else(|_| request.url.text.clone(), |uri| uri.to_string());
    }
    let text = &request.url.text;
    let base = text.split_once('#').map_or(text.as_str(), |(base, _)| base);
    if query.is_empty() {
        return base.to_owned();
    }
    let separator = match base {
        b if !b.contains('?') => "?",
        b if b.ends_with(['?', '&']) => "",
        _ => "&",
    };
    format!("{base}{separator}{}", form(&query, true))
}

/// `application/x-www-form-urlencoded` text of `pairs`.
pub(crate) fn form(pairs: &[ResolvedPair], keep_markers: bool) -> String {
    if !keep_markers {
        return wire::form(pairs);
    }
    pairs
        .iter()
        .map(|p| format!("{}={}", encode(&p.name.text), encode(&p.value.text)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Form-encode `text`, keeping `{{name}}` markers.
fn encode(text: &str) -> String {
    let Ok(template) = Template::parse(text) else {
        return form_urlencoded::byte_serialize(text.as_bytes()).collect();
    };
    template
        .segments()
        .iter()
        .map(|segment| match segment {
            Segment::Var(name) => format!("{{{{{name}}}}}"),
            Segment::Literal(text) => form_urlencoded::byte_serialize(text.as_bytes()).collect(),
            Segment::Dynamic(dynamic) => format!("{{{{{dynamic}}}}}"),
        })
        .collect()
}

/// A file as a snippet names it: relative to the workspace root, with `/`
/// separators, when `root` is known; otherwise as written.
pub(crate) fn file_path(file: &ResolvedFile, root: Option<&Path>) -> String {
    let relative = root.and_then(|root| file.path.strip_prefix(root).ok());
    match relative {
        Some(relative) => relative
            .components()
            .filter_map(|c| match c {
                Component::Normal(part) => Some(part.to_string_lossy()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("/"),
        None => file.raw.clone(),
    }
}

/// The name a multipart file part is sent with: the last path segment.
pub(crate) fn file_name(file: &ResolvedFile) -> String {
    file.raw
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(name: &str, value: &str) -> ResolvedPair {
        ResolvedPair {
            name: Resolved::literal(name),
            value: Resolved::literal(value),
        }
    }

    #[test]
    fn markers_survive_encoding() {
        let pairs = [pair("q", "a b&{{term}}"), pair("{{k}}", "1")];
        assert_eq!(form(&pairs, true), "q=a+b%26{{term}}&{{k}}=1");
        assert_eq!(form(&pairs[1..], false), "%7B%7Bk%7D%7D=1");
    }

    #[test]
    fn workspace_relative_paths_use_slashes() {
        let root = Path::new("/ws");
        let file = ResolvedFile {
            raw: "../files/a.png".into(),
            path: root.join("files").join("a.png"),
        };
        assert_eq!(file_path(&file, Some(root)), "files/a.png");
        assert_eq!(file_path(&file, None), "../files/a.png");
        assert_eq!(file_name(&file), "a.png");
    }
}
