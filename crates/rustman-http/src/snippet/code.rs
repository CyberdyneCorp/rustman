//! How each language writes strings, constants and JSON values: the quoting
//! filters the templates use.

use rustman_core::vars::{Segment, Template};
use serde_json::Value;

/// The string syntax of a snippet language.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Syntax {
    /// POSIX shell words (curl, httpie, wget).
    Shell,
    PowerShell,
    Python,
    Js,
    Go,
    Rust,
    CSharp,
}

impl Syntax {
    /// `text` as a string literal.
    pub fn literal(self, text: &str) -> String {
        match self {
            Self::Shell => shell_word(text),
            Self::PowerShell => powershell_literal(text),
            Self::Python | Self::Js => serde_json::to_string(text).expect("strings serialize"),
            // C# rejects these line terminators inside a regular string literal.
            Self::CSharp => serde_json::to_string(text)
                .expect("strings serialize")
                .replace('\u{2028}', "\\u2028")
                .replace('\u{2029}', "\\u2029")
                .replace('\u{0085}', "\\u0085"),
            // Go refuses a byte order mark anywhere but at the start of a file.
            Self::Go => serde_json::to_string(text)
                .expect("strings serialize")
                .replace('\u{feff}', "\\ufeff"),
            Self::Rust => format!("{text:?}"),
        }
    }

    /// A reference to the constant `name`.
    fn constant(self, name: &str) -> String {
        match self {
            Self::PowerShell => format!("${name}"),
            _ => name.to_owned(),
        }
    }

    /// `parts` joined into one string expression.
    fn concat(self, parts: &[String]) -> String {
        match self {
            Self::Rust => format!("[{}].concat()", parts.join(", ")),
            _ => parts.join(" + "),
        }
    }

    /// `text` as an expression: a literal, or in placeholder mode a
    /// concatenation of literals and constants for its `{{name}}` markers.
    /// Shell tools keep the markers inside the literal.
    pub fn expression(self, text: &str, placeholders: bool, grouped: bool) -> String {
        if !placeholders || self == Self::Shell {
            return self.literal(text);
        }
        let Ok(template) = Template::parse(text) else {
            return self.literal(text);
        };
        let parts: Vec<String> = template
            .segments()
            .iter()
            .map(|segment| match segment {
                Segment::Var(name) => self.constant(&constant_name(name)),
                Segment::Literal(text) => self.literal(text),
                Segment::Dynamic(dynamic) => self.literal(&format!("{{{{{dynamic}}}}}")),
                Segment::Secret(reference) => self.literal(&format!("{{{{secret:{reference}}}}}")),
            })
            .collect();
        match parts.as_slice() {
            [] => self.literal(""),
            [one] => one.clone(),
            _ if grouped && self != Self::Rust => format!("({})", self.concat(&parts)),
            _ => self.concat(&parts),
        }
    }

    /// A header value as an expression that sends its UTF-8 bytes, as the
    /// engine does. Python's and Node's HTTP clients write `str` header
    /// values as Latin-1 (or refuse them), and reqwest refuses a `&str`
    /// holding non-ASCII, so a non-ASCII value goes in as bytes there.
    pub fn header_value(self, text: &str, placeholders: bool) -> String {
        if !needs_bytes(text) {
            return self.expression(text, placeholders, false);
        }
        match self {
            Self::Python => format!("{}.encode()", self.expression(text, placeholders, true)),
            Self::Js => format!(
                "String.fromCharCode(...new TextEncoder().encode({}))",
                self.expression(text, placeholders, false)
            ),
            Self::Rust => format!("{}.as_bytes()", self.expression(text, placeholders, true)),
            _ => self.expression(text, placeholders, false),
        }
    }

    /// A JSON value as a Python or JavaScript literal, with its strings
    /// written as [`Syntax::expression`]s.
    pub fn json(self, value: &Value, placeholders: bool) -> String {
        match value {
            Value::Null => self.keyword("None", "null"),
            Value::Bool(true) => self.keyword("True", "true"),
            Value::Bool(false) => self.keyword("False", "false"),
            Value::Number(n) => n.to_string(),
            Value::String(s) => self.expression(s, placeholders, false),
            Value::Array(items) => {
                let items: Vec<String> = items.iter().map(|v| self.json(v, placeholders)).collect();
                format!("[{}]", items.join(", "))
            }
            Value::Object(map) => {
                let entries: Vec<String> = map
                    .iter()
                    .map(|(k, v)| {
                        format!(
                            "{}: {}",
                            self.key(k, placeholders),
                            self.json(v, placeholders)
                        )
                    })
                    .collect();
                format!("{{{}}}", entries.join(", "))
            }
        }
    }

    fn keyword(self, python: &str, js: &str) -> String {
        match self {
            Self::Python => python.to_owned(),
            _ => js.to_owned(),
        }
    }

    /// An object key; a computed key in JavaScript when it holds a marker.
    fn key(self, key: &str, placeholders: bool) -> String {
        let expression = self.expression(key, placeholders, false);
        if self == Self::Js && expression != self.literal(key) {
            format!("[{expression}]")
        } else {
            expression
        }
    }
}

