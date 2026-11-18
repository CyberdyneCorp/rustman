//! JSON pointers (RFC 6901) as used in `$ref` fragments and diagnostics.

use percent_encoding::percent_decode_str;

/// `token` escaped for use as one pointer segment: `~` is `~0`, `/` is `~1`.
pub fn escape(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
}

/// `pointer` with one more segment.
pub fn push(pointer: &str, token: &str) -> String {
    format!("{pointer}/{}", escape(token))
}

/// The pointer in a `$ref` fragment (the part after `#`), percent-decoded.
/// `None` when the fragment is not a pointer (a plain-name anchor).
pub fn from_fragment(fragment: &str) -> Option<String> {
    let decoded = percent_decode_str(fragment).decode_utf8().ok()?;
    (decoded.is_empty() || decoded.starts_with('/')).then(|| decoded.into_owned())
}

/// How a pointer is shown in diagnostics: the root is `/`.
pub fn display(pointer: &str) -> &str {
    if pointer.is_empty() { "/" } else { pointer }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_are_escaped() {
        assert_eq!(push("/paths", "/users/{id}"), "/paths/~1users~1{id}");
        assert_eq!(escape("a~b"), "a~0b");
    }

    #[test]
    fn fragments_are_decoded() {
        assert_eq!(
            from_fragment("/components/schemas/My%20Pet").as_deref(),
            Some("/components/schemas/My Pet")
        );
        assert_eq!(from_fragment("").as_deref(), Some(""));
        assert_eq!(from_fragment("anchor"), None);
        assert_eq!(display(""), "/");
    }
}
