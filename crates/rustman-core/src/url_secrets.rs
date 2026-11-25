//! Credentials typed into a URL: its `user:password@` userinfo, the values
//! of key-like query parameters (`apikey`, `api_key`, `key`, `token`,
//! `access_token`, `auth`, `secret`, ...) and long opaque path segments
//! (`/v3/<key>`). Callers hand them to a [`crate::Redactor`] so a provider
//! URL written with a literal key never shows the key in errors or reports.

/// Query parameter names whose values are credentials: a name containing
/// one of these words, case-insensitively (`apikey`, `api_key`,
/// `access_token`, `X-Auth`).
const KEY_WORDS: [&str; 5] = ["key", "token", "secret", "auth", "password"];

/// The shortest query value taken for a credential: shorter ones are
/// flags (`key=1`), and redacting them would hide unrelated text.
const MIN_QUERY_VALUE: usize = 4;

/// The shortest path segment taken for a key.
pub const MIN_KEY_SEGMENT: usize = 20;

/// The credentials in `url`, in order: the userinfo (whole), then key-like
/// path segments, then key-like query values.
pub fn url_secrets(url: &str) -> Vec<&str> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let rest = rest.split('#').next().unwrap_or_default();
    let authority_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let (authority, rest) = rest.split_at(authority_end);
    let userinfo = authority.rsplit_once('@').map(|(info, _)| info);
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let in_path = path.split('/').filter(|s| key_segment(s));
    let in_query = query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .filter(|(name, value)| key_param(name) && value.len() >= MIN_QUERY_VALUE)
        .map(|(_, value)| value);
    userinfo
        .into_iter()
        .filter(|info| !info.is_empty())
        .chain(in_path)
        .chain(in_query)
        .collect()
}

/// Whether the query parameter `name` holds a credential.
pub fn key_param(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    KEY_WORDS.iter().any(|word| name.contains(word))
}

/// A long opaque path segment: at least [`MIN_KEY_SEGMENT`] characters of
/// `[A-Za-z0-9_-]` mixing letters and digits, and not a `0x` hex value (a
/// block hash or an address).
fn key_segment(segment: &str) -> bool {
    let bytes = segment.as_bytes();
    let hex = segment
        .strip_prefix("0x")
        .is_some_and(|digits| digits.bytes().all(|b| b.is_ascii_hexdigit()));
    segment.len() >= MIN_KEY_SEGMENT
        && !hex
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'-' || *b == b'_')
        && bytes.iter().any(u8::is_ascii_digit)
        && bytes.iter().any(u8::is_ascii_alphabetic)
}

/// What a URL's userinfo is shown and stored as.
pub const USERINFO_MARK: &str = "«redacted:userinfo»";

/// Where a URL's authority ends, as URL parsers end it: its userinfo may
/// hold any other character (sub-delims such as `'` and `)` included).
const URL_ENDS: [char; 4] = ['/', '?', '#', '\\'];

/// Where the authority of a URL inside free text (an error, a warning)
/// ends: where a URL's ends, or at whitespace. Quotes and brackets do not
/// end it, since a password may hold them.
const TEXT_ENDS: [char; 8] = ['/', '?', '#', '\\', ' ', '\t', '\n', '\r'];

/// The byte range of the userinfo of `url` (`scheme://userinfo@host`):
/// before the last `@` of the authority, as URL parsers read it. `None`
/// without a scheme, without an `@`, or with an empty userinfo.
pub fn url_userinfo(url: &str) -> Option<std::ops::Range<usize>> {
    let start = url.find("://")? + 3;
    userinfo_at(url, start, &URL_ENDS)
}

/// The userinfo ranges of `url` and of every URL nested in it
/// (`?next=https://user:pw@host/`), in order.
pub fn url_userinfos(url: &str) -> Vec<std::ops::Range<usize>> {
    userinfos(url, &URL_ENDS)
}

/// The userinfo after `start` (just past a `://`), the authority ending at
/// the first of `ends`.
fn userinfo_at(text: &str, start: usize, ends: &[char]) -> Option<std::ops::Range<usize>> {
    let rest = &text[start..];
    let authority = &rest[..rest.find(ends).unwrap_or(rest.len())];
    let at = authority.rfind('@')?;
    (at > 0).then_some(start..start + at)
}

/// The userinfo ranges after every `scheme://` in `text`, in order.
fn userinfos(text: &str, ends: &[char]) -> Vec<std::ops::Range<usize>> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(at) = text[from..].find("://") {
        let start = from + at + 3;
        from = start;
        let has_scheme = text[..start - 3]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric());
        if let Some(range) = has_scheme.then(|| userinfo_at(text, start, ends)).flatten() {
            from = range.end;
            found.push(range);
        }
    }
    found
}

/// `text` with `ranges` (in order, apart) replaced by [`USERINFO_MARK`];
/// borrowed when there are none.
fn mark<'t>(text: &'t str, ranges: &[std::ops::Range<usize>]) -> std::borrow::Cow<'t, str> {
    if ranges.is_empty() {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    for range in ranges {
        out.push_str(&text[copied..range.start]);
        out.push_str(USERINFO_MARK);
        copied = range.end;
    }
    out.push_str(&text[copied..]);
    std::borrow::Cow::Owned(out)
}

