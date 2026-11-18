//! Values quoted in assertion messages. A value is not escaped or cut where
//! the message is built: [`quoted`] and [`plain`] only mark it, and
//! [`finish`] redacts the whole message first, then quotes and cuts each
//! marked value. So the redactor always sees the value as received (never a
//! `Debug`-escaped form of an escaped echo), a cut never leaves part of a
//! secret, and it never splits a `«redacted:<name>»` placeholder.

use crate::redact::Redactor;

/// How many characters of a value a message shows.
pub(crate) const MAX_SHOWN: usize = 200;

/// Starts a value shown quoted, as Rust's `Debug` quotes a string.
const QUOTED: char = '\u{E000}';
/// Starts a value shown as is.
const PLAIN: char = '\u{E001}';
/// Ends a marked value.
const END: char = '\u{E002}';

/// `text`, to be shown quoted and cut to [`MAX_SHOWN`] characters.
pub(crate) fn quoted(text: &str) -> String {
    mark(QUOTED, text)
}

/// `text`, to be shown as is, cut to [`MAX_SHOWN`] characters.
pub(crate) fn plain(text: &str) -> String {
    mark(PLAIN, text)
}

fn mark(kind: char, text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 6);
    out.push(kind);
    out.extend(text.chars().filter(|c| !matches!(*c, QUOTED | PLAIN | END)));
    out.push(END);
    out
}

/// `text` with every secret `redactor` knows replaced, then each marked
/// value quoted and cut.
pub(crate) fn finish(text: &str, redactor: &Redactor) -> String {
    let redacted = redactor.text(text);
    let mut rest: &str = &redacted;
    let mut out = String::with_capacity(rest.len());
    while let Some(start) = rest.find([QUOTED, PLAIN]) {
        out.push_str(&rest[..start]);
        let quote = rest[start..].starts_with(QUOTED);
        let value = &rest[start + QUOTED.len_utf8()..];
        let end = value.find(END).unwrap_or(value.len());
        render(&mut out, &value[..end], quote);
        rest = value.get(end + END.len_utf8()..).unwrap_or_default();
    }
    out.push_str(rest);
    out
}

fn render(out: &mut String, value: &str, quote: bool) {
    let shown = shorten(value);
    if quote {
        out.push_str(&format!("{shown:?}"));
    } else {
        out.push_str(shown);
    }
    if shown.len() < value.len() {
        out.push('…');
    }
}

/// The first [`MAX_SHOWN`] characters of `value`, extended to the end of a
/// placeholder the cut would split.
fn shorten(value: &str) -> &str {
    let Some((cut, _)) = value.char_indices().nth(MAX_SHOWN) else {
        return value;
    };
    let head = &value[..cut];
    let open = head.rfind('«');
    let inside = open.is_some_and(|open| head.rfind('»').is_none_or(|close| close < open));
    match open.filter(|_| inside) {
        Some(open) => {
            let close = value[open..].find('»').map(|c| open + c + '»'.len_utf8());
            &value[..close.unwrap_or(value.len())]
        }
        None => head,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::Secret;

    fn redactor(value: &str) -> Redactor {
        Redactor::new().with(&Secret::new("tok", value.to_owned()))
    }

    #[test]
    fn values_are_quoted_and_cut_after_redaction() {
        let r = Redactor::new();
        assert_eq!(
            finish(&format!("got {}", quoted("a\"b")), &r),
            r#"got "a\"b""#
        );
        let long = "x".repeat(300);
        let shown = finish(&plain(&long), &r);
        assert_eq!(shown.chars().count(), MAX_SHOWN + 1);
        assert!(shown.ends_with('…'));
    }

    #[test]
    fn escaped_echoes_are_redacted_before_quoting() {
        let r = redactor("cafeCANARYé99\"q");
        let echo = r#"{"t":"cafeCANARY\u00e999\"q"}"#;
        let shown = finish(&quoted(echo), &r);
        assert!(!shown.contains("CANARY"), "{shown}");
        assert!(shown.contains("«redacted:tok»"), "{shown}");
    }

    #[test]
    fn a_cut_never_leaves_part_of_a_secret_or_placeholder() {
        let r = redactor("simpleCANARY");
        for pad in 185..200 {
            let body = format!("{}simpleCANARY tail", "A".repeat(pad));
            let shown = finish(&quoted(&body), &r);
            assert!(!shown.contains("simple"), "{pad}: {shown}");
            assert!(shown.contains("«redacted:tok»"), "{pad}: {shown}");
        }
    }
}
