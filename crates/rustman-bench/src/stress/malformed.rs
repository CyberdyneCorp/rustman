//! Malformed payload families (stress-testing "Malformed payload families",
//! design D5): raw bytes sent through the normal executor in place of a
//! request's body, so auth, cookies and timing apply as usual. Every
//! family is bounded: depth by [`MAX_DEPTH`], width by [`MAX_WIDE`], size
//! by `max_body` (and `large-body` past 1 MiB only with its opt-in), and a
//! compression bomb's expansion by `max_bomb_ratio × max_body`.
//!
//! Generation is deterministic: the same family, base body and pick give
//! the same bytes. The large families that ignore the body (`deep-nesting`,
//! `large-body`, `wide`) are built once per run in a [`PayloadCache`] and
//! shared, so in-flight copies do not multiply with the VUs.

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;
use std::sync::{Mutex, PoisonError};

use bytes::Bytes;
use rustman_core::stress::{MALFORMED_FAMILIES, StressSettings};
use rustman_explore::mutate::{self, Candidate, MutationKind};
use serde_json::Value;
use tokio::io::AsyncReadExt;

/// The deepest `deep-nesting` payload.
pub const MAX_DEPTH: u64 = 10_000;
/// The most keys or items of a `wide` payload.
pub const MAX_WIDE: u64 = 100_000;
/// The expansion a compression bomb aims for, under its cap.
pub const BOMB_TARGET: u64 = 64 * 1024 * 1024;

const DEPTHS: [u64; 5] = [16, 128, 1024, 5000, MAX_DEPTH];
const KIB: u64 = 1024;
const LARGE_SIZES: [u64; 5] = [64 * KIB, 512 * KIB, 1024 * KIB, 4096 * KIB, 10_240 * KIB];
const WIDTHS: [u64; 3] = [1000, 10_000, MAX_WIDE];
const JSON: &str = "application/json";

/// A malformed payload family.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Family {
    JsonSyntax,
    DeepNesting,
    LargeBody,
    Wide,
    Encoding,
    ContentTypeMismatch,
    DuplicateKeys,
    Numbers,
    CompressionBomb,
}

impl Family {
    pub const ALL: [Self; 9] = [
        Self::JsonSyntax,
        Self::DeepNesting,
        Self::LargeBody,
        Self::Wide,
        Self::Encoding,
        Self::ContentTypeMismatch,
        Self::DuplicateKeys,
        Self::Numbers,
        Self::CompressionBomb,
    ];

    /// The name of `--malformed`.
    pub fn name(self) -> &'static str {
        let index = Self::ALL.iter().position(|f| *f == self).unwrap_or(0);
        MALFORMED_FAMILIES[index]
    }

    /// Whether a 2xx answer means the target accepted input it should have
    /// refused (`accepted-malformed-input`). Large, wide, deep and
    /// compressed payloads are well-formed: accepting them is judged by
    /// size (`oversized-input-accepted`) or by a failure.
    pub fn is_invalid(self) -> bool {
        matches!(
            self,
            Self::JsonSyntax
                | Self::Encoding
                | Self::ContentTypeMismatch
                | Self::DuplicateKeys
                | Self::Numbers
        )
    }
}

impl fmt::Display for Family {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for Family {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        Self::ALL
            .into_iter()
            .find(|f| f.name() == text)
            .ok_or_else(|| format!("unknown malformed family '{text}'"))
    }
}

/// The bounds of every payload, from `[stress]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caps {
    /// `max_body`: no payload is larger.
    pub max_body: u64,
    /// The largest `large-body`: `max_body`, or 1 MiB without the opt-in.
    pub large_body: u64,
    /// The largest expansion of a compression bomb.
    pub bomb: u64,
}

impl Caps {
    pub fn of(settings: &StressSettings) -> Self {
        Self {
            max_body: settings.max_body,
            large_body: settings.large_body_limit(),
            bomb: settings.bomb_limit(),
        }
    }
}

/// One malformed body.
#[derive(Clone, PartialEq, Eq)]
pub struct Payload {
    pub family: Family,
    pub bytes: Bytes,
    pub content_type: String,
    /// `gzip` for a compression bomb.
    pub content_encoding: Option<String>,
    /// What grows in this family, and how much: `depth`, `bytes` or `keys`
    /// (a bomb's expanded `bytes`).
    pub measure: Option<(&'static str, u64)>,
}

impl fmt::Debug for Payload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Payload")
            .field("family", &self.family)
            .field("bytes", &self.bytes.len())
            .field("content_type", &self.content_type)
            .field("content_encoding", &self.content_encoding)
            .field("measure", &self.measure)
            .finish()
    }
}

