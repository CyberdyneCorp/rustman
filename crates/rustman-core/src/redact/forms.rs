//! The forms a secret value is matched in: how common encoders write it
//! when a server echoes it back. One form per encoder family, so the
//! matcher stays bounded (at most [`MAX_FORMS`] per value, usually far
//! fewer once duplicates are dropped):
//!
//! - the raw value;
//! - JSON string escapes (`\"`, `\\`, control characters), with or without
//!   `\/` for `/` (PHP), with or without `\u0026`, `\u003c` and `\u003e`
//!   for `&`, `<` and `>` (Go's `encoding/json`, which also writes U+2028
//!   and U+2029 as `\u2028` and `\u2029`), each with non-ASCII characters
//!   as is or as `\uXXXX` in upper- or lowercase hex (Python, PHP);
//! - Rust `Debug` escapes;
//! - percent-encoding in upper- or lowercase hex: RFC 3986 with a space as
//!   `%20` or `+` (form encoding), leaving `/` as is (Python's `quote`), or
//!   leaving `!'()*` as is (JavaScript's `encodeURIComponent`);
//! - HTML escapes: `&amp;`, `&lt;`, `&gt;` with `"` and `'` as `&quot;` and
//!   `&#x27;` (Python), `&quot;` and `&#39;`, or `&#34;` and `&#39;` (Go);
//!   or every character but letters and digits as a decimal entity
//!   (`&#47;`).

/// The most forms one value can have.
pub(super) const MAX_FORMS: usize = 26;

/// Every form of `value`, the raw one first, without duplicates.
pub(super) fn encoded_forms(value: &str) -> Vec<String> {
    let mut forms = vec![value.to_owned()];
    let mut push = |form: String| {
        if !forms.contains(&form) {
            forms.push(form);
        }
    };
    for form in json_forms(value) {
        push(form);
    }
    let debug = format!("{value:?}");
    push(debug[1..debug.len() - 1].to_owned());
    for upper in [true, false] {
        push(percent_encode(value, "%20", upper, b""));
        push(percent_encode(value, "+", upper, b""));
        push(percent_encode(value, "%20", upper, b"/"));
        push(percent_encode(value, "%20", upper, b"!'()*"));
    }
    for (quot, apos) in [
        ("&quot;", "&#x27;"),
        ("&quot;", "&#39;"),
        ("&#34;", "&#39;"),
    ] {
        push(html_escape(value, quot, apos));
    }
    push(html_numeric(value));
    debug_assert!(forms.len() <= MAX_FORMS);
    forms
}

/// The JSON string escapes of `value`, without the quotes, in every
/// combination of `\/`, Go's HTML-safe escapes and `\uXXXX` for non-ASCII.
fn json_forms(value: &str) -> Vec<String> {
    let quoted = serde_json::to_string(value).unwrap_or_default();
    let json = &quoted[1..quoted.len() - 1];
    let mut forms = Vec::with_capacity(12);
    for slash in [false, true] {
        for go in [false, true] {
            let mut base = json.to_owned();
            if go {
                base = base
                    .replace('&', "\\u0026")
                    .replace('<', "\\u003c")
                    .replace('>', "\\u003e")
                    .replace('\u{2028}', "\\u2028")
                    .replace('\u{2029}', "\\u2029");
            }
            if slash {
                base = base.replace('/', "\\/");
            }
            forms.push(ascii_json(&base, true));
            forms.push(ascii_json(&base, false));
            forms.push(base);
        }
    }
    forms
}

/// `json` with every non-ASCII character written as `\uXXXX` escapes
/// (UTF-16, so a surrogate pair beyond the BMP).
fn ascii_json(json: &str, upper: bool) -> String {
    let mut out = String::with_capacity(json.len());
    let mut units = [0; 2];
    for ch in json.chars() {
        if ch.is_ascii() {
            out.push(ch);
            continue;
        }
        for unit in ch.encode_utf16(&mut units) {
            out.push_str(&if upper {
                format!("\\u{unit:04X}")
            } else {
                format!("\\u{unit:04x}")
            });
        }
    }
    out
}

/// Percent-encoding of every byte but unreserved characters and `safe`, in
/// upper- or lowercase hex; a space becomes `space`.
fn percent_encode(value: &str, space: &str, upper: bool, safe: &[u8]) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(char::from(byte));
            }
            _ if safe.contains(&byte) => out.push(char::from(byte)),
            b' ' => out.push_str(space),
            _ if upper => out.push_str(&format!("%{byte:02X}")),
            _ => out.push_str(&format!("%{byte:02x}")),
        }
    }
    out
}

/// `value` with `&`, `<`, `>`, `"` and `'` as HTML entities.
fn html_escape(value: &str, quot: &str, apos: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str(quot),
            '\'' => out.push_str(apos),
            _ => out.push(ch),
        }
    }
    out
}

/// `value` with every character but letters and digits as `&#<decimal>;`.
fn html_numeric(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_alphanumeric() {
            out.push(ch);
        } else {
            out.push_str(&format!("&#{};", u32::from(ch)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "a/b&c<d>e'f\"g h!(i)*é";

    fn has(form: &str) {
        let forms = encoded_forms(SECRET);
        assert!(forms.iter().any(|f| f == form), "{form} not in {forms:#?}");
    }

    #[test]
    fn every_encoder_family_has_its_form() {
        has(SECRET);
        has(r#"a/b&c<d>e'f\"g h!(i)*\u00e9"#);
        has(r#"a/b&c<d>e'f\"g h!(i)*\u00E9"#);
        has(r#"a\/b&c<d>e'f\"g h!(i)*\u00e9"#);
        has(r#"a/b\u0026c\u003cd\u003ee'f\"g h!(i)*é"#);
        has(r#"a/b&c<d>e'f\"g h!(i)*é"#);
        has("a%2Fb%26c%3Cd%3Ee%27f%22g%20h%21%28i%29%2A%C3%A9");
        has("a%2fb%26c%3cd%3ee%27f%22g+h%21%28i%29%2a%c3%a9");
        has("a/b%26c%3Cd%3Ee%27f%22g%20h%21%28i%29%2A%C3%A9");
        has("a%2Fb%26c%3Cd%3Ee'f%22g%20h!(i)*%C3%A9");
        has("a/b&amp;c&lt;d&gt;e&#x27;f&quot;g h!(i)*é");
        has("a/b&amp;c&lt;d&gt;e&#39;f&#34;g h!(i)*é");
        has("a&#47;b&#38;c&#60;d&#62;e&#39;f&#34;g&#32;h&#33;&#40;i&#41;&#42;é");
    }

    /// `json.Marshal("tOk'&<v~*/é")` in Go 1.22 prints
    /// `"tOk'\u0026\u003cv~*/é"`: `'` stays as is.
    #[test]
    fn the_go_form_is_what_go_writes() {
        let forms = encoded_forms("tOk'&<v~*/é");
        assert!(
            forms.iter().any(|f| f == r#"tOk'\u0026\u003cv~*/é"#),
            "{forms:#?}"
        );
        let line = encoded_forms("a\u{2028}<");
        assert!(line.iter().any(|f| f == r#"a\u2028\u003c"#), "{line:#?}");
    }

    #[test]
    fn forms_stay_bounded_and_plain_values_have_one() {
        assert!(encoded_forms(SECRET).len() <= MAX_FORMS);
        assert_eq!(encoded_forms("plainValue123"), ["plainValue123"]);
    }
}
