//! Signature checks (design D4; spec: GitHub, Stripe and Generic HMAC
//! signature verification). Every check runs on the raw body bytes and
//! compares MACs in constant time.

use std::fmt;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hmac::{Hmac, KeyInit, Mac};
use serde::Serialize;
use subtle::ConstantTimeEq;
use time::OffsetDateTime;

use crate::capture::HeaderEntry;

/// The header GitHub signs deliveries with.
pub const GITHUB_HEADER: &str = "X-Hub-Signature-256";
/// The header Stripe signs events with.
pub const STRIPE_HEADER: &str = "Stripe-Signature";
/// How old a Stripe timestamp may be by default.
pub const STRIPE_TOLERANCE: Duration = Duration::from_secs(300);

/// The outcome of a signature check, as captures show it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Signature {
    Valid,
    Invalid,
    Missing,
    /// Correctly signed, but the signed timestamp is older than allowed.
    Expired,
}

impl Signature {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::Invalid => "invalid",
            Self::Missing => "missing",
            Self::Expired => "expired",
        }
    }
}

impl fmt::Display for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The hash behind an HMAC.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Algorithm {
    Sha1,
    #[default]
    Sha256,
    Sha512,
}

impl Algorithm {
    pub fn parse(text: &str) -> Result<Self, String> {
        match text.to_ascii_lowercase().as_str() {
            "sha1" => Ok(Self::Sha1),
            "sha256" => Ok(Self::Sha256),
            "sha512" => Ok(Self::Sha512),
            _ => Err(format!(
                "unknown algorithm {text:?} (use sha256, sha1 or sha512)"
            )),
        }
    }

    /// The HMAC of the concatenation of `parts` under `key`.
    pub fn hmac(self, key: &[u8], parts: &[&[u8]]) -> Vec<u8> {
        match self {
            Self::Sha1 => mac::<Hmac<sha1::Sha1>>(key, parts),
            Self::Sha256 => mac::<Hmac<sha2::Sha256>>(key, parts),
            Self::Sha512 => mac::<Hmac<sha2::Sha512>>(key, parts),
        }
    }
}

fn mac<M: Mac + KeyInit>(key: &[u8], parts: &[&[u8]]) -> Vec<u8> {
    let mut mac = <M as KeyInit>::new_from_slice(key).expect("HMAC accepts keys of any length");
    for part in parts {
        mac.update(part);
    }
    mac.finalize().into_bytes().to_vec()
}

/// How a signature is written in its header.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Encoding {
    #[default]
    Hex,
    Base64,
}

impl Encoding {
    pub fn parse(text: &str) -> Result<Self, String> {
        match text.to_ascii_lowercase().as_str() {
            "hex" => Ok(Self::Hex),
            "base64" => Ok(Self::Base64),
            _ => Err(format!("unknown encoding {text:?} (use hex or base64)")),
        }
    }

    /// The bytes `text` encodes, `None` when it is not valid.
    pub fn decode(self, text: &str) -> Option<Vec<u8>> {
        match self {
            Self::Hex => decode_hex(text),
            Self::Base64 => STANDARD.decode(text).ok(),
        }
    }
}

fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    text.as_bytes()
        .chunks(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair).ok()?;
            u8::from_str_radix(pair, 16).ok()
        })
        .collect()
}

/// Whether `given` equals `expected`, in time that does not depend on where
/// they differ.
fn same(given: &[u8], expected: &[u8]) -> bool {
    given.ct_eq(expected).into()
}

/// A header-carried HMAC of the raw body: `<prefix><encoded MAC>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HmacScheme {
    pub header: String,
    pub algorithm: Algorithm,
    pub encoding: Encoding,
    pub prefix: String,
}

impl HmacScheme {
    /// GitHub's `X-Hub-Signature-256: sha256=<hex>`.
    pub fn github() -> Self {
        Self {
            header: GITHUB_HEADER.to_owned(),
            algorithm: Algorithm::Sha256,
            encoding: Encoding::Hex,
            prefix: "sha256=".to_owned(),
        }
    }

    fn check(&self, key: &[u8], headers: &[HeaderEntry], body: &[u8]) -> Signature {
        let Some(value) = first_value(headers, &self.header) else {
            return Signature::Missing;
        };
        let given = value
            .trim()
            .strip_prefix(self.prefix.as_str())
            .and_then(|encoded| self.encoding.decode(encoded.trim()));
        match given {
            Some(given) if same(&given, &self.algorithm.hmac(key, &[body])) => Signature::Valid,
            _ => Signature::Invalid,
        }
    }
}

