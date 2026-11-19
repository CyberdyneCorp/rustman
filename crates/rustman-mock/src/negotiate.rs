//! Content negotiation (spec: Content negotiation): the response media
//! type is the documented one the `Accept` header ranks highest. With no
//! `Accept` (or only `*/*`) the first JSON media type wins, else the first.

use rustman_explore::openapi::model::MediaType;
use rustman_explore::synth::is_json_media_type;

/// One `Accept` range with its quality.
#[derive(Clone, Debug, PartialEq)]
struct Range {
    essence: String,
    quality: f32,
}

/// The index in `content` of the media type to answer with; `None` when
/// none is acceptable. `content` must not be empty.
pub fn choose(accept: Option<&str>, content: &[MediaType]) -> Option<usize> {
    let ranges = accept.map(parse).unwrap_or_default();
    if ranges.is_empty() || ranges.iter().all(|r| r.essence == "*/*" && r.quality > 0.0) {
        return Some(default_index(content));
    }
    let mut ranked: Vec<&Range> = ranges.iter().filter(|r| r.quality > 0.0).collect();
    ranked.sort_by(|a, b| b.quality.total_cmp(&a.quality));
    ranked.into_iter().find_map(|range| {
        if range.essence == "*/*" {
            return Some(default_index(content));
        }
        content
            .iter()
            .position(|m| matches(&range.essence, &essence(&m.media_type)))
    })
}

/// The first JSON media type, else the first.
fn default_index(content: &[MediaType]) -> usize {
    content
        .iter()
        .position(|m| is_json_media_type(&m.media_type))
        .unwrap_or(0)
}

/// Whether the `Accept` range `wanted` covers the documented `offered`
/// (itself possibly a range such as `application/*`).
pub(crate) fn matches(wanted: &str, offered: &str) -> bool {
    let split = |s: &str| {
        let (kind, sub) = s.split_once('/').unwrap_or((s, "*"));
        (kind.to_owned(), sub.to_owned())
    };
    let (wanted_type, wanted_sub) = split(wanted);
    let (offered_type, offered_sub) = split(offered);
    let part = |a: &str, b: &str| a == "*" || b == "*" || a == b;
    part(&wanted_type, &offered_type) && part(&wanted_sub, &offered_sub)
}

/// The lower-case `type/subtype` of a media type, without parameters.
pub fn essence(media_type: &str) -> String {
    media_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

fn parse(accept: &str) -> Vec<Range> {
    accept
        .split(',')
        .filter_map(|item| {
            let mut parts = item.split(';');
            let essence = essence(parts.next()?);
            if essence.is_empty() {
                return None;
            }
            let quality = parts
                .filter_map(|p| p.trim().strip_prefix("q="))
                .find_map(|q| q.trim().parse::<f32>().ok())
                .unwrap_or(1.0);
            Some(Range { essence, quality })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content(types: &[&str]) -> Vec<MediaType> {
        types
            .iter()
            .map(|t| MediaType {
                media_type: (*t).into(),
                schema: None,
                example: None,
                examples: Vec::new(),
            })
            .collect()
    }

    #[test]
    fn no_accept_or_any_picks_the_first_json_type() {
        let offered = content(&["application/xml", "application/json"]);
        assert_eq!(choose(None, &offered), Some(1));
        assert_eq!(choose(Some("*/*"), &offered), Some(1));
        assert_eq!(choose(Some(""), &offered), Some(1));
        assert_eq!(choose(None, &content(&["text/plain"])), Some(0));
    }

    #[test]
    fn the_best_ranked_documented_type_wins() {
        let offered = content(&["application/json", "application/xml; charset=utf-8"]);
        assert_eq!(choose(Some("application/xml"), &offered), Some(1));
        let ranked = "application/json;q=0.5, application/xml";
        assert_eq!(choose(Some(ranked), &offered), Some(1));
        assert_eq!(choose(Some("application/*"), &offered), Some(0));
        assert_eq!(choose(Some("text/html, */*;q=0.1"), &offered), Some(0));
    }

    #[test]
    fn nothing_acceptable() {
        let offered = content(&["application/json"]);
        assert_eq!(choose(Some("application/xml"), &offered), None);
        assert_eq!(choose(Some("application/json;q=0"), &offered), None);
        assert_eq!(choose(Some("text/*"), &content(&["application/*"])), None);
    }
}
