//! Lenient decoders for taint checks: a captured value may hold a piece of
//! a secret in an encoded form (`x%2Fx%2F`, `q\"r\"`, `&amp;en`) that no
//! raw window matches. [`variants`] gives the value decoded each common way,
//! so its pieces can be checked against the raw windows of every secret.
//! Invalid sequences are left as they are.

/// `text` percent-decoded (`+` as a space, and as is), JSON-unescaped and
/// HTML-unescaped, each only when it differs from `text`.
pub(super) fn variants(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for decoded in [
        percent_decode(text, false),
        percent_decode(text, true),
        json_unescape(text),
        html_unescape(text),
    ] {
        if decoded != text && !out.contains(&decoded) {
            out.push(decoded);
        }
    }
    out
}

fn percent_decode(text: &str, plus_is_space: bool) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok());
        match (bytes[i], hex.and_then(|h| u8::from_str_radix(h, 16).ok())) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
                continue;
            }
            (b'+', _) if plus_is_space => out.push(b' '),
            (byte, _) => out.push(byte),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn json_unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('\\') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let (decoded, used) = json_escape(rest);
        out.push_str(&decoded);
        rest = &rest[used..];
    }
    out.push_str(rest);
    out
}

/// The escape at the start of `text` (which starts with `\`) decoded, and
/// how many bytes it took; an invalid one is kept as its backslash.
fn json_escape(text: &str) -> (String, usize) {
    let simple = match text.as_bytes().get(1) {
        Some(b'"') => Some('"'),
        Some(b'\\') => Some('\\'),
        Some(b'/') => Some('/'),
        Some(b'n') => Some('\n'),
        Some(b't') => Some('\t'),
        Some(b'r') => Some('\r'),
        Some(b'b') => Some('\u{8}'),
        Some(b'f') => Some('\u{c}'),
        _ => None,
    };
    if let Some(ch) = simple {
        return (ch.to_string(), 2);
    }
    let units = unicode_units(text);
    match char::decode_utf16(units.iter().copied()).next() {
        Some(Ok(ch)) if !units.is_empty() => {
            let used = if ch.len_utf16() == 2 { 12 } else { 6 };
            (ch.to_string(), used)
        }
        _ => ("\\".to_owned(), 1),
    }
}

/// The UTF-16 units of one `\uXXXX` escape, or two for a surrogate pair.
fn unicode_units(text: &str) -> Vec<u16> {
    let unit = |at: usize| {
        let hex = text.get(at..at + 6)?.strip_prefix("\\u")?;
        u16::from_str_radix(hex, 16).ok()
    };
    match (unit(0), unit(6)) {
        (Some(high @ 0xD800..=0xDBFF), Some(low)) => vec![high, low],
        (Some(one), _) => vec![one],
        (None, _) => Vec::new(),
    }
}

fn html_unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        match entity(rest) {
            Some((ch, used)) => {
                out.push(ch);
                rest = &rest[used..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The entity at the start of `text` (which starts with `&`), and how many
/// bytes it took.
fn entity(text: &str) -> Option<(char, usize)> {
    let end = text.bytes().take(12).position(|b| b == b';')?;
    let name = &text[1..end];
    let ch = match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        _ => {
            let number = name.strip_prefix('#')?;
            let code = match number.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => number.parse().ok()?,
            };
            char::from_u32(code)?
        }
    };
    Some((ch, end + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_encoding_is_decoded_leniently() {
        assert_eq!(percent_decode("x%2Fx%2f+%zz%", false), "x/x/+%zz%");
        assert_eq!(percent_decode("y+z", true), "y z");
        assert_eq!(
            json_unescape(r#"q\"r\/\u00e9\ud83d\ude00\x\u12"#),
            "q\"r/é😀\\x\\u12"
        );
        assert_eq!(
            html_unescape("&amp;en&lt;ok&#47;&#x27;&bogus;&"),
            "&en<ok/'&bogus;&"
        );
        assert_eq!(variants("plain"), Vec::<String>::new());
    }
}
