//! The `[stress]` table of `rustman.toml` (stress-testing "Stress targets
//! come only from config", "Stress caps", "Costly families need config
//! opt-in"): where `rustman stress` may send load, the caps it may not
//! exceed and the costly fault families it may use. Command-line flags only
//! ask for less; raising a cap or adding a target is a reviewed change to
//! this table.
//!
//! ```toml
//! [stress]
//! targets = ["https://staging.example.com"]
//! allow_production = []                 # hosts also confirmed with --production
//! families = ["compression-bomb"]       # costly families (opt-in)
//! max_rps = 500
//! max_duration = "10m"
//! max_soak_duration = "2h"
//! max_body = 10485760                   # bytes, or "10MiB"
//! max_bomb_ratio = 100
//! max_wire_connections = 20
//! max_vus = 1000                        # virtual users at once
//! expected_max_body = "1MiB"            # larger bodies must be refused
//!
//! [stress.slo]                          # SLOs of every stress run
//! p95 = "<300ms"
//! error_rate = "<1%"
//! ```
//!
//! Unlike most of `rustman.toml`, an unknown key in `[stress]` is an error,
//! not a warning: the table holds consent, and a misspelt key (such as
//! `max_rsp`) must not silently fall back to a default.

use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::time::Duration;

use serde::{Deserialize, Deserializer};

use crate::duration::parse_duration;
use crate::safety::{HostPattern, Origin};

/// Default `max_rps`.
pub const DEFAULT_MAX_RPS: NonZeroU32 = NonZeroU32::new(500).unwrap();
/// Default `max_duration`.
pub const DEFAULT_MAX_DURATION: Duration = Duration::from_secs(10 * 60);
/// Default `max_soak_duration`.
pub const DEFAULT_MAX_SOAK_DURATION: Duration = Duration::from_secs(2 * 60 * 60);
/// Default `max_body`: 10 MiB.
pub const DEFAULT_MAX_BODY: u64 = 10 * MIB;
/// Default `max_bomb_ratio`.
pub const DEFAULT_MAX_BOMB_RATIO: NonZeroU32 = NonZeroU32::new(100).unwrap();
/// Default `max_wire_connections`.
pub const DEFAULT_MAX_WIRE_CONNECTIONS: NonZeroU32 = NonZeroU32::new(20).unwrap();
/// Default `max_vus`.
pub const DEFAULT_MAX_VUS: NonZeroU32 = NonZeroU32::new(1000).unwrap();
/// The largest `large-body` sent without the family in `families`.
pub const LARGE_BODY_WITHOUT_OPT_IN: u64 = MIB;

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;

/// The malformed payload families (`--malformed`).
pub const MALFORMED_FAMILIES: &[&str] = &[
    "json-syntax",
    "deep-nesting",
    "large-body",
    "wide",
    "encoding",
    "content-type-mismatch",
    "duplicate-keys",
    "numbers",
    "compression-bomb",
];

/// The wire-fault families (`--wire`).
pub const WIRE_FAMILIES: &[&str] = &[
    "oversized-headers",
    "many-headers",
    "slow-body",
    "cl-te-conflict",
    "invalid-chunked",
    "invalid-request-line",
];

/// Families that run only when listed in `[stress] families` (for
/// `large-body`, only above [`LARGE_BODY_WITHOUT_OPT_IN`]).
pub const COSTLY_FAMILIES: &[&str] = &["compression-bomb", "large-body"];

/// The keys of `[stress]`, for the unknown-key check of `rustman.toml`.
pub const STRESS_KEYS: &[&str] = &[
    "targets",
    "allow_production",
    "families",
    "max_rps",
    "max_duration",
    "max_soak_duration",
    "max_body",
    "max_bomb_ratio",
    "max_wire_connections",
    "max_vus",
    "expected_max_body",
    "slo",
];

