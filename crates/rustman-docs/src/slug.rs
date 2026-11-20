//! Stable ids for pages and anchors (design D5): a slug of the path, with
//! a numeric suffix on collision assigned in order.

use std::collections::BTreeSet;

/// Hands out unique slugs in the order they are asked for.
#[derive(Debug, Default)]
pub struct Slugs {
    used: BTreeSet<String>,
}

impl Slugs {
    /// A slug of `text` not handed out before: `users-list`, then
    /// `users-list-2`, `users-list-3`...
    pub fn next(&mut self, text: &str) -> String {
        let base = slugify(text);
        let mut candidate = base.clone();
        let mut n = 2;
        while !self.used.insert(candidate.clone()) {
            candidate = format!("{base}-{n}");
            n += 1;
        }
        candidate
    }
}

/// Lowercase ASCII letters and digits, with every other run of characters
/// as one `-`; `page` when nothing is left.
pub fn slugify(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_end_matches('-');
    if trimmed.is_empty() {
        "page".to_owned()
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_stable_and_unique() {
        assert_eq!(slugify("users/List Users.v2"), "users-list-users-v2");
        assert_eq!(slugify("__"), "page");
        assert_eq!(slugify("Ünïcode/ok"), "n-code-ok");
        let mut slugs = Slugs::default();
        assert_eq!(slugs.next("a/b"), "a-b");
        assert_eq!(slugs.next("a-b"), "a-b-2");
        assert_eq!(slugs.next("A B"), "a-b-3");
        assert_eq!(slugs.next("c"), "c");
    }
}
