//! File, folder and variable names derived from an OpenAPI document.

use rustman_core::vars::template::is_variable_name;

/// The longest name generated, so paths stay well within OS limits.
const MAX_NAME: usize = 100;

/// Device names Windows refuses as file names, whatever the extension.
const RESERVED: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// `text` in kebab-case: ASCII lower-case words split at camelCase
/// boundaries and at every other character, e.g. `getPetByID` →
/// `get-pet-by-id`.
pub fn kebab(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_ascii_alphanumeric() {
            push_separator(&mut out);
            continue;
        }
        if c.is_ascii_uppercase() && starts_word(&chars, i) {
            push_separator(&mut out);
        }
        out.push(c.to_ascii_lowercase());
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Whether the upper-case letter at `i` starts a new word: after a
/// lower-case letter or digit (`petId`), or as the last capital before a
/// lower-case letter (`HTTPServer`).
fn starts_word(chars: &[char], i: usize) -> bool {
    let Some(&prev) = i.checked_sub(1).and_then(|p| chars.get(p)) else {
        return false;
    };
    let next_lower = chars.get(i + 1).is_some_and(char::is_ascii_lowercase);
    prev.is_ascii_lowercase() || prev.is_ascii_digit() || (prev.is_ascii_uppercase() && next_lower)
}

fn push_separator(out: &mut String) {
    if !out.is_empty() && !out.ends_with('-') {
        out.push('-');
    }
}

/// A file or folder name: `kebab(text)`, or `fallback` when that is empty,
/// shortened, and never a Windows device name.
pub fn file_name(text: &str, fallback: &str) -> String {
    let mut name = kebab(text);
    if name.is_empty() {
        name = fallback.to_owned();
    }
    if name.len() > MAX_NAME {
        name.truncate(MAX_NAME);
        while name.ends_with('-') {
            name.pop();
        }
    }
    if RESERVED.contains(&name.as_str()) {
        name.push_str("-request");
    }
    name
}

/// The request file stem of an operation without `operationId`:
/// `<method>-<path-slug>`, e.g. `get-users-id-posts`.
pub fn method_and_path(method: &str, path: &str) -> String {
    let slug = kebab(path);
    let method = method.to_ascii_lowercase();
    if slug.is_empty() {
        method
    } else {
        format!("{method}-{slug}")
    }
}

/// The variable a parameter becomes: its name, unless that cannot appear
/// inside `{{...}}` (whitespace, braces, or a leading `$`).
pub fn variable(name: &str) -> String {
    if is_variable_name(name) && !name.starts_with('$') {
        return name.to_owned();
    }
    let cleaned: String = name
        .trim_start_matches('$')
        .chars()
        .map(|c| {
            if c.is_whitespace() || c == '{' || c == '}' {
                '_'
            } else {
                c
            }
        })
        .collect();
    if cleaned.is_empty() {
        "param".to_owned()
    } else {
        cleaned
    }
}

/// `base`, or `base-2`, `base-3`, ... : the first not in `taken`.
pub fn unique(base: &str, taken: &[String]) -> String {
    let mut name = base.to_owned();
    let mut n = 1;
    while taken.contains(&name) {
        n += 1;
        name = format!("{base}-{n}");
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kebab_case() {
        for (input, expected) in [
            ("listPets", "list-pets"),
            ("createPet", "create-pet"),
            ("getPetByID", "get-pet-by-id"),
            ("HTTPServerStatus", "http-server-status"),
            ("get_v2Users", "get-v2-users"),
            ("Pet Store", "pet-store"),
            ("  --a__b--  ", "a-b"),
            ("café", "caf"),
            ("", ""),
        ] {
            assert_eq!(kebab(input), expected, "{input}");
        }
    }

    #[test]
    fn fallbacks_and_reserved_names() {
        assert_eq!(
            method_and_path("GET", "/users/{id}/posts"),
            "get-users-id-posts"
        );
        assert_eq!(method_and_path("POST", "/"), "post");
        assert_eq!(file_name("Con", "x"), "con-request");
        assert_eq!(file_name("!!!", "untagged"), "untagged");
        assert_eq!(file_name(&"a".repeat(150), "x").len(), 100);
    }

    #[test]
    fn variables_and_collisions() {
        assert_eq!(variable("petId"), "petId");
        assert_eq!(variable("X-Request-Id"), "X-Request-Id");
        assert_eq!(variable("$filter"), "filter");
        assert_eq!(variable("page size"), "page_size");
        let taken = vec!["a".to_owned(), "a-2".to_owned()];
        assert_eq!(unique("a", &taken), "a-3");
        assert_eq!(unique("b", &taken), "b");
    }
}
