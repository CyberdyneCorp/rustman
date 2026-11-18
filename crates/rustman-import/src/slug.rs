//! Deterministic names: file slugs with collision suffixes, and variable
//! names for extracted secrets.

use std::collections::HashSet;

/// The longest slug, before any collision suffix is counted.
pub const MAX_SLUG: usize = 64;

/// The longest generated path, relative to the workspace root, in
/// characters. Windows' `MAX_PATH` is 260; this leaves 80 for the
/// workspace root itself and the `.rmtmp` suffix of a staged file.
pub const MAX_PATH_LEN: usize = 180;

/// The most `/`-separated parts a generated path may have (folders plus
/// the file name).
pub const MAX_PATH_DEPTH: usize = 20;

/// Why the generated relative path `path` is too long or too deep, or
/// `None` when it fits within [`MAX_PATH_LEN`] and [`MAX_PATH_DEPTH`].
/// The message shows `path` shortened, never kilobytes of it.
pub fn path_limit_error(path: &str) -> Option<String> {
    let len = path.chars().count();
    let depth = path.split('/').count();
    let what = if len > MAX_PATH_LEN {
        format!("{len} characters, limit {MAX_PATH_LEN}")
    } else if depth > MAX_PATH_DEPTH {
        format!("{depth} levels, limit {MAX_PATH_DEPTH}")
    } else {
        return None;
    };
    Some(format!(
        "generated path too long ({what}): {}; shorten folder or request names, \
         flatten the folders or import with a shorter --into",
        abbreviate(path)
    ))
}

/// `path` cut to its first 40 and last 30 characters around `...` when
/// it is longer than 80.
pub fn abbreviate(path: &str) -> String {
    let chars: Vec<char> = path.chars().collect();
    if chars.len() <= 80 {
        return path.to_owned();
    }
    let head: String = chars[..40].iter().collect();
    let tail: String = chars[chars.len() - 30..].iter().collect();
    format!("{head}...{tail}")
}

/// Names Windows refuses as file names, whatever the extension.
const RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// The slug of `name`: lowercase ASCII letters and digits, `-` between
/// words, at most [`MAX_SLUG`] characters; `fallback` when nothing is left.
pub fn slug(name: &str, fallback: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.truncate(MAX_SLUG);
    let trimmed = out.trim_end_matches('-');
    let base = if trimmed.is_empty() {
        fallback
    } else {
        trimmed
    };
    if RESERVED.contains(&base) {
        format!("{base}-item")
    } else {
        base.to_owned()
    }
}

/// `base` with the collision suffix `-n` (`n >= 2`), kept within
/// [`MAX_SLUG`] characters.
pub fn suffixed(base: &str, n: usize) -> String {
    let suffix = format!("-{n}");
    let keep = MAX_SLUG.saturating_sub(suffix.len()).min(base.len());
    format!("{}{suffix}", base[..keep].trim_end_matches('-'))
}

/// Hands out unique slugs in source order: the first `get-user`, then
/// `get-user-2`, `get-user-3`.
#[derive(Debug, Default)]
pub struct Slugs {
    taken: HashSet<String>,
}

impl Slugs {
    pub fn unique(&mut self, name: &str, fallback: &str) -> String {
        let base = slug(name, fallback);
        let mut candidate = base.clone();
        let mut n = 1;
        while self.taken.contains(&candidate) {
            n += 1;
            candidate = suffixed(&base, n);
        }
        self.taken.insert(candidate.clone());
        candidate
    }
}

/// A variable name for `label`: lowercase ASCII letters, digits and `_`.
pub fn var_name(label: &str, fallback: &str) -> String {
    let mut out = String::new();
    for c in label.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('_') {
            out.push('_');
        }
    }
    let trimmed = out.trim_end_matches('_');
    if trimmed.is_empty() {
        fallback.to_owned()
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_lowercase_ascii_with_dashes() {
        assert_eq!(slug("Get User", "x"), "get-user");
        assert_eq!(slug("get user!", "x"), "get-user");
        assert_eq!(slug("  Billing  API (v2) ", "x"), "billing-api-v2");
        assert_eq!(slug("Café ☕ crème", "x"), "caf-cr-me");
        assert_eq!(slug("!!!", "request"), "request");
        assert_eq!(slug("CON", "x"), "con-item");
        assert_eq!(slug(&"a".repeat(80), "x").len(), MAX_SLUG);
        assert_eq!(slug(&format!("{}-b", "a".repeat(63)), "x"), "a".repeat(63));
    }

    #[test]
    fn collisions_get_suffixes_in_source_order() {
        let mut slugs = Slugs::default();
        assert_eq!(slugs.unique("Get User", "x"), "get-user");
        assert_eq!(slugs.unique("get user!", "x"), "get-user-2");
        assert_eq!(slugs.unique("GET USER", "x"), "get-user-3");
        let long = "b".repeat(70);
        assert_eq!(slugs.unique(&long, "x").len(), MAX_SLUG);
        let second = slugs.unique(&long, "x");
        assert_eq!(second.len(), MAX_SLUG);
        assert!(second.ends_with("-2"));
    }

    #[test]
    fn paths_beyond_the_limits_are_refused_with_a_short_message() {
        assert_eq!(path_limit_error("billing-api/users/list.rman.yaml"), None);
        let long = format!("{}/x.rman.yaml", vec!["a".repeat(64); 60].join("/"));
        let message = path_limit_error(&long).unwrap();
        assert!(
            message.starts_with("generated path too long ("),
            "{message}"
        );
        assert!(
            message.contains(&format!("limit {MAX_PATH_LEN}")),
            "{message}"
        );
        assert!(message.len() < 300, "{message}");
        assert!(message.contains("..."), "{message}");
        let deep = format!("{}/x.rman.yaml", vec!["a"; MAX_PATH_DEPTH].join("/"));
        let message = path_limit_error(&deep).unwrap();
        assert!(
            message.contains(&format!("limit {MAX_PATH_DEPTH})")),
            "{message}"
        );
        assert_eq!(abbreviate("short"), "short");
        assert_eq!(abbreviate(&"é".repeat(100)).chars().count(), 73);
    }

    #[test]
    fn variable_names_use_underscores() {
        assert_eq!(var_name("X-API-Key", "v"), "x_api_key");
        assert_eq!(var_name("--", "api_key"), "api_key");
    }
}
