//! How a captured body is displayed (spec: Binary-safe bodies): pretty JSON,
//! form fields, plain text, or a hex dump when the bytes are not UTF-8.
//! Display never changes the stored bytes.

use std::fmt::Write as _;

/// A body prepared for display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BodyView {
    Empty,
    /// Pretty-printed JSON.
    Json(String),
    /// Decoded `application/x-www-form-urlencoded` fields, in order.
    Form(Vec<(String, String)>),
    Text(String),
    /// A hex dump (see [`hex_dump`]).
    Hex(String),
}

/// The view of `body` sent with `content_type`.
pub fn view(content_type: Option<&str>, body: &[u8]) -> BodyView {
    if body.is_empty() {
        return BodyView::Empty;
    }
    let Ok(text) = std::str::from_utf8(body) else {
        return BodyView::Hex(hex_dump(body));
    };
    let media = content_type
        .and_then(|ct| ct.split(';').next())
        .map(|m| m.trim().to_ascii_lowercase())
        .unwrap_or_default();
    if media == "application/x-www-form-urlencoded" {
        return BodyView::Form(form_fields(text));
    }
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(value) => BodyView::Json(serde_json::to_string_pretty(&value).unwrap_or_default()),
        Err(_) => BodyView::Text(text.to_owned()),
    }
}

fn form_fields(text: &str) -> Vec<(String, String)> {
    text.split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            (decode(name), decode(value))
        })
        .collect()
}

/// Percent-decoding with `+` as space; invalid escapes are kept as written.
fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let escaped = (bytes[i] == b'%')
            .then(|| text.get(i + 1..i + 3))
            .flatten()
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match (bytes[i], escaped) {
            (_, Some(byte)) => {
                out.push(byte);
                i += 3;
                continue;
            }
            (b'+', None) => out.push(b' '),
            (byte, None) => out.push(byte),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 16 bytes per line: offset, hex bytes and the printable ASCII column.
pub fn hex_dump(bytes: &[u8]) -> String {
    let mut out = String::new();
    for (line, chunk) in bytes.chunks(16).enumerate() {
        let _ = write!(out, "{:08x} ", line * 16);
        for byte in chunk {
            let _ = write!(out, " {byte:02x}");
        }
        let pad = (16 - chunk.len()) * 3;
        let ascii: String = chunk
            .iter()
            .map(|&b| {
                if b.is_ascii_graphic() || b == b' ' {
                    b as char
                } else {
                    '.'
                }
            })
            .collect();
        let _ = writeln!(out, "{:pad$}  |{ascii}|", "");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_utf8_bodies_are_shown_as_hex() {
        let BodyView::Hex(dump) = view(Some("text/plain"), &[0xFF, 0xFE, 0x00]) else {
            panic!("expected hex");
        };
        assert!(dump.starts_with("00000000  ff fe 00 "), "{dump}");
        assert!(dump.trim_end().ends_with("|...|"), "{dump}");
    }

    #[test]
    fn json_is_pretty_printed() {
        assert_eq!(
            view(Some("application/json"), br#"{"a":1}"#),
            BodyView::Json("{\n  \"a\": 1\n}".into())
        );
    }

    #[test]
    fn forms_are_decoded_in_order() {
        let fields = view(
            Some("application/x-www-form-urlencoded; charset=utf-8"),
            b"b=2&a=x+y%21&bad=%zz",
        );
        assert_eq!(
            fields,
            BodyView::Form(vec![
                ("b".into(), "2".into()),
                ("a".into(), "x y!".into()),
                ("bad".into(), "%zz".into()),
            ])
        );
    }

    #[test]
    fn other_text_and_empty_bodies() {
        assert_eq!(view(None, b"hello"), BodyView::Text("hello".into()));
        assert_eq!(view(None, b""), BodyView::Empty);
    }

    #[test]
    fn hex_dump_wraps_at_16_bytes() {
        let dump = hex_dump(&[b'a'; 17]);
        let lines: Vec<&str> = dump.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[1].starts_with("00000010  61 "));
        assert!(lines[1].ends_with("|a|"));
    }
}