impl Payload {
    fn json(family: Family, bytes: impl Into<Bytes>) -> Self {
        Self::typed(family, bytes, JSON)
    }

    fn typed(family: Family, bytes: impl Into<Bytes>, content_type: &str) -> Self {
        Self {
            family,
            bytes: bytes.into(),
            content_type: content_type.to_owned(),
            content_encoding: None,
            measure: None,
        }
    }

    fn measured(mut self, what: &'static str, value: u64) -> Self {
        self.measure = Some((what, value));
        self
    }

    /// How large the payload is once decoded.
    pub fn expanded(&self) -> u64 {
        match (self.family, self.measure) {
            (Family::CompressionBomb, Some((_, bytes))) => bytes,
            _ => self.bytes.len() as u64,
        }
    }
}

/// The payload of `family` for a seeded `pick`, built around `base` (the
/// request's JSON body, when it has one). A compression bomb needs
/// [`bomb`] instead.
pub fn generate(family: Family, base: Option<&Value>, pick: u64, caps: &Caps) -> Payload {
    match family {
        Family::JsonSyntax => json_syntax(base, pick),
        Family::DeepNesting => deep_nesting(pick, caps),
        Family::LargeBody => large_body(pick, caps),
        Family::Wide => wide(pick, caps),
        Family::Encoding => encoding(pick),
        Family::ContentTypeMismatch => content_type_mismatch(base, pick),
        Family::DuplicateKeys => duplicate_keys(base),
        Family::Numbers => numbers(base, pick),
        Family::CompressionBomb => Payload::json(family, Bytes::new()),
    }
}

/// The payloads of a run's body-independent large families, by family and
/// size: each is built once, and every send shares its bytes.
#[derive(Default)]
pub struct PayloadCache(Mutex<HashMap<(Family, u64), Payload>>);

impl PayloadCache {
    /// [`generate`], from the cache when `family` ignores the body.
    pub fn generate(
        &self,
        family: Family,
        base: Option<&Value>,
        pick: u64,
        caps: &Caps,
    ) -> Payload {
        let Some(size) = shared_size(family, pick, caps) else {
            return generate(family, base, pick, caps);
        };
        let mut cache = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        cache
            .entry((family, size))
            .or_insert_with(|| generate(family, base, pick, caps))
            .clone()
    }

