//! Dynamic variables: `{{$uuid}}`, `{{$timestamp}}`, `{{$randomInt 5 7}}`, ...
//!
//! Each occurrence is evaluated separately, so two `{{$uuid}}` in one body differ.

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use rand::distr::{Alphanumeric, SampleString};
use time::OffsetDateTime;

/// `{{$randomInt}}` without arguments draws from `0..=1000`.
pub const DEFAULT_RANDOM_INT: (i64, i64) = (0, 1000);
/// `{{$randomString}}` without an argument has this length.
pub const DEFAULT_RANDOM_STRING_LEN: usize = 16;
/// The longest `{{$randomString n}}` accepted.
pub const MAX_RANDOM_STRING_LEN: usize = 1 << 20;

/// Usage for every dynamic variable, used in error messages.
const USAGE: &[(&str, &str)] = &[
    ("$uuid", "$uuid"),
    ("$timestamp", "$timestamp"),
    ("$timestampMs", "$timestampMs"),
    ("$isoTimestamp", "$isoTimestamp"),
    ("$randomInt", "$randomInt [min max]"),
    ("$randomString", "$randomString [length]"),
    ("$env", "$env NAME"),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Dynamic {
    /// A random UUIDv4.
    Uuid,
    /// Unix time in seconds.
    Timestamp,
    /// Unix time in milliseconds.
    TimestampMs,
    /// RFC 3339 UTC, with milliseconds.
    IsoTimestamp,
    /// A random integer in `min..=max`.
    RandomInt { min: i64, max: i64 },
    /// A random alphanumeric string of this length.
    RandomString(usize),
    /// An OS environment variable; unresolved when unset.
    Env(String),
}

/// Reads an OS environment variable.
pub type OsEnv = fn(&str) -> Option<String>;

/// The process environment. Non-UTF-8 values count as unset.
pub fn process_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

impl Dynamic {
    /// Parse the inside of a placeholder that starts with `$`, e.g. `$randomInt 5 7`.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut words = text.split_whitespace();
        let head = words.next().unwrap_or_default();
        let args: Vec<&str> = words.collect();
        let Some((_, usage)) = USAGE.iter().find(|(name, _)| *name == head) else {
            return Err(format!("unknown dynamic variable '{head}'"));
        };
        Self::from_parts(head, &args).ok_or_else(|| {
            format!("invalid dynamic variable '{{{{{text}}}}}' (usage: {{{{{usage}}}}})")
        })
    }

    fn from_parts(head: &str, args: &[&str]) -> Option<Self> {
        match (head, args) {
            ("$uuid", []) => Some(Self::Uuid),
            ("$timestamp", []) => Some(Self::Timestamp),
            ("$timestampMs", []) => Some(Self::TimestampMs),
            ("$isoTimestamp", []) => Some(Self::IsoTimestamp),
            ("$randomInt", []) => {
                let (min, max) = DEFAULT_RANDOM_INT;
                Some(Self::RandomInt { min, max })
            }
            ("$randomInt", [min, max]) => random_int(min, max),
            ("$randomString", []) => Some(Self::RandomString(DEFAULT_RANDOM_STRING_LEN)),
            ("$randomString", [len]) => len
                .parse()
                .ok()
                .filter(|n| *n <= MAX_RANDOM_STRING_LEN)
                .map(Self::RandomString),
            ("$env", [name]) => Some(Self::Env((*name).to_owned())),
            _ => None,
        }
    }

    /// Produce a value. Only `$env` can fail, when the variable is unset.
    pub fn evaluate(&self, os_env: OsEnv) -> Option<String> {
        match self {
            Self::Uuid => Some(uuid::Uuid::new_v4().to_string()),
            Self::Timestamp => Some(since_epoch().as_secs().to_string()),
            Self::TimestampMs => Some(since_epoch().as_millis().to_string()),
            Self::IsoTimestamp => Some(iso_now()),
            Self::RandomInt { min, max } => Some(rand::random_range(*min..=*max).to_string()),
            Self::RandomString(len) => Some(Alphanumeric.sample_string(&mut rand::rng(), *len)),
            Self::Env(name) => os_env(name),
        }
    }
}

fn random_int(min: &str, max: &str) -> Option<Dynamic> {
    let min: i64 = min.parse().ok()?;
    let max: i64 = max.parse().ok()?;
    (min <= max).then_some(Dynamic::RandomInt { min, max })
}

