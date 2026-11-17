//! Quoting rules for strings: plain when YAML allows it, then a literal
//! block for multi-line text, then single quotes, then double quotes.

use std::fmt::Write as _;

/// Characters that may not start a plain scalar.
const INDICATORS: &[char] = &[
    '-', '?', ':', ',', '[', ']', '{', '}', '#', '&', '*', '!', '|', '>', '\'', '"', '%', '@', '`',
];

/// Words that YAML 1.1 or 1.2 readers would load as something other than a
/// string (compared in lower case).
const RESERVED: &[&str] = &[
    "null", "~", "true", "false", "yes", "no", "on", "off", "<<", "=", ".inf", "-.inf", "+.inf",
    ".nan",
];

pub(super) fn write_key(out: &mut String, key: &str) {
    if is_plain(key) {
        out.push_str(key);
    } else if is_single_quotable(key) {
        write_single_quoted(out, key);
    } else {
        write_double_quoted(out, key);
    }
}

pub(super) fn write_value(out: &mut String, value: &str, content_width: usize) {
    if is_plain(value) {
        out.push_str(value);
    } else if is_literal_block(value) {
        write_literal_block(out, value, content_width);
    } else if is_single_quotable(value) {
        write_single_quoted(out, value);
    } else {
        write_double_quoted(out, value);
    }
}

fn is_special(c: char) -> bool {
    c.is_control() || matches!(c, '\u{feff}' | '\u{2028}' | '\u{2029}')
}

fn is_plain(s: &str) -> bool {
    let Some(first) = s.chars().next() else {
        return false;
    };
    let has_edge_space = s.starts_with(' ') || s.ends_with(' ');
    !has_edge_space
        && !INDICATORS.contains(&first)
        && !s.chars().any(is_special)
        && !s.contains(": ")
        && !s.contains(" #")
        && !s.ends_with(':')
        && !looks_like_non_string(s)
}

/// True for words and number-like text that a YAML reader might not load as a string.
fn looks_like_non_string(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    if RESERVED.contains(&lower.as_str()) || s.starts_with("---") || s.starts_with("...") {
        return true;
    }
    let unsigned = s.trim_start_matches(['+', '-']);
    let number_start = unsigned.strip_prefix('.').unwrap_or(unsigned);
    number_start.starts_with(|c: char| c.is_ascii_digit()) || s.parse::<f64>().is_ok()
}

fn is_single_quotable(s: &str) -> bool {
    !s.chars().any(is_special)
}

fn is_literal_block(s: &str) -> bool {
    let first_line = s.split('\n').next().unwrap_or_default();
    s.contains('\n')
        && !first_line.is_empty()
        && !first_line.starts_with([' ', '\t'])
        && !s.chars().any(|c| c != '\n' && c != '\t' && is_special(c))
}

fn write_literal_block(out: &mut String, s: &str, width: usize) {
    let body = s.trim_end_matches('\n');
    let trailing = s.len() - body.len();
    out.push_str(match trailing {
        0 => "|-",
        1 => "|",
        _ => "|+",
    });
    for line in body.split('\n') {
        out.push('\n');
        if !line.is_empty() {
            out.extend(std::iter::repeat_n(' ', width));
            out.push_str(line);
        }
    }
    for _ in 1..trailing {
        out.push('\n');
    }
}

fn write_single_quoted(out: &mut String, s: &str) {
    out.push('\'');
    out.push_str(&s.replace('\'', "''"));
    out.push('\'');
}

fn write_double_quoted(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if is_special(c) => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn value(s: &str) -> String {
        let mut out = String::new();
        write_value(&mut out, s, 2);
        out
    }

    #[test]
    fn plain_when_allowed() {
        for s in [
            "Get user",
            "GET",
            "$.items",
            "https://x.io/a?b=c",
            "Bearer {{token}}",
            "é",
        ] {
            assert_eq!(value(s), s);
        }
    }

    #[test]
    fn quotes_when_required() {
        assert_eq!(value("{{base_url}}/users"), "'{{base_url}}/users'");
        assert_eq!(value("no"), "'no'");
        assert_eq!(value("0755"), "'0755'");
        assert_eq!(value("2xx"), "'2xx'");
        assert_eq!(value(""), "''");
        assert_eq!(value("it's: x"), "'it''s: x'");
        assert_eq!(value("a\u{1}"), "\"a\\u0001\"");
        assert_eq!(value(" lead\n"), "\" lead\\n\"");
    }

    #[test]
    fn literal_block_chomping() {
        assert_eq!(value("a\nb"), "|-\n  a\n  b");
        assert_eq!(value("a\nb\n"), "|\n  a\n  b");
        assert_eq!(value("a\n\nb\n\n"), "|+\n  a\n\n  b\n");
    }

    /// Every string, written as a value and as a key, must load back unchanged
    /// with Rustman's parser options.
    #[test]
    fn tricky_strings_round_trip() {
        let cases = [
            "",
            " ",
            "  x",
            "x ",
            "-",
            "- x",
            "-x",
            "--- x",
            "...",
            "?",
            "? x",
            ":",
            "x:",
            "a: b",
            "a #b",
            "#x",
            "&a",
            "*a",
            "!x",
            "|",
            ">",
            "'",
            "\"",
            "%x",
            "@x",
            "`x",
            "null",
            "Null",
            "~",
            "true",
            "False",
            "yes",
            "ON",
            "y",
            "n",
            "<<",
            "=",
            "0",
            "-1",
            "+1",
            "1.5",
            ".5",
            "1e3",
            "0x1F",
            "0o17",
            "0b1",
            "1_000",
            ".inf",
            "-.inf",
            ".nan",
            "NaN",
            "inf",
            "infinity",
            "2001-12-14",
            "12:30",
            "a,b",
            "[a]",
            "{a}",
            "a]b",
            "tab\tin",
            "\ttab",
            "x\n",
            "x\n\n",
            "\nx",
            "a\nb\n\n\n",
            "a\r\nb",
            "  a\nb",
            "a\n  b\n",
            "a\n \nb",
            "\u{feff}x",
            "\u{85}",
            "\u{2028}",
            "\\",
            "\\n",
            "é\u{0}",
            "{{x}}",
            "\\{{x}}",
            "y",
            "n",
            "./a",
            "../a",
            ".5",
            "-.5",
            ".hidden",
            "...x",
            "---x",
        ];
        for case in cases {
            let mut doc = String::from("k:\n  ");
            write_key(&mut doc, case);
            doc.push_str(": ");
            write_value(&mut doc, case, 4);
            doc.push('\n');
            let loaded: serde_json::Value = crate::format::from_yaml(Path::new("t.yaml"), &doc)
                .unwrap_or_else(|e| panic!("{case:?} -> {doc:?}: {e}"));
            let expected = serde_json::json!({ "k": { case: case } });
            assert_eq!(loaded, expected, "document: {doc:?}");
        }
    }
}
