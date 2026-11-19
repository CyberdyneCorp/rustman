//! W3C Baggage: the `baggage` header.

use std::fmt::Write as _;

pub const BAGGAGE: &str = "baggage";

/// The W3C Baggage limits a receiver must accept: at most this many
/// members and this many bytes. Larger headers may be cut or rejected.
pub const MAX_MEMBERS: usize = 64;
pub const MAX_BYTES: usize = 8192;

/// A warning when the `baggage` value `header` exceeds the W3C limits;
/// it is still sent as is.
pub fn limit_warning(header: &str) -> Option<String> {
    let members = header.split(',').filter(|m| !m.trim().is_empty()).count();
    (members > MAX_MEMBERS || header.len() > MAX_BYTES).then(|| {
        format!(
            "trace: the baggage header has {members} members and {} bytes, over the W3C \
             limits of {MAX_MEMBERS} members and {MAX_BYTES} bytes; servers may drop or \
             reject it",
            header.len()
        )
    })
}

/// Join `members` into a `baggage` value, percent-encoding each value.
/// Every key must be a token ([`is_key`]).
pub fn encode(members: &[(String, String)]) -> String {
    members
        .iter()
        .map(|(k, v)| format!("{k}={}", encode_value(v)))
        .collect::<Vec<_>>()
        .join(",")
}

/// Whether `key` is an RFC 7230 token, as a baggage key must be.
pub fn is_key(key: &str) -> bool {
    !key.is_empty() && key.bytes().all(is_tchar)
}

fn is_tchar(c: u8) -> bool {
    c.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&c)
}

/// A `baggage-octet` may stand for itself; `%` always starts an escape.
fn is_plain(c: u8) -> bool {
    matches!(c, 0x21 | 0x23..=0x2b | 0x2d..=0x3a | 0x3c..=0x5b | 0x5d..=0x7e) && c != b'%'
}

/// `value` as a `baggage` member value: every byte but a plain
/// `baggage-octet` percent-encoded in uppercase hex.
pub(crate) fn encode_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if is_plain(byte) {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn members(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn values_are_percent_encoded() {
        assert_eq!(encode(&members(&[("tenant", "acme")])), "tenant=acme");
        assert_eq!(
            encode(&members(&[("userId", "alice"), ("serverNode", "DF 28")])),
            "userId=alice,serverNode=DF%2028"
        );
        assert_eq!(
            encode(&members(&[("k", "a,b;c\\d\"e%f=g")])),
            "k=a%2Cb%3Bc%5Cd%22e%25f=g"
        );
        assert_eq!(encode(&members(&[("k", "é")])), "k=%C3%A9");
        assert_eq!(encode(&[]), "");
    }

    #[test]
    fn oversized_baggage_is_warned_about() {
        let many: Vec<_> = (0..=MAX_MEMBERS)
            .map(|i| (format!("k{i}"), "v".to_owned()))
            .collect();
        let warning = limit_warning(&encode(&many)).unwrap();
        assert!(warning.contains("65 members"), "{warning}");
        let big = encode(&members(&[("k", &"x".repeat(MAX_BYTES))]));
        assert!(limit_warning(&big).unwrap().contains("over the W3C limits"));
        let at_limit: Vec<_> = many[..MAX_MEMBERS].to_vec();
        assert_eq!(limit_warning(&encode(&at_limit)), None);
        assert_eq!(limit_warning("tenant=acme"), None);
    }

    #[test]
    fn keys_are_tokens() {
        assert!(is_key("tenant_id"));
        assert!(is_key("user.id-2"));
        for bad in ["", "a b", "a,b", "a=b", "a;b", "é"] {
            assert!(!is_key(bad), "{bad}");
        }
    }
}
