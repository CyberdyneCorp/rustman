//! HTTPie request items (`Name:value`, `name=value`, `name@file`): httpie
//! picks the first, longest separator in an item, so a value starting with
//! `=`, `@` or `:=` would turn a header into a JSON or file-embedding item
//! that reads a local file. Every separator character (`:`, `;`, `=`, `@`)
//! of a name is backslash-escaped, and so is the first character of a value
//! when it is one, so the separator written here is the first one httpie
//! finds and nothing extends it. Later separator characters of a value are
//! plain text to httpie.
//!
//! httpie keeps a backslash before any other character but reads one before
//! a separator character as an escape, so a backslash right before `:`,
//! `;`, `=` or `@` (or at the end of a name) cannot be written: such an item
//! is left out, with a warning.

use super::view::View;

/// The characters httpie's request item separators are made of.
const SEPARATOR_CHARS: [char; 4] = [':', ';', '=', '@'];

/// `text` with its separator characters escaped (every one, or only a
/// leading one), `None` when it holds a backslash right before one, which
/// httpie would read as an escape.
fn escape(text: &str, every: bool) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut after_backslash = false;
    for (i, c) in text.chars().enumerate() {
        if SEPARATOR_CHARS.contains(&c) {
            if after_backslash {
                return None;
            }
            if every || i == 0 {
                out.push('\\');
            }
        }
        after_backslash = c == '\\';
        out.push(c);
    }
    Some(out)
}

/// `name`, separator `separator`, then `value`, escaped.
fn item(name: &str, separator: &str, value: &str) -> Option<String> {
    if name.ends_with('\\') {
        return None;
    }
    Some(format!(
        "{}{separator}{}",
        escape(name, true)?,
        escape(value, false)?
    ))
}

/// A header item; an empty value is written `Name;`, since `Name:` tells
/// httpie to leave the header out.
pub(super) fn header(name: &str, value: &str) -> Option<String> {
    if value.is_empty() {
        item(name, ";", "")
    } else {
        item(name, ":", value)
    }
}

/// A text multipart part: `name=value`.
pub(super) fn text_part(name: &str, value: &str) -> Option<String> {
    item(name, "=", value)
}

/// A file multipart part: `name@path;type=content-type`. httpie splits the
/// unescaped value at `;type=`, so the type is not escaped, only the path.
pub(super) fn file_part(name: &str, path: &str, content_type: &str) -> Option<String> {
    let path = item(name, "@", path)?;
    Some(format!("{path};type={content_type}"))
}

/// Whether httpie reads `method` as a method: only letters. Otherwise it
/// takes the method for the URL and the URL for a request item.
pub(super) fn sends_method(method: &str) -> bool {
    !method.is_empty() && method.bytes().all(|b| b.is_ascii_alphabetic())
}

/// The request items of `view`, and a warning for each item httpie cannot
/// write.
pub(super) fn items(view: &View) -> (Vec<String>, Vec<String>) {
    let mut items = Vec::new();
    let mut warnings = Vec::new();
    let mut push = |item: Option<String>, what: &str, name: &str| {
        match item {
        Some(item) => items.push(item),
        None => warnings.push(format!(
            "snippet: httpie cannot write {what} '{name}' (a backslash before ':', ';', '=' or '@'); it is left out"
        )),
    }
    };
    for h in &view.headers {
        push(header(&h.name, &h.value), "header", &h.name);
    }
    for p in &view.body.parts {
        let item = if p.is_file {
            let content_type = p.content_type.as_deref().unwrap_or_default();
            file_part(&p.name, &p.path, content_type)
        } else {
            text_part(&p.name, &p.value)
        };
        push(item, "part", &p.name);
    }
    (items, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separators_in_names_and_values_are_escaped() {
        assert_eq!(
            header("X-A", "=@/etc/passwd").unwrap(),
            r"X-A:\=@/etc/passwd"
        );
        assert_eq!(
            header("X-A", ":=@/etc/passwd").unwrap(),
            r"X-A:\:=@/etc/passwd"
        );
        assert_eq!(text_part("f", "@/etc/passwd").unwrap(), r"f=\@/etc/passwd");
        assert_eq!(text_part("a=b", "==x").unwrap(), r"a\=b=\==x");
        assert_eq!(header("X-B", r"C:\dir").unwrap(), r"X-B:C:\dir");
    }

    #[test]
    fn empty_headers_use_the_semicolon_form() {
        assert_eq!(header("X-Empty", "").unwrap(), "X-Empty;");
    }

    #[test]
    fn a_backslash_before_a_separator_cannot_be_written() {
        assert_eq!(header("X-A", r"a\=b"), None);
        assert_eq!(text_part(r"name\", "v"), None);
        assert_eq!(text_part(r"x\@/etc/passwd", "v"), None);
        assert_eq!(header("X-A", r"trailing\"), Some(r"X-A:trailing\".into()));
    }

    #[test]
    fn file_parts_keep_their_type() {
        assert_eq!(
            file_part("avatar", "files/a=b.png", "image/png").unwrap(),
            r"avatar@files/a=b.png;type=image/png"
        );
    }

    #[test]
    fn only_letter_methods_are_methods() {
        assert!(sends_method("PURGE"));
        assert!(!sends_method("M-SEARCH"));
        assert!(!sends_method("-x"));
    }
}
