//! Built-in presets (snapshot-testing "Built-in presets"): each replaces what
//! it matches with a typed placeholder in the `«…»` style of redaction.
//!
//! String presets run in a fixed order whatever order the file lists them
//! in (`jwt`, `uuids`, `timestamps`, `dates`, `etags`), so a timestamp is
//! never half-replaced by the date preset. `epoch_ms` needs the field name
//! and is applied by the JSON walk ([`epoch_ms_field`]).

use std::sync::LazyLock;

use regex::Regex;

use crate::model::Preset;

pub const TIMESTAMP: &str = "«timestamp»";
pub const DATE: &str = "«date»";
pub const UUID: &str = "«uuid»";
pub const ETAG: &str = "«etag»";
pub const EPOCH_MS: &str = "«epoch_ms»";
pub const JWT: &str = "«jwt»";

fn compiled(pattern: &str) -> Regex {
    Regex::new(pattern).expect("built-in preset patterns are valid")
}

static JWT_RE: LazyLock<Regex> =
    LazyLock::new(|| compiled(r"\beyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]*"));
static UUID_RE: LazyLock<Regex> = LazyLock::new(|| {
    compiled(r"\b[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\b")
});
/// RFC 3339 and ISO 8601 date-times: a date, `T` or a space, a time with
/// optional seconds and fraction, and an optional zone.
static TIMESTAMP_RE: LazyLock<Regex> = LazyLock::new(|| {
    compiled(
        r"\b\d{4}-\d{2}-\d{2}[Tt ]\d{2}:\d{2}(?::\d{2}(?:[.,]\d+)?)?(?:[Zz]|[+-]\d{2}(?::?\d{2})?)?",
    )
});
/// Calendar dates (`2026-11-17`) and HTTP dates (`Tue, 17 Nov 2026 10:00:00 GMT`).
static DATE_RE: LazyLock<Regex> = LazyLock::new(|| {
    compiled(
        r"\b(?:Mon|Tue|Wed|Thu|Fri|Sat|Sun), \d{2} (?:Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec) \d{4} \d{2}:\d{2}:\d{2} GMT\b|\b\d{4}-\d{2}-\d{2}\b",
    )
});
/// A whole value that is an entity tag: `"abc"` or `W/"abc"`.
static ETAG_RE: LazyLock<Regex> = LazyLock::new(|| compiled(r#"^(?:W/)?"[^"]*"$"#));

/// The string presets in the order they run, with their pattern and placeholder.
fn string_presets() -> [(Preset, &'static LazyLock<Regex>, &'static str); 5] {
    [
        (Preset::Jwt, &JWT_RE, JWT),
        (Preset::Uuids, &UUID_RE, UUID),
        (Preset::Timestamps, &TIMESTAMP_RE, TIMESTAMP),
        (Preset::Dates, &DATE_RE, DATE),
        (Preset::Etags, &ETAG_RE, ETAG),
    ]
}

/// `text` with every enabled string preset applied.
pub fn apply(presets: &[Preset], text: &str) -> String {
    let mut out = text.to_owned();
    for (preset, pattern, placeholder) in string_presets() {
        if presets.contains(&preset) && pattern.is_match(&out) {
            out = pattern.replace_all(&out, placeholder).into_owned();
        }
    }
    out
}

/// Whether a field named `key` holds a millisecond epoch under `epoch_ms`:
/// `*_at`, `*time*` or `timestamp`, case-insensitively.
pub fn epoch_ms_field(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.ends_with("_at") || key.contains("time") || key == "timestamp"
}

/// Whether `digits` is a 13-digit millisecond epoch.
pub fn is_epoch_ms(digits: &str) -> bool {
    digits.len() == 13 && digits.bytes().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn only(preset: Preset, text: &str) -> String {
        apply(&[preset], text)
    }

    #[test]
    fn timestamps() {
        for t in [
            "2026-11-17T10:00:00Z",
            "2026-11-17T10:00:00.123+02:00",
            "2026-11-17 10:00",
            "2026-11-17t10:00:00-0500",
        ] {
            assert_eq!(only(Preset::Timestamps, t), TIMESTAMP, "{t}");
        }
        assert_eq!(
            only(Preset::Timestamps, "at 2026-11-17T10:00:00Z."),
            "at «timestamp»."
        );
        assert_eq!(only(Preset::Timestamps, "2026-11-17"), "2026-11-17");
    }

    #[test]
    fn dates() {
        assert_eq!(only(Preset::Dates, "due 2026-11-17"), "due «date»");
        assert_eq!(only(Preset::Dates, "Tue, 17 Nov 2026 10:00:00 GMT"), DATE);
        assert_eq!(only(Preset::Dates, "v2026-1"), "v2026-1");
        let both = apply(&[Preset::Timestamps, Preset::Dates], "2026-11-17T10:00:00Z");
        assert_eq!(both, TIMESTAMP, "timestamps run before dates");
    }

    #[test]
    fn uuids() {
        assert_eq!(
            only(Preset::Uuids, "id 3F2504E0-4F89-11D3-9A0C-0305E82C3301!"),
            "id «uuid»!"
        );
        assert_eq!(only(Preset::Uuids, "3f2504e0-4f89"), "3f2504e0-4f89");
    }

    #[test]
    fn etags() {
        assert_eq!(only(Preset::Etags, "\"33a64df5\""), ETAG);
        assert_eq!(only(Preset::Etags, "W/\"0815\""), ETAG);
        assert_eq!(only(Preset::Etags, "say \"hi\""), "say \"hi\"");
    }

    #[test]
    fn jwts() {
        let token = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJl";
        assert_eq!(
            only(Preset::Jwt, &format!("Bearer {token}")),
            "Bearer «jwt»"
        );
        assert_eq!(only(Preset::Jwt, "eyJ.only"), "eyJ.only");
    }

    #[test]
    fn epoch_ms_fields() {
        for key in ["created_at", "updatedTime", "timestamp", "TIME"] {
            assert!(epoch_ms_field(key), "{key}");
        }
        assert!(!epoch_ms_field("id") && !epoch_ms_field("atlas"));
        assert!(is_epoch_ms("1763373600000"));
        assert!(!is_epoch_ms("176337360000") && !is_epoch_ms("17633736000a0"));
    }

    #[test]
    fn disabled_presets_keep_text() {
        assert_eq!(apply(&[], "2026-11-17T10:00:00Z"), "2026-11-17T10:00:00Z");
    }

    #[test]
    fn placeholders_are_stable() {
        let all = [
            Preset::Timestamps,
            Preset::Dates,
            Preset::Uuids,
            Preset::Etags,
            Preset::Jwt,
        ];
        for p in [TIMESTAMP, DATE, UUID, ETAG, EPOCH_MS, JWT] {
            assert_eq!(apply(&all, p), p);
        }
    }
}
