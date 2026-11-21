//! Credentials written into broker addresses (add-messaging-client
//! "Secret redaction"): where the userinfo of a broker URL is, its
//! password, and the address shown without it.
//!
//! An address that parses as a URL has its userinfo where every parser
//! puts it: before the last `@` of the authority. One that does not parse
//! (a password with an unencoded `/`, `?` or `#`, say) is read as having
//! userinfo up to the last `@` of the whole text, so a password the
//! author meant is never shown, nor missed as a secret.

use url::Url;

/// The byte range of the userinfo of `broker` (`scheme://userinfo@host`,
/// or `userinfo@host` without a scheme), when it has one.
pub fn broker_userinfo(broker: &str) -> Option<std::ops::Range<usize>> {
    let start = scheme_end(broker).unwrap_or(0);
    let rest = &broker[start..];
    let at = if parses(broker, start) {
        let end = rest.find(['/', '?', '#', '\\']).unwrap_or(rest.len());
        rest[..end].rfind('@')?
    } else {
        rest.rfind('@')?
    };
    Some(start..start + at)
}

/// The password of the userinfo of `broker`, as written.
pub fn url_password(broker: &str) -> Option<&str> {
    let userinfo = &broker[broker_userinfo(broker)?];
    userinfo.split_once(':').map(|(_, password)| password)
}

/// The password before the last `@` of `broker` whether or not it
/// parses: what an author may have meant even when a URL parser reads
/// the text otherwise (`amqp://u:123/pw@h` is host `u`, port `123`).
fn loose_password(broker: &str) -> Option<&str> {
    let rest = &broker[scheme_end(broker).unwrap_or(0)..];
    let (userinfo, _) = rest.rsplit_once('@')?;
    userinfo.split_once(':').map(|(_, password)| password)
}

/// `broker` for display: any userinfo replaced by a marker.
pub fn without_userinfo(broker: &str) -> String {
    match broker_userinfo(broker) {
        Some(range) => format!(
            "{}«redacted:userinfo»{}",
            &broker[..range.start],
            &broker[range.end..]
        ),
        None => broker.to_owned(),
    }
}

/// The index just past `scheme://`, when `broker` starts with a valid
/// scheme.
fn scheme_end(broker: &str) -> Option<usize> {
    let (scheme, _) = broker.split_once("://")?;
    let mut chars = scheme.chars();
    let valid = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    valid.then_some(scheme.len() + 3)
}

/// Whether `broker` parses as a URL (a scheme-less one as `kafka://`).
fn parses(broker: &str, scheme_end: usize) -> bool {
    if scheme_end > 0 {
        Url::parse(broker).is_ok()
    } else {
        Url::parse(&format!("kafka://{broker}")).is_ok()
    }
}

/// `text` with each `%XX` escape decoded; invalid escapes and invalid
/// UTF-8 are kept as written.
pub fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let escaped = (bytes[at] == b'%')
            .then(|| text.get(at + 1..at + 3))
            .flatten()
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match escaped {
            Some(byte) => {
                out.push(byte);
                at += 3;
            }
            None => {
                out.push(bytes[at]);
                at += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_owned())
}

/// Every non-empty literal credential: `passwords` as written, and the
/// password of each of `brokers` as written and percent-decoded, also
/// read up to the last `@` of the text when that differs.
pub fn literal_credentials<'a>(
    passwords: impl IntoIterator<Item = Option<&'a str>>,
    brokers: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    let mut found: Vec<String> = passwords.into_iter().flatten().map(str::to_owned).collect();
    for broker in brokers {
        for password in [url_password(broker), loose_password(broker)]
            .into_iter()
            .flatten()
        {
            found.push(password.to_owned());
            found.push(percent_decode(password));
        }
    }
    found.retain(|v| !v.is_empty());
    found.sort();
    found.dedup();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_passwords_are_found_and_decoded() {
        assert_eq!(url_password("mqtt://app:pw%40x@h:1883/"), Some("pw%40x"));
        assert_eq!(url_password("amqp://guest@rabbit"), None);
        assert_eq!(url_password("amqp://rabbit:5672/v"), None);
        assert_eq!(url_password("app:pw@h:9092"), Some("pw"));
        assert_eq!(percent_decode("pw%40x%2"), "pw@x%2");
        assert_eq!(percent_decode("a%2Fb+c"), "a/b+c");
    }

    #[test]
    fn a_password_holding_a_slash_question_mark_or_hash_is_found() {
        // Regression: the userinfo was cut at the first '/', '?' or '#',
        // so these passwords were shown and never registered as secrets.
        assert_eq!(
            url_password("amqp://guest:SLASH/PW@rabbit/"),
            Some("SLASH/PW")
        );
        assert_eq!(url_password("mqtt://app:Q?PW@h:1883"), Some("Q?PW"));
        assert_eq!(url_password("mqtts://app:H#PW@h"), Some("H#PW"));
        assert_eq!(url_password("app:K/PW@h:9092"), Some("K/PW"));
        assert_eq!(
            without_userinfo("amqp://guest:SLASH/PW@rabbit/"),
            "amqp://«redacted:userinfo»@rabbit/"
        );
        assert_eq!(
            without_userinfo("app:K/PW@h:9092"),
            "«redacted:userinfo»@h:9092"
        );
    }

    #[test]
    fn userinfo_ends_at_the_authority_of_a_url_that_parses() {
        assert_eq!(
            without_userinfo("tcp://app:pw@h:1/x@y"),
            "tcp://«redacted:userinfo»@h:1/x@y"
        );
        assert_eq!(without_userinfo("mqtt://h/a@b"), "mqtt://h/a@b");
        assert_eq!(
            without_userinfo("mqtt://app:p@ss@h"),
            "mqtt://«redacted:userinfo»@h"
        );
        assert_eq!(url_password("mqtt://app:p@ss@h"), Some("p@ss"));
    }

    #[test]
    fn literal_credentials_are_deduplicated_and_decoded() {
        let found = literal_credentials(
            [Some("a"), None, Some("")],
            ["amqp://u:b%2F@h", "amqp://u:a@h"],
        );
        assert_eq!(found, ["a", "b%2F", "b/"]);
        // A URL that parses with the password read as a port and path.
        let found = literal_credentials([], ["amqp://guest:123/PW@rabbit/"]);
        assert_eq!(found, ["123/PW"]);
    }
}