/// How a verifier checks a request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scheme {
    Hmac(HmacScheme),
    /// `Stripe-Signature: t=<unix time>,v1=<hex>[,v1=<hex>...]`, an
    /// HMAC-SHA256 of `<t>.<raw body>`.
    Stripe {
        tolerance: Duration,
    },
}

impl Scheme {
    /// Check the signature of a request with `headers` and `body`, signed
    /// with `key`, at time `now`.
    pub fn check(
        &self,
        key: &[u8],
        headers: &[HeaderEntry],
        body: &[u8],
        now: OffsetDateTime,
    ) -> Signature {
        match self {
            Self::Hmac(scheme) => scheme.check(key, headers, body),
            Self::Stripe { tolerance } => stripe(key, headers, body, now, *tolerance),
        }
    }
}

/// The first value of header `name` (any case), as text.
fn first_value(headers: &[HeaderEntry], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case(name))
        .map(HeaderEntry::value_text)
}

/// The parts of a `Stripe-Signature` value.
struct StripeHeader<'a> {
    timestamp: &'a str,
    signatures: Vec<&'a str>,
}

fn parse_stripe(value: &str) -> Option<StripeHeader<'_>> {
    let mut timestamp = None;
    let mut signatures = Vec::new();
    for (key, val) in value.split(',').filter_map(|item| item.split_once('=')) {
        match key.trim() {
            "t" => timestamp = Some(val.trim()),
            "v1" => signatures.push(val.trim()),
            _ => {}
        }
    }
    Some(StripeHeader {
        timestamp: timestamp?,
        signatures,
    })
}