/// `text` as one POSIX shell word: single-quoted unless it is made only of
/// characters no shell reads specially. A word starting with `=` is quoted
/// too, since zsh expands `=name` to the path of command `name`.
pub(crate) fn shell_word(text: &str) -> String {
    let word = shell_escape::unix::escape(text.into());
    if word.starts_with('=') {
        format!("'{word}'")
    } else {
        word.into_owned()
    }
}

/// Whether header value `text` holds non-ASCII text of its own. A
/// `«redacted:<name>»` placeholder does not count: it stands for a secret
/// and is shown as written.
pub(crate) fn needs_bytes(text: &str) -> bool {
    let mut rest = text;
    while let Some(start) = rest.find("«redacted:") {
        if !rest[..start].is_ascii() {
            return true;
        }
        let after = &rest[start..];
        match after.find('»') {
            Some(end) => rest = &after[end + '»'.len_utf8()..],
            None => return true,
        }
    }
    !rest.is_ascii()
}

/// A variable name as a constant: upper case, with every character that is
/// not a letter or digit replaced by `_`.
pub(crate) fn constant_name(name: &str) -> String {
    let mut constant: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    if !constant.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
        constant.insert(0, '_');
    }
    constant
}

/// A single-quoted PowerShell string. PowerShell also reads the typographic
/// single quotes as quotes, so those are doubled too.
fn powershell_literal(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('\'');
    for c in text.chars() {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            out.push(c);
        }
        out.push(c);
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn literals_escape_what_each_language_needs() {
        let text = "a\"b'c\\d\n";
        assert_eq!(Syntax::Shell.literal(text), "'a\"b'\\''c\\d\n'");
        assert_eq!(Syntax::PowerShell.literal("it's ’"), "'it''s ’’'");
        assert_eq!(Syntax::Python.literal(text), r#""a\"b'c\\d\n""#);
        assert_eq!(Syntax::Rust.literal(text), r#""a\"b'c\\d\n""#);
        assert_eq!(Syntax::Go.literal("\u{1}"), r#""\u0001""#);
        assert_eq!(Syntax::Go.literal("a\u{feff}"), r#""a\ufeff""#);
        assert_eq!(Syntax::Shell.literal("=ls"), "'=ls'");
        assert_eq!(Syntax::Shell.literal("$(id)`id`!!"), r"'$(id)`id`'\!''\!''");
    }

    /// Regression: C# refuses U+0085, U+2028 and U+2029 inside a regular
    /// string literal (they are line terminators there), so the snippet did
    /// not compile.
    #[test]
    fn csharp_escapes_its_line_terminators() {
        assert_eq!(
            Syntax::CSharp.literal("a\u{85}b\u{2028}c\u{2029}d\n"),
            r#""a\u0085b\u2028c\u2029d\n""#
        );
        assert_eq!(Syntax::Js.literal("\u{2028}"), "\"\u{2028}\"");
    }

    #[test]
    fn markers_become_constants_in_code_only() {
        let url = "{{base_url}}/users/{{id}}";
        assert_eq!(
            Syntax::Python.expression(url, true, false),
            r#"BASE_URL + "/users/" + ID"#
        );
        assert_eq!(
            Syntax::Python.expression(url, true, true),
            r#"(BASE_URL + "/users/" + ID)"#
        );
        assert_eq!(
            Syntax::Rust.expression(url, true, true),
            r#"[BASE_URL, "/users/", ID].concat()"#
        );
        assert_eq!(Syntax::PowerShell.expression("{{id}}", true, false), "$ID");
        assert_eq!(
            Syntax::Shell.expression(url, true, false),
            format!("'{url}'")
        );
        assert_eq!(
            Syntax::Python.expression(url, false, false),
            format!("\"{url}\"")
        );
    }

    #[test]
    fn non_ascii_header_values_are_sent_as_utf8_bytes() {
        assert_eq!(
            Syntax::Python.header_value("café", false),
            r#""café".encode()"#
        );
        assert_eq!(
            Syntax::Js.header_value("café", false),
            r#"String.fromCharCode(...new TextEncoder().encode("café"))"#
        );
        assert_eq!(
            Syntax::Rust.header_value("café", false),
            r#""café".as_bytes()"#
        );
        assert_eq!(Syntax::Go.header_value("café", false), r#""café""#);
        assert_eq!(Syntax::Python.header_value("plain", false), r#""plain""#);
        let redacted = "Bearer «redacted:api_token»";
        assert_eq!(
            Syntax::Js.header_value(redacted, false),
            format!("{redacted:?}")
        );
        assert!(needs_bytes("«redacted:a» é"));
        assert!(needs_bytes("«redacted:a"));
        assert_eq!(
            Syntax::Python.header_value("é {{v}}", true),
            r#"("é " + V).encode()"#
        );
    }

    #[test]
    fn json_values_become_language_literals() {
        let value = json!({"a": [1, true, null], "{{k}}": "x{{v}}"});
        assert_eq!(
            Syntax::Python.json(&value, true),
            r#"{"a": [1, True, None], K: "x" + V}"#
        );
        assert_eq!(
            Syntax::Js.json(&value, true),
            r#"{"a": [1, true, null], [K]: "x" + V}"#
        );
    }

    #[test]
    fn constant_names_are_upper_snake_case() {
        assert_eq!(constant_name("base-url"), "BASE_URL");
        assert_eq!(constant_name("user.id"), "USER_ID");
        assert_eq!(constant_name("2fa"), "_2FA");
    }
}