/// `[stress]`: the targets, caps and opt-ins of `rustman stress`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StressSettings {
    /// The only origins a stress run may target.
    pub targets: Vec<Origin>,
    /// Production hosts a stress run may target, when `--production`
    /// repeats the host.
    pub allow_production: Vec<HostPattern>,
    /// Costly families allowed ([`COSTLY_FAMILIES`]).
    #[serde(deserialize_with = "families")]
    pub families: Vec<String>,
    pub max_rps: NonZeroU32,
    #[serde(deserialize_with = "max_duration")]
    pub max_duration: Duration,
    #[serde(deserialize_with = "max_soak_duration")]
    pub max_soak_duration: Duration,
    /// Bytes.
    #[serde(deserialize_with = "bytes")]
    pub max_body: u64,
    pub max_bomb_ratio: NonZeroU32,
    pub max_wire_connections: NonZeroU32,
    /// The most VUs at once (`--max-vus` may only ask for fewer).
    pub max_vus: NonZeroU32,
    /// Bytes; a 2xx answer to a larger body is `oversized-input-accepted`.
    #[serde(deserialize_with = "optional_bytes")]
    pub expected_max_body: Option<u64>,
    /// `[stress.slo]`: a metric (`p95`, `file:login:p95`) and its condition
    /// (`<300ms`); [`Self::slo_expressions`] joins them.
    pub slo: BTreeMap<String, String>,
}

impl Default for StressSettings {
    fn default() -> Self {
        Self {
            targets: Vec::new(),
            allow_production: Vec::new(),
            families: Vec::new(),
            max_rps: DEFAULT_MAX_RPS,
            max_duration: DEFAULT_MAX_DURATION,
            max_soak_duration: DEFAULT_MAX_SOAK_DURATION,
            max_body: DEFAULT_MAX_BODY,
            max_bomb_ratio: DEFAULT_MAX_BOMB_RATIO,
            max_wire_connections: DEFAULT_MAX_WIRE_CONNECTIONS,
            max_vus: DEFAULT_MAX_VUS,
            expected_max_body: None,
            slo: BTreeMap::new(),
        }
    }
}

impl StressSettings {
    /// Whether `target` is listed in `targets`.
    pub fn lists_target(&self, target: &Origin) -> bool {
        self.targets.contains(target)
    }

    /// Whether production host `host` is listed in `allow_production`.
    pub fn allows_production(&self, host: &str) -> bool {
        self.allow_production.iter().any(|p| p.matches(host))
    }

    /// Whether costly family `family` is listed in `families`.
    pub fn allows_family(&self, family: &str) -> bool {
        self.families.iter().any(|f| f == family)
    }

    /// The largest `large-body` payload: `max_body` with the family opted
    /// in, else at most [`LARGE_BODY_WITHOUT_OPT_IN`].
    pub fn large_body_limit(&self) -> u64 {
        if self.allows_family("large-body") {
            self.max_body
        } else {
            self.max_body.min(LARGE_BODY_WITHOUT_OPT_IN)
        }
    }

    /// The compression-bomb ceiling: `max_bomb_ratio × max_body` bytes once
    /// expanded.
    pub fn bomb_limit(&self) -> u64 {
        self.max_body
            .saturating_mul(u64::from(self.max_bomb_ratio.get()))
    }

    /// Each `[stress.slo]` entry as one expression, such as `p95<300ms`,
    /// in key order.
    pub fn slo_expressions(&self) -> Vec<String> {
        self.slo
            .iter()
            .map(|(metric, condition)| format!("{}{}", metric.trim(), condition.trim()))
            .collect()
    }
}

/// `families`: names of [`COSTLY_FAMILIES`].
fn families<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    let names = Vec::<String>::deserialize(deserializer)?;
    if let Some(bad) = names
        .iter()
        .find(|n| !COSTLY_FAMILIES.contains(&n.as_str()))
    {
        return Err(serde::de::Error::custom(format!(
            "unknown costly family '{bad}' (expected {})",
            COSTLY_FAMILIES.join(" or ")
        )));
    }
    Ok(names)
}

fn max_duration<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
    long_duration(deserializer, "max_duration")
}

fn max_soak_duration<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
    long_duration(deserializer, "max_soak_duration")
}