fn stripe(
    key: &[u8],
    headers: &[HeaderEntry],
    body: &[u8],
    now: OffsetDateTime,
    tolerance: Duration,
) -> Signature {
    let Some(value) = first_value(headers, STRIPE_HEADER) else {
        return Signature::Missing;
    };
    let Some(header) = parse_stripe(&value) else {
        return Signature::Invalid;
    };
    let Ok(signed_at) = header.timestamp.parse::<i64>() else {
        return Signature::Invalid;
    };
    let expected = Algorithm::Sha256.hmac(key, &[header.timestamp.as_bytes(), b".", body]);
    let matched = header
        .signatures
        .iter()
        .filter_map(|s| Encoding::Hex.decode(s))
        .fold(false, |any, given| any | same(&given, &expected));
    if !matched {
        return Signature::Invalid;
    }
    // A timestamp too far in the future is as far outside the tolerance
    // window as an old one.
    let skew = now.unix_timestamp().abs_diff(signed_at);
    if skew > tolerance.as_secs() {
        Signature::Expired
    } else {
        Signature::Valid
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const KEY: &[u8] = b"It's a Secret to Everybody";
    const BODY: &[u8] = b"Hello, World!";

    pub fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn headers(name: &str, value: &str) -> Vec<HeaderEntry> {
        vec![HeaderEntry::new(name, value.to_owned())]
    }

    fn at(unix: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(unix).unwrap()
    }

    fn github(value: &str, body: &[u8]) -> Signature {
        Scheme::Hmac(HmacScheme::github()).check(
            KEY,
            &headers("x-hub-signature-256", value),
            body,
            at(0),
        )
    }

    #[test]
    fn github_documented_example_is_valid() {
        // The example from GitHub's "Validating webhook deliveries" guide.
        let mac = "757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17";
        assert_eq!(github(&format!("sha256={mac}"), BODY), Signature::Valid);
        let upper = format!("sha256={}", mac.to_uppercase());
        assert_eq!(github(&upper, BODY), Signature::Valid);
    }

    #[test]
    fn github_tampered_and_missing() {
        let signature = format!("sha256={}", hex(&Algorithm::Sha256.hmac(KEY, &[BODY])));
        assert_eq!(github(&signature, b"Hello, World?"), Signature::Invalid);
        assert_eq!(github("sha1=abcd", BODY), Signature::Invalid);
        assert_eq!(github("sha256=not-hex", BODY), Signature::Invalid);
        let none = Scheme::Hmac(HmacScheme::github()).check(KEY, &[], BODY, at(0));
        assert_eq!(none, Signature::Missing);
    }

    fn stripe_mac(t: i64, key: &[u8], body: &[u8]) -> String {
        hex(&Algorithm::Sha256.hmac(key, &[t.to_string().as_bytes(), b".", body]))
    }

    fn stripe_header(t: i64, key: &[u8], body: &[u8]) -> String {
        format!("t={t},v1={}", stripe_mac(t, key, body))
    }

    fn stripe_check(value: &str, now: i64) -> Signature {
        let scheme = Scheme::Stripe {
            tolerance: STRIPE_TOLERANCE,
        };
        scheme.check(KEY, &headers("Stripe-Signature", value), BODY, at(now))
    }

    #[test]
    fn stripe_valid_within_tolerance_and_with_several_v1() {
        let t = 1_700_000_000;
        assert_eq!(
            stripe_check(&stripe_header(t, KEY, BODY), t + 300),
            Signature::Valid
        );
        let rotated = format!(
            "t={t},v1={},v1={}",
            "00".repeat(32),
            stripe_mac(t, KEY, BODY)
        );
        assert_eq!(stripe_check(&rotated, t), Signature::Valid);
        let with_v0 = format!("{},v0=ignored", stripe_header(t, KEY, BODY));
        assert_eq!(stripe_check(&with_v0, t), Signature::Valid);
    }

    #[test]
    fn stripe_ten_minutes_old_is_expired() {
        let t = 1_700_000_000;
        assert_eq!(
            stripe_check(&stripe_header(t, KEY, BODY), t + 600),
            Signature::Expired
        );
        assert_eq!(
            stripe_check(&stripe_header(t, KEY, BODY), t + 301),
            Signature::Expired
        );
    }

    #[test]
    fn stripe_far_future_timestamp_is_expired() {
        let now = 1_700_000_000;
        let future = now + 100_000_000;
        assert_eq!(
            stripe_check(&stripe_header(future, KEY, BODY), now),
            Signature::Expired
        );
        assert_eq!(
            stripe_check(&stripe_header(now + 300, KEY, BODY), now),
            Signature::Valid
        );
    }

    #[test]
    fn stripe_wrong_key_malformed_and_missing() {
        let t = 1_700_000_000;
        assert_eq!(
            stripe_check(&stripe_header(t, b"other", BODY), t),
            Signature::Invalid
        );
        // An expired timestamp with a wrong signature is invalid, not expired.
        assert_eq!(
            stripe_check(&stripe_header(t, b"other", BODY), t + 600),
            Signature::Invalid
        );
        assert_eq!(stripe_check("v1=abcd", t), Signature::Invalid);
        assert_eq!(stripe_check("t=abc,v1=abcd", t), Signature::Invalid);
        assert_eq!(stripe_check(&format!("t={t}"), t), Signature::Invalid);
        let scheme = Scheme::Stripe {
            tolerance: STRIPE_TOLERANCE,
        };
        assert_eq!(scheme.check(KEY, &[], BODY, at(t)), Signature::Missing);
    }

    #[test]
    fn generic_hmac_base64_with_prefix_and_every_algorithm() {
        for algorithm in [Algorithm::Sha1, Algorithm::Sha256, Algorithm::Sha512] {
            let scheme = HmacScheme {
                header: "X-Signature".into(),
                algorithm,
                encoding: Encoding::Base64,
                prefix: "v1,".into(),
            };
            let value = format!("v1,{}", STANDARD.encode(algorithm.hmac(KEY, &[BODY])));
            let ok = Scheme::Hmac(scheme.clone()).check(
                KEY,
                &headers("x-signature", &value),
                BODY,
                at(0),
            );
            assert_eq!(ok, Signature::Valid, "{algorithm:?}");
            let bad = Scheme::Hmac(scheme).check(KEY, &headers("X-Signature", &value), b"x", at(0));
            assert_eq!(bad, Signature::Invalid, "{algorithm:?}");
        }
    }

    #[test]
    fn known_hmac_vectors() {
        // RFC 2202 / RFC 4231 test case 2.
        let (key, data) = (
            b"Jefe".as_slice(),
            b"what do ya want for nothing?".as_slice(),
        );
        assert_eq!(
            hex(&Algorithm::Sha1.hmac(key, &[data])),
            "effcdf6ae5eb2fa2d27416d5f184df9c259a7c79"
        );
        assert_eq!(
            hex(&Algorithm::Sha256.hmac(key, &[b"what do ya ", b"want for nothing?"])),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        assert!(hex(&Algorithm::Sha512.hmac(key, &[data])).starts_with("164b7a7bfcf819e2"));
    }

    #[test]
    fn names_parse_and_display() {
        assert_eq!(Algorithm::parse("SHA512"), Ok(Algorithm::Sha512));
        assert!(Algorithm::parse("md5").is_err());
        assert_eq!(Encoding::parse("base64"), Ok(Encoding::Base64));
        assert!(Encoding::parse("b32").is_err());
        assert_eq!(Signature::Expired.to_string(), "expired");
        assert_eq!(
            serde_json::to_string(&Signature::Valid).unwrap(),
            "\"valid\""
        );
        assert_eq!(decode_hex("0aFf"), Some(vec![0x0a, 0xff]));
        assert_eq!(decode_hex("abc"), None);
    }
}