    /// How many payloads are kept.
    pub fn len(&self) -> usize {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// What sizes a body-independent family's payload for `pick`.
fn shared_size(family: Family, pick: u64, caps: &Caps) -> Option<u64> {
    match family {
        Family::DeepNesting => Some(depth(pick, caps)),
        Family::LargeBody => Some(large_size(pick, caps)),
        Family::Wide => Some(width(pick, caps)),
        _ => None,
    }
}

fn nth<T: Copy>(items: &[T], pick: u64) -> T {
    let index = usize::try_from(pick % items.len() as u64).unwrap_or(0);
    items[index]
}

/// The base body as text, or a small object.
fn base_text(base: Option<&Value>) -> String {
    base.map_or_else(
        || r#"{"id":1,"name":"rustman"}"#.to_owned(),
        Value::to_string,
    )
}

fn json_syntax(base: Option<&Value>, pick: u64) -> Payload {
    let text = base_text(base);
    let cut = text
        .char_indices()
        .nth(text.chars().count() / 2)
        .map_or(0, |(i, _)| i);
    let samples = [
        text[..cut].to_owned(),
        format!("{text},"),
        r#"{"id": 1,}"#.to_owned(),
        "{id: 1}".to_owned(),
        "{'id': 1}".to_owned(),
        r#"{"id": "unterminated}"#.to_owned(),
        r#"{"id": NaN}"#.to_owned(),
        "{".to_owned(),
    ];
    let index = usize::try_from(pick % samples.len() as u64).unwrap_or(0);
    Payload::json(Family::JsonSyntax, samples[index].clone())
}

fn depth(pick: u64, caps: &Caps) -> u64 {
    let fits = |d: &u64| d.saturating_mul(2) <= caps.max_body;
    let depths: Vec<u64> = DEPTHS.into_iter().filter(fits).collect();
    if depths.is_empty() {
        (caps.max_body / 2).max(1)
    } else {
        nth(&depths, pick)
    }
}

fn deep_nesting(pick: u64, caps: &Caps) -> Payload {
    let depth = depth(pick, caps);
    let n = usize::try_from(depth).unwrap_or(0);
    let body = format!("{}{}", "[".repeat(n), "]".repeat(n));
    Payload::json(Family::DeepNesting, body).measured("depth", depth)
}

/// `{"data":"xxx…"}` of exactly `size` bytes (at least the envelope).
fn sized(size: u64) -> String {
    const ENVELOPE: u64 = 11;
    let fill = usize::try_from(size.saturating_sub(ENVELOPE)).unwrap_or(0);
    format!(r#"{{"data":"{}"}}"#, "x".repeat(fill))
}

fn large_size(pick: u64, caps: &Caps) -> u64 {
    let limit = caps.large_body.min(caps.max_body);
    let sizes: Vec<u64> = LARGE_SIZES.into_iter().filter(|s| *s <= limit).collect();
    if sizes.is_empty() {
        limit
    } else {
        nth(&sizes, pick)
    }
}

fn large_body(pick: u64, caps: &Caps) -> Payload {
    let body = sized(large_size(pick, caps));
    let len = body.len() as u64;
    Payload::json(Family::LargeBody, body).measured("bytes", len)
}

/// `{"k0":0,"k1":0,…}` with `keys` members.
fn wide_object(keys: u64) -> String {
    let members: Vec<String> = (0..keys).map(|i| format!("\"k{i}\":0")).collect();
    format!("{{{}}}", members.join(","))
}

/// The size of [`wide_object`] with `keys` members.
fn wide_len(keys: u64) -> u64 {
    let digits: u64 = (0..keys)
        .map(|i| i.checked_ilog10().unwrap_or(0) as u64 + 1)
        .sum();
    // `"k` + digits + `":0` per member, commas between, braces around.
    keys * 5 + digits + keys.saturating_sub(1) + 2
}

fn width(pick: u64, caps: &Caps) -> u64 {
    let widths: Vec<u64> = WIDTHS
        .into_iter()
        .filter(|w| wide_len(*w) <= caps.max_body)
        .collect();
    if widths.is_empty() {
        1
    } else {
        nth(&widths, pick)
    }
}

fn wide(pick: u64, caps: &Caps) -> Payload {
    let keys = width(pick, caps);
    Payload::json(Family::Wide, wide_object(keys)).measured("keys", keys)
}

fn encoding(pick: u64) -> Payload {
    let samples: [&[u8]; 4] = [
        // Invalid UTF-8.
        b"{\"name\":\"\xff\xfe\"}",
        // A raw NUL inside a string.
        b"{\"name\":\"a\x00b\"}",
        // An overlong encoding of '/'.
        b"{\"name\":\"\xc0\xaf\"}",
        // A lone surrogate.
        br#"{"name":"\ud800"}"#,
    ];
    Payload::json(Family::Encoding, Bytes::from_static(nth(&samples, pick)))
}

fn content_type_mismatch(base: Option<&Value>, pick: u64) -> Payload {
    let family = Family::ContentTypeMismatch;
    match pick % 3 {
        0 => Payload::typed(family, base_text(base), "application/xml"),
        1 => Payload::json(family, "<order><qty>2</qty></order>"),
        _ => Payload::json(family, "qty=2&sku=A1"),
    }
}

/// The first member of `base` (an object) repeated with another value, as
/// the schema-less mutation does; `{"role":"user","role":"admin"}` without
/// an object.
fn duplicate_keys(base: Option<&Value>) -> Payload {
    let first = base
        .and_then(Value::as_object)
        .and_then(|o| o.keys().next())
        .map(|k| format!("/{}", k.replace('~', "~0").replace('/', "~1")));
    let text = first
        .and_then(|pointer| {
            let candidate = Candidate {
                kind: MutationKind::DuplicateKey,
                pointer,
            };
            mutate::apply(base?, &candidate)
        })
        .map_or_else(
            || r#"{"role":"user","role":"admin"}"#.to_owned(),
            |v| v.body.to_text(),
        );
    Payload::json(Family::DuplicateKeys, text)
}

const NUMBERS: [&str; 4] = [
    "1e400",
    "1234567890123456789012345678901234567890",
    "-0",
    "1e-400",
];

/// Placeholder for the number written raw.
const RAW_NUMBER: &str = "\u{0}rustman-number\u{0}";

/// `base` with its first number (else its first member) written as an
/// edge-case number; `{"n":<number>}` without an object.
fn numbers(base: Option<&Value>, pick: u64) -> Payload {
    let number = nth(&NUMBERS, pick);
    let text = base
        .and_then(Value::as_object)
        .and_then(|object| {
            let key = object
                .iter()
                .find(|(_, v)| v.is_number())
                .or_else(|| object.iter().next())
                .map(|(k, _)| k.clone())?;
            let mut copy = object.clone();
            copy.insert(key, Value::String(RAW_NUMBER.to_owned()));
            let quoted = Value::String(RAW_NUMBER.to_owned()).to_string();
            Some(Value::Object(copy).to_string().replacen(&quoted, number, 1))
        })
        .unwrap_or_else(|| format!(r#"{{"n":{number}}}"#));
    Payload::json(Family::Numbers, text)
}

/// A gzip-encoded JSON body that expands to at most `caps.bomb` bytes
/// ([`BOMB_TARGET`] at most), sent with `Content-Encoding: gzip`.
pub async fn bomb(caps: &Caps) -> std::io::Result<Payload> {
    use async_compression::tokio::bufread::GzipEncoder;
    let expanded = caps.bomb.clamp(16, BOMB_TARGET);
    let head: &[u8] = br#"{"data":""#;
    let tail: &[u8] = br#""}"#;
    let fill = expanded - (head.len() + tail.len()) as u64;
    let plain = head.chain(tokio::io::repeat(b'0').take(fill)).chain(tail);
    let mut encoder = GzipEncoder::new(tokio::io::BufReader::with_capacity(64 * 1024, plain));
    let mut compressed = Vec::new();
    encoder.read_to_end(&mut compressed).await?;
    Ok(Payload {
        family: Family::CompressionBomb,
        bytes: compressed.into(),
        content_type: JSON.to_owned(),
        content_encoding: Some("gzip".to_owned()),
        measure: Some(("bytes", expanded)),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn caps(max_body: u64) -> Caps {
        Caps {
            max_body,
            large_body: max_body.min(1024 * KIB),
            bomb: max_body * 100,
        }
    }

    fn all_picks(family: Family, caps: &Caps) -> Vec<Payload> {
        let base = json!({"qty": 2, "sku": "A1"});
        (0..20)
            .map(|p| generate(family, Some(&base), p, caps))
            .collect()
    }

    #[test]
    fn families_round_trip_their_names() {
        for family in Family::ALL {
            assert_eq!(family.name().parse::<Family>(), Ok(family));
        }
        assert!("teapot".parse::<Family>().is_err());
    }

    #[test]
    fn depths_never_exceed_the_cap_or_max_body() {
        for max_body in [100, 3000, 10 * 1024 * KIB] {
            let caps = caps(max_body);
            for payload in all_picks(Family::DeepNesting, &caps) {
                let (what, depth) = payload.measure.unwrap();
                assert_eq!(what, "depth");
                assert!(depth <= MAX_DEPTH);
                assert!(payload.bytes.len() as u64 <= max_body);
                assert_eq!(payload.bytes.len() as u64, depth * 2);
            }
        }
        let depths: Vec<u64> = all_picks(Family::DeepNesting, &caps(10 * 1024 * KIB))
            .iter()
            .map(|p| p.measure.unwrap().1)
            .collect();
        assert!(depths.contains(&5000) && depths.contains(&MAX_DEPTH));
    }

    #[test]
    fn large_bodies_stay_under_the_large_body_limit() {
        for (max_body, large) in [(10 * 1024 * KIB, 1024 * KIB), (300 * KIB, 300 * KIB)] {
            let caps = Caps {
                max_body,
                large_body: large,
                bomb: max_body,
            };
            let payloads = all_picks(Family::LargeBody, &caps);
            assert!(payloads.iter().all(|p| p.bytes.len() as u64 <= large));
            let largest = payloads.iter().map(|p| p.bytes.len()).max().unwrap();
            assert!(largest as u64 >= 64 * KIB);
            for p in &payloads {
                serde_json::from_slice::<Value>(&p.bytes).unwrap();
            }
        }
    }

    #[test]
    fn wide_payloads_stay_under_max_wide_and_max_body() {
        for max_body in [50 * KIB, 10 * 1024 * KIB] {
            for payload in all_picks(Family::Wide, &caps(max_body)) {
                let (_, keys) = payload.measure.unwrap();
                assert!(keys <= MAX_WIDE);
                assert!(payload.bytes.len() as u64 <= max_body);
                assert_eq!(payload.bytes.len() as u64, wide_len(keys));
                let parsed: Value = serde_json::from_slice(&payload.bytes).unwrap();
                assert_eq!(parsed.as_object().unwrap().len() as u64, keys);
            }
        }
    }

    #[tokio::test]
    async fn a_bomb_expands_to_at_most_its_cap() {
        use async_compression::tokio::bufread::GzipDecoder;
        let caps = caps(1024);
        let payload = bomb(&caps).await.unwrap();
        assert_eq!(payload.content_encoding.as_deref(), Some("gzip"));
        let mut decoder = GzipDecoder::new(&payload.bytes[..]);
        let mut plain = Vec::new();
        decoder.read_to_end(&mut plain).await.unwrap();
        assert_eq!(plain.len() as u64, payload.expanded());
        assert!(payload.expanded() <= caps.bomb);
        assert!(plain.len() > payload.bytes.len() * 10, "a bomb compresses");
        serde_json::from_slice::<Value>(&plain).unwrap();
    }

    #[test]
    fn invalid_families_do_not_parse_as_the_json_they_claim() {
        let caps = caps(1024 * KIB);
        for family in [Family::JsonSyntax, Family::Encoding] {
            for payload in all_picks(family, &caps) {
                let parsed = std::str::from_utf8(&payload.bytes)
                    .ok()
                    .and_then(|t| serde_json::from_str::<Value>(t).ok());
                let lone_surrogate = payload.bytes.as_ref() == br#"{"name":"\ud800"}"#;
                assert!(parsed.is_none() || lone_surrogate, "{family}: {payload:?}");
            }
        }
    }

    /// Large body-independent payloads are built once and share their
    /// bytes; body-dependent ones are built per call.
    #[test]
    fn the_cache_shares_large_payloads_across_sends() {
        let caps = caps(1024 * KIB);
        let cache = PayloadCache::default();
        let base = json!({"qty": 2});
        let first = cache.generate(Family::LargeBody, Some(&base), 7, &caps);
        let again = cache.generate(Family::LargeBody, None, 7, &caps);
        assert_eq!(first, generate(Family::LargeBody, None, 7, &caps));
        assert_eq!(first.bytes.as_ptr(), again.bytes.as_ptr());
        for pick in 0..200 {
            for family in [Family::LargeBody, Family::DeepNesting, Family::Wide] {
                assert_eq!(
                    cache.generate(family, Some(&base), pick, &caps),
                    generate(family, Some(&base), pick, &caps)
                );
            }
        }
        // One payload per size: 5 depths, 3 sizes under 1 MiB, 3 widths.
        assert!(cache.len() <= 11, "{}", cache.len());
        let syntax = cache.generate(Family::JsonSyntax, Some(&base), 1, &caps);
        assert_eq!(syntax, generate(Family::JsonSyntax, Some(&base), 1, &caps));
        assert!(cache.len() <= 11);
    }

    fn sample(family: Family, pick: u64) -> String {
        let base = json!({"qty": 2, "sku": "A1"});
        let payload = generate(family, Some(&base), pick, &caps(20_000));
        let text: String = String::from_utf8_lossy(&payload.bytes)
            .escape_debug()
            .take(60)
            .collect();
        format!(
            "{} {} ({} bytes): {text}",
            payload.content_type,
            payload
                .measure
                .map_or(String::new(), |(w, n)| format!("{w}={n}")),
            payload.bytes.len()
        )
    }

    #[test]
    fn small_samples() {
        let mut lines = Vec::new();
        for family in Family::ALL {
            if family == Family::CompressionBomb {
                continue;
            }
            for pick in 0..4 {
                lines.push(format!("{family}[{pick}] {}", sample(family, pick)));
            }
        }
        insta::assert_snapshot!(lines.join("\n"));
    }
}