/// A duration written as `500ms`, `30s`, `5m` or `2h`, or as milliseconds;
/// zero is refused, naming `what`.
fn long_duration<'de, D: Deserializer<'de>>(
    deserializer: D,
    what: &str,
) -> Result<Duration, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Millis(u64),
        Text(String),
    }
    let duration = match Raw::deserialize(deserializer)? {
        Raw::Millis(ms) => Duration::from_millis(ms),
        Raw::Text(text) => parse_stress_duration(&text).map_err(serde::de::Error::custom)?,
    };
    if duration.is_zero() {
        return Err(serde::de::Error::custom(format!(
            "{what} must be above zero"
        )));
    }
    Ok(duration)
}

/// `500ms`, `5s`, `2m`, `2h` or a bare number of milliseconds: the
/// durations of `rustman.toml` (shared parser), plus hours for soaks.
/// Zero is rejected.
pub fn parse_stress_duration(text: &str) -> Result<Duration, String> {
    let trimmed = text.trim();
    let Some(hours) = trimmed.strip_suffix('h') else {
        return parse_duration(trimmed).map_err(|_| {
            format!("invalid duration '{trimmed}' (use 500ms, 5s, 2m, 2h or milliseconds)")
        });
    };
    match hours.parse::<u64>() {
        Ok(h) if h > 0 => Ok(Duration::from_secs(h.saturating_mul(3600))),
        _ => Err(format!(
            "invalid duration '{trimmed}' (use 500ms, 5s, 2m, 2h or milliseconds)"
        )),
    }
}

fn bytes<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Number(u64),
        Text(String),
    }
    let value = match Raw::deserialize(deserializer)? {
        Raw::Number(n) => n,
        Raw::Text(text) => parse_size(&text).map_err(serde::de::Error::custom)?,
    };
    if value == 0 {
        return Err(serde::de::Error::custom("a size must be above zero"));
    }
    Ok(value)
}

fn optional_bytes<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<u64>, D::Error> {
    bytes(deserializer).map(Some)
}

