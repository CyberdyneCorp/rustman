//! Text from a remote party (a node, a server) made safe to write to a
//! terminal.

use std::borrow::Cow;

/// `text` with every control character other than newline and tab, and
/// every invisible formatting character (bidi overrides and isolates,
/// zero-width characters), written escaped (`\u{1b}`, `\u{202e}`), so text
/// a remote party controls cannot send escape sequences (OSC window titles
/// or clipboard writes, CSI screen clears) to the terminal, nor reorder or
/// hide what is shown next to it (a reversed address or amount).
pub fn printable(text: &str) -> Cow<'_, str> {
    if !text.chars().any(is_unsafe) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if is_unsafe(c) {
            out.extend(c.escape_unicode());
        } else {
            out.push(c);
        }
    }
    Cow::Owned(out)
}

/// [`printable`] for a value shown on one line (a field of a
/// confirmation): newlines and tabs are escaped too, so the value cannot
/// draw lines that look like other fields.
pub fn printable_line(text: &str) -> Cow<'_, str> {
    if !text.chars().any(|c| is_unsafe(c) || c.is_control()) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if is_unsafe(c) || c.is_control() {
            out.extend(c.escape_unicode());
        } else {
            out.push(c);
        }
    }
    Cow::Owned(out)
}

fn is_unsafe(c: char) -> bool {
    (c.is_control() && c != '\n' && c != '\t') || is_invisible_format(c)
}

/// Unicode format characters that change how text around them is shown:
/// bidi marks, embeddings, overrides and isolates, zero-width spaces and
/// joiners, word joiners and invisible operators, and the byte-order mark.
fn is_invisible_format(c: char) -> bool {
    matches!(
        c,
        '\u{061c}'
            | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{feff}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_sequences_are_escaped_and_plain_text_borrowed() {
        assert!(matches!(printable("geth 1.0\n\tok"), Cow::Borrowed(_)));
        assert_eq!(
            printable("Geth\u{1b}]0;PWNED\u{7}\u{1b}[2J\u{9b}x"),
            "Geth\\u{1b}]0;PWNED\\u{7}\\u{1b}[2J\\u{9b}x"
        );
    }

    #[test]
    fn one_line_values_escape_newlines_and_tabs() {
        // Regression: a chain name with a newline drew a fake field line.
        assert_eq!(
            printable_line("x\n  value        0 ETH\t\u{1b}"),
            "x\\u{a}  value        0 ETH\\u{9}\\u{1b}"
        );
        assert!(matches!(
            printable_line("Sepolia (11155111)"),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn bidi_and_zero_width_characters_are_escaped() {
        // Regression: U+202E (category Cf) passed through, so a token
        // symbol or revert reason could show text reversed.
        assert_eq!(printable("USD\u{202e}CT"), "USD\\u{202e}CT");
        assert_eq!(
            printable("a\u{2066}b\u{2069}c\u{200b}d\u{feff}"),
            "a\\u{2066}b\\u{2069}c\\u{200b}d\\u{feff}"
        );
        assert!(matches!(
            printable("vitalik.eth ünïcode ✓"),
            Cow::Borrowed(_)
        ));
    }
}