/// The URL `url` with its userinfo, and that of every URL nested in it,
/// replaced by [`USERINFO_MARK`]. The authority ends only where a URL
/// parser ends it, so a password holding `'`, `)` or a space is hidden
/// whole. Free text goes through [`hide_userinfo`] instead.
pub fn hide_url_userinfo(url: &str) -> std::borrow::Cow<'_, str> {
    mark(url, &url_userinfos(url))
}

/// `text` with the userinfo of every URL in it (`scheme://user:pw@host`)
/// replaced by [`USERINFO_MARK`]: a password typed into a URL is no known
/// secret, so redaction alone would keep it. A URL in free text also ends
/// at whitespace. Text without one is borrowed.
pub fn hide_userinfo(text: &str) -> std::borrow::Cow<'_, str> {
    mark(text, &userinfos(text, &TEXT_ENDS))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn userinfo_is_found_and_hidden() {
        let url = "https://alice:hunter2@h.example:8443/p@x?q=a@b";
        assert_eq!(&url[url_userinfo(url).unwrap()], "alice:hunter2");
        assert_eq!(url_userinfo("https://h/p@x"), None);
        assert_eq!(url_userinfo("https://@h/"), None);
        assert_eq!(url_userinfo("no url"), None);
        assert_eq!(
            hide_userinfo(url),
            "https://«redacted:userinfo»@h.example:8443/p@x?q=a@b"
        );
        let text = "error sending to http://u:p@a/x (from wss://tok@b): mail me@c.example";
        assert_eq!(
            hide_userinfo(text),
            "error sending to http://«redacted:userinfo»@a/x \
             (from wss://«redacted:userinfo»@b): mail me@c.example"
        );
        assert!(matches!(
            hide_userinfo("https://h/x and ://u@v"),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    /// `'`, `)` and the like are valid in userinfo: the whole password is
    /// hidden, in a URL field and in free text.
    #[test]
    fn passwords_with_sub_delims_are_hidden_whole() {
        for url in [
            "https://bob:Pa)ss9@api.example/x",
            "https://bob:pa'ss@api.example/x",
            "https://bob:pa\"ss>@api.example/x",
            "https://bob:pa ss@api.example/x",
        ] {
            assert_eq!(
                hide_url_userinfo(url),
                "https://«redacted:userinfo»@api.example/x",
                "{url}"
            );
        }
        assert_eq!(
            hide_userinfo("invalid URL 'https://bob:Pa)ss9@api.example/x': bad"),
            "invalid URL 'https://«redacted:userinfo»@api.example/x': bad"
        );
        assert_eq!(
            hide_userinfo("(see 'http://h') mail me@c.example"),
            "(see 'http://h') mail me@c.example"
        );
    }

    /// A URL nested in another one's query keeps no userinfo either.
    #[test]
    fn nested_url_userinfo_is_found() {
        let url = "https://api.example.com/login?next=https://bob:hunter2@intranet.example/";
        let found: Vec<&str> = url_userinfos(url).into_iter().map(|r| &url[r]).collect();
        assert_eq!(found, ["bob:hunter2"]);
        assert_eq!(
            hide_url_userinfo(url),
            "https://api.example.com/login?next=https://«redacted:userinfo»@intranet.example/"
        );
        let both = "https://a:b@h/?u=wss://c:d@i/";
        let found: Vec<&str> = url_userinfos(both).into_iter().map(|r| &both[r]).collect();
        assert_eq!(found, ["a:b", "c:d"]);
    }

    #[test]
    fn keys_in_paths_queries_and_userinfo_are_found() {
        let url =
            "https://mainnet.example/v3/9aa3d95b3bc440fa88ea12eaa4456161?apikey=QKEY456&chain=1#x";
        assert_eq!(
            url_secrets(url),
            ["9aa3d95b3bc440fa88ea12eaa4456161", "QKEY456"]
        );
        let info = "https://user:hunter2@node.example/rpc?access_token=tok-123&X-Auth=abcd";
        assert_eq!(url_secrets(info), ["user:hunter2", "tok-123", "abcd"]);
        let names = "http://h/?api_key=aaaa&key=bbbb&token=cccc&secret=dddd&auth=eeee&page=ffff";
        assert_eq!(url_secrets(names), ["aaaa", "bbbb", "cccc", "dddd", "eeee"]);
    }

    #[test]
    fn ordinary_urls_hold_no_keys() {
        for url in [
            "https://eth-mainnet.example/ethereum-mainnet-archive/rpc?chain=1",
            "http://127.0.0.1:8545",
            "https://h/v1/key1",
            "https://h/?key=1",
            // Block hashes and addresses are not keys.
            "https://h/block/0x5c504ed432cb51138bcf09aa5e8a410dd4a1e204ef84bfed1be16dfba1b22060",
            "https://h/address/0xd8da6bf26964af9d7eed9e03e53415d37aa96045",
            "https://@h/",
        ] {
            assert!(url_secrets(url).is_empty(), "{url}: {:?}", url_secrets(url));
        }
    }
}