/// A size in bytes: `1024`, `512KiB`, `10MiB`, `1GiB` (also `KB`, `MB`,
/// `GB` as binary multiples, and `B`).
pub fn parse_size(text: &str) -> Result<u64, String> {
    const UNITS: [(&str, u64); 7] = [
        ("KiB", KIB),
        ("MiB", MIB),
        ("GiB", 1024 * MIB),
        ("KB", KIB),
        ("MB", MIB),
        ("GB", 1024 * MIB),
        ("B", 1),
    ];
    let text = text.trim();
    let (number, scale) = UNITS
        .iter()
        .find_map(|(unit, scale)| text.strip_suffix(unit).map(|n| (n.trim(), *scale)))
        .unwrap_or((text, 1));
    number
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(scale))
        .ok_or_else(|| format!("'{text}' is not a size, such as 10MiB or 1048576"))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::settings::Settings;

    const HEAD: &str = "[workspace]\nname = \"shop\"\nversion = 1\n";

    fn parse(stress: &str) -> Result<(StressSettings, Vec<String>), String> {
        let text = format!("{HEAD}{stress}");
        Settings::parse(Path::new("rustman.toml"), &text)
            .map(|(settings, warnings)| (settings.stress, warnings))
            .map_err(|e| e.to_string())
    }

    #[test]
    fn a_missing_table_lists_no_target_and_applies_the_default_caps() {
        let (stress, warnings) = parse("").unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(stress.targets.is_empty());
        assert_eq!(stress.max_rps.get(), 500);
        assert_eq!(stress.max_duration, Duration::from_secs(600));
        assert_eq!(stress.max_soak_duration, Duration::from_secs(7200));
        assert_eq!(stress.max_body, 10 * 1024 * 1024);
        assert_eq!(stress.max_bomb_ratio.get(), 100);
        assert_eq!(stress.max_wire_connections.get(), 20);
        assert_eq!(stress.max_vus.get(), 1000);
        assert_eq!(stress.expected_max_body, None);
        assert_eq!(stress.large_body_limit(), 1024 * 1024);
    }

    const FULL: &str = "[stress]\n\
        targets = [\"https://staging.example.com\", \"http://127.0.0.1:8080\"]\n\
        allow_production = [\"api.example.com\"]\n\
        families = [\"compression-bomb\", \"large-body\"]\n\
        max_rps = 2000\nmax_duration = \"30m\"\nmax_soak_duration = \"8h\"\n\
        max_body = \"20MiB\"\nmax_bomb_ratio = 50\nmax_wire_connections = 5\n\
        max_vus = 2000\nexpected_max_body = 4096\n\
        [stress.slo]\np95 = \"<300ms\"\n\"file:checkout:p99\" = \"<800ms\"\n";

    #[test]
    fn every_key_is_read_without_an_unknown_key_warning() {
        let (stress, warnings) = parse(FULL).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        let origin = |text| Origin::parse(text).unwrap();
        assert!(stress.lists_target(&origin("https://staging.example.com")));
        assert!(stress.lists_target(&origin("http://127.0.0.1:8080")));
        assert!(!stress.lists_target(&origin("https://api.other.com")));
        assert!(stress.allows_production("api.example.com"));
        assert!(!stress.allows_production("eu.api.example.com"));
        assert!(stress.allows_family("compression-bomb"));
        assert_eq!(
            stress.slo_expressions(),
            ["file:checkout:p99<800ms", "p95<300ms"]
        );
    }

    #[test]
    fn caps_are_raised_in_the_table() {
        let (stress, _) = parse(FULL).unwrap();
        assert_eq!(stress.max_rps.get(), 2000);
        assert_eq!(stress.max_duration, Duration::from_secs(1800));
        assert_eq!(stress.max_soak_duration, Duration::from_secs(8 * 3600));
        assert_eq!(stress.max_body, 20 * 1024 * 1024);
        assert_eq!(stress.large_body_limit(), 20 * 1024 * 1024);
        assert_eq!(stress.bomb_limit(), 50 * 20 * 1024 * 1024);
        assert_eq!(stress.max_wire_connections.get(), 5);
        assert_eq!(stress.max_vus.get(), 2000);
        assert_eq!(stress.expected_max_body, Some(4096));
    }

    /// `[stress]` holds consent: an unknown or misspelt key is an error
    /// naming the key, not a warning (design open question, resolved).
    #[test]
    fn an_unknown_key_is_an_error_naming_it() {
        let err = parse("[stress]\nmax_rsp = 5000\n").unwrap_err();
        assert!(
            err.starts_with("rustman.toml:5: unknown field `max_rsp`"),
            "{err}"
        );
    }

    #[test]
    fn invalid_values_are_errors() {
        for bad in [
            "[stress]\ntargets = [\"https://staging.example.com/v1\"]\n",
            "[stress]\ntargets = [\"ftp://staging.example.com\"]\n",
            "[stress]\nfamilies = [\"slow-body\"]\n",
            "[stress]\nmax_rps = 0\n",
            "[stress]\nmax_duration = \"0s\"\n",
            "[stress]\nmax_soak_duration = \"later\"\n",
            "[stress]\nmax_body = \"10 parsecs\"\n",
            "[stress]\nmax_body = 0\n",
            "[stress]\nmax_wire_connections = 0\n",
            "[stress.slo]\np95 = 3\n",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn durations_take_hours() {
        assert_eq!(parse_stress_duration("2h"), Ok(Duration::from_secs(7200)));
        assert_eq!(parse_stress_duration("90s"), Ok(Duration::from_secs(90)));
        assert_eq!(
            parse_stress_duration("1500"),
            Ok(Duration::from_millis(1500))
        );
        for bad in ["0h", "h", "2d", "0s"] {
            assert!(parse_stress_duration(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn sizes_take_binary_units() {
        assert_eq!(parse_size("1024"), Ok(1024));
        assert_eq!(parse_size("512KiB"), Ok(512 * 1024));
        assert_eq!(parse_size("10 MiB"), Ok(10 * 1024 * 1024));
        assert_eq!(parse_size("1GB"), Ok(1024 * 1024 * 1024));
        assert_eq!(parse_size("7B"), Ok(7));
        assert!(parse_size("ten").is_err());
        assert!(parse_size("99999999999999GiB").is_err());
    }
}