fn since_epoch() -> std::time::Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
}

/// RFC 3339 UTC with milliseconds, e.g. `2026-11-17T09:30:00.123Z`.
pub(crate) fn iso_now() -> String {
    let now = OffsetDateTime::now_utc();
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
        now.millisecond()
    )
}

impl fmt::Display for Dynamic {
    /// The placeholder text without braces, e.g. `$env API_KEY`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Uuid => f.write_str("$uuid"),
            Self::Timestamp => f.write_str("$timestamp"),
            Self::TimestampMs => f.write_str("$timestampMs"),
            Self::IsoTimestamp => f.write_str("$isoTimestamp"),
            Self::RandomInt { min, max } => write!(f, "$randomInt {min} {max}"),
            Self::RandomString(len) => write!(f, "$randomString {len}"),
            Self::Env(name) => write!(f, "$env {name}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn eval(text: &str) -> String {
        Dynamic::parse(text).unwrap().evaluate(no_env).unwrap()
    }

    #[test]
    fn uuids_are_distinct_v4() {
        let a = eval("$uuid");
        let b = eval("$uuid");
        assert_ne!(a, b);
        for id in [a, b] {
            let parsed = uuid::Uuid::parse_str(&id).unwrap();
            assert_eq!(parsed.get_version_num(), 4, "{id}");
            assert_eq!(id.len(), 36);
        }
    }

    #[test]
    fn bounded_random_int_stays_in_range() {
        let seen: HashSet<i64> = (0..1000)
            .map(|_| eval("$randomInt 5 7").parse().unwrap())
            .collect();
        assert_eq!(seen, HashSet::from([5, 6, 7]));
    }

    #[test]
    fn default_random_int_is_0_to_1000() {
        for _ in 0..1000 {
            let n: i64 = eval("$randomInt").parse().unwrap();
            assert!((0..=1000).contains(&n), "{n}");
        }
        assert_eq!(eval("$randomInt -3 -3"), "-3");
    }

    #[test]
    fn random_strings_are_alphanumeric() {
        let s = eval("$randomString");
        assert_eq!(s.len(), DEFAULT_RANDOM_STRING_LEN);
        assert!(s.chars().all(|c| c.is_ascii_alphanumeric()), "{s}");
        assert_eq!(eval("$randomString 40").len(), 40);
        assert_eq!(eval("$randomString 0"), "");
    }

    #[test]
    fn timestamps() {
        let secs: u64 = eval("$timestamp").parse().unwrap();
        let millis: u128 = eval("$timestampMs").parse().unwrap();
        assert!(secs > 1_700_000_000, "{secs}");
        assert!(millis / 1000 >= u128::from(secs), "{millis}");
        let iso = eval("$isoTimestamp");
        // e.g. 2026-11-17T09:30:00.123Z
        let shape: String = iso
            .chars()
            .map(|c| if c.is_ascii_digit() { 'd' } else { c })
            .collect();
        assert_eq!(shape, "dddd-dd-ddTdd:dd:dd.dddZ", "{iso}");
        let year = OffsetDateTime::now_utc().year().to_string();
        assert!(iso.starts_with(&year[..3]), "{iso}");
    }

    #[test]
    fn env_reads_the_given_lookup() {
        let env = Dynamic::parse("$env API_KEY").unwrap();
        assert_eq!(env.evaluate(no_env), None);
        assert_eq!(
            env.evaluate(|n| Some(format!("v-{n}"))).as_deref(),
            Some("v-API_KEY")
        );
        assert_eq!(env.to_string(), "$env API_KEY");
    }

    #[test]
    fn invalid_usages_are_errors() {
        assert_eq!(
            Dynamic::parse("$nope").unwrap_err(),
            "unknown dynamic variable '$nope'"
        );
        assert_eq!(
            Dynamic::parse("$randomInt 7 5").unwrap_err(),
            "invalid dynamic variable '{{$randomInt 7 5}}' (usage: {{$randomInt [min max]}})"
        );
        for bad in [
            "$randomInt 1",
            "$randomInt a b",
            "$uuid x",
            "$env",
            "$env A B",
            "$randomString -1",
        ] {
            assert!(Dynamic::parse(bad).is_err(), "{bad}");
        }
        assert!(Dynamic::parse(&format!("$randomString {}", MAX_RANDOM_STRING_LEN + 1)).is_err());
    }
}
