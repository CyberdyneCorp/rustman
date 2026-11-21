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

#[cfg(test)]
mod tests {
    use super::*;

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
