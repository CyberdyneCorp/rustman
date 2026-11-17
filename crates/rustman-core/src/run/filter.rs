//! `rustman run --filter <tag:name | glob>`.

use crate::workspace::RequestRef;

/// Which requests a run keeps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Filter {
    /// `tag:<name>`: requests whose `tags` include `name`.
    Tag(String),
    /// Any other value: a glob over the request id (`users/get-user`) or its
    /// file path (`users/get-user.rman.yaml`). `*` and `?` stay within one
    /// path segment; `**` crosses segments.
    Glob(String),
}

impl Filter {
    pub fn parse(text: &str) -> Self {
        match text.strip_prefix("tag:") {
            Some(tag) => Self::Tag(tag.to_owned()),
            None => Self::Glob(text.to_owned()),
        }
    }

    pub fn matches(&self, request: &RequestRef<'_>) -> bool {
        match self {
            Self::Tag(tag) => request.request().tags().iter().any(|t| t == tag),
            Self::Glob(pattern) => {
                let path = request.entry.path.to_string_lossy().replace('\\', "/");
                glob_matches(pattern, &request.entry.id()) || glob_matches(pattern, &path)
            }
        }
    }
}

/// Whether `text` matches the glob `pattern` (`*`, `**`, `?`; `/` separates
/// segments).
pub fn glob_matches(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    matches_from(&pattern, &text)
}

fn matches_from(pattern: &[char], text: &[char]) -> bool {
    match pattern {
        [] => text.is_empty(),
        ['*', '*', rest @ ..] => {
            // `**/` also matches no folder at all.
            let rest_no_slash = rest.strip_prefix(&['/']).unwrap_or(rest);
            (0..=text.len())
                .any(|i| matches_from(rest, &text[i..]) || matches_from(rest_no_slash, &text[i..]))
        }
        ['*', rest @ ..] => {
            let segment = text.iter().position(|c| *c == '/').unwrap_or(text.len());
            (0..=segment).any(|i| matches_from(rest, &text[i..]))
        }
        ['?', rest @ ..] => {
            text.first().is_some_and(|c| *c != '/') && matches_from(rest, &text[1..])
        }
        [p, rest @ ..] => text.first() == Some(p) && matches_from(rest, &text[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_prefix_selects_a_tag_filter() {
        assert_eq!(Filter::parse("tag:smoke"), Filter::Tag("smoke".into()));
        assert_eq!(Filter::parse("users/*"), Filter::Glob("users/*".into()));
    }

    #[test]
    fn single_star_stays_in_one_segment() {
        assert!(glob_matches("users/*", "users/get-user"));
        assert!(!glob_matches("users/*", "users/admin/list"));
        assert!(glob_matches("*-user", "get-user"));
        assert!(!glob_matches("*", "a/b"));
    }

    #[test]
    fn double_star_crosses_segments() {
        assert!(glob_matches("users/**", "users/admin/list"));
        assert!(glob_matches("**/list", "users/admin/list"));
        assert!(glob_matches("**/list", "list"));
        assert!(glob_matches("**", "a/b/c"));
    }

    #[test]
    fn question_mark_matches_one_character() {
        assert!(glob_matches("v?/ping", "v1/ping"));
        assert!(!glob_matches("v?/ping", "v10/ping"));
        assert!(!glob_matches("a?b", "a/b"));
    }

    #[test]
    fn literals_must_match_exactly() {
        assert!(glob_matches("users/get-user", "users/get-user"));
        assert!(!glob_matches("users/get", "users/get-user"));
    }
}
