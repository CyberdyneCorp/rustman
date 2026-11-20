//! The `[bench]` table of `rustman.toml` (load-testing "Default load caps"):
//! the ceilings a load test may not exceed. Command-line flags may only ask
//! for less; raising a cap is a reviewed change to this table.
//!
//! ```toml
//! [bench]
//! max_vus = 50          # virtual users at once
//! max_duration = "5m"   # total run time (500ms, 30s, 5m)
//! max_rps = 200         # requests per second, across every VU
//!
//! [bench.thresholds]    # checked by every bench of the workspace
//! p95 = "<300ms"
//! error_rate = "<1%!"
//!
//! [[bench.stages]]       # ramping-vus stages when no --stage is given
//! duration = "30s"
//! target = 20
//! ```

use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::time::Duration;

use serde::{Deserialize, Deserializer};

use crate::duration::parse_duration;

/// Default `max_vus`.
pub const DEFAULT_MAX_VUS: NonZeroU32 = NonZeroU32::new(50).unwrap();
/// Default `max_duration`.
pub const DEFAULT_MAX_DURATION: Duration = Duration::from_secs(5 * 60);
/// Default `max_rps`.
pub const DEFAULT_MAX_RPS: NonZeroU32 = NonZeroU32::new(200).unwrap();

/// The keys of `[bench]`, for the unknown-key check of `rustman.toml`.
pub const BENCH_KEYS: &[&str] = &["max_vus", "max_duration", "max_rps", "thresholds", "stages"];

/// `[bench]`: the caps of `rustman bench`, and the thresholds every bench
/// of the workspace checks.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BenchSettings {
    pub max_vus: NonZeroU32,
    #[serde(deserialize_with = "duration")]
    pub max_duration: Duration,
    pub max_rps: NonZeroU32,
    /// `[bench.thresholds]`: a metric (`p95`, `file:login:p95`) and its
    /// condition (`<300ms`, `<5%!`); [`Self::threshold_expressions`]
    /// joins them.
    pub thresholds: BTreeMap<String, String>,
    /// `[[bench.stages]]`: the `ramping-vus` stages used when the command
    /// line gives no `--stage`.
    pub stages: Vec<BenchStage>,
}

/// One `[[bench.stages]]` entry: move linearly to `target` VUs over
/// `duration`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchStage {
    #[serde(deserialize_with = "stage_duration")]
    pub duration: Duration,
    pub target: u32,
}

impl BenchSettings {
    /// Each `[bench.thresholds]` entry as one expression, such as
    /// `p95<300ms`, in key order.
    pub fn threshold_expressions(&self) -> Vec<String> {
        self.thresholds
            .iter()
            .map(|(metric, condition)| format!("{}{}", metric.trim(), condition.trim()))
            .collect()
    }
}

impl Default for BenchSettings {
    fn default() -> Self {
        Self {
            max_vus: DEFAULT_MAX_VUS,
            max_duration: DEFAULT_MAX_DURATION,
            max_rps: DEFAULT_MAX_RPS,
            thresholds: BTreeMap::new(),
            stages: Vec::new(),
        }
    }
}

/// `max_duration`: a duration above zero.
fn duration<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
    positive_duration(deserializer, "max_duration")
}

/// A stage's `duration`: a duration above zero.
fn stage_duration<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
    positive_duration(deserializer, "a stage duration")
}

/// A duration written as `500ms`, `30s` or `5m`, or as milliseconds; zero
/// is refused, naming `what`.
fn positive_duration<'de, D: Deserializer<'de>>(
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
        Raw::Text(text) => parse_duration(&text).map_err(serde::de::Error::custom)?,
    };
    if duration.is_zero() {
        return Err(serde::de::Error::custom(format!(
            "{what} must be above zero"
        )));
    }
    Ok(duration)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::settings::Settings;

    const HEAD: &str = "[workspace]\nname = \"shop\"\nversion = 1\n";

    fn parse(bench: &str) -> Result<(BenchSettings, Vec<String>), String> {
        let text = format!("{HEAD}{bench}");
        Settings::parse(Path::new("rustman.toml"), &text)
            .map(|(settings, warnings)| (settings.bench, warnings))
            .map_err(|e| e.to_string())
    }

    #[test]
    fn a_missing_table_applies_the_default_caps() {
        let (bench, warnings) = parse("").unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(bench.max_vus.get(), 50);
        assert_eq!(bench.max_duration, Duration::from_secs(300));
        assert_eq!(bench.max_rps.get(), 200);
    }

    #[test]
    fn every_key_is_read_without_an_unknown_key_warning() {
        let (bench, warnings) =
            parse("[bench]\nmax_vus = 200\nmax_duration = \"10m\"\nmax_rps = 1000\n").unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(bench.max_vus.get(), 200);
        assert_eq!(bench.max_duration, Duration::from_secs(600));
        assert_eq!(bench.max_rps.get(), 1000);
        let (bench, _) = parse("[bench]\nmax_duration = 1500\n").unwrap();
        assert_eq!(bench.max_duration, Duration::from_millis(1500));
    }

    #[test]
    fn thresholds_join_their_metric_and_condition() {
        let (bench, warnings) =
            parse("[bench.thresholds]\np95 = \"<300ms\"\n\"file:login:p99\" = \" < 1s!\"\n")
                .unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            bench.threshold_expressions(),
            ["file:login:p99< 1s!", "p95<300ms"]
        );
        assert!(parse("[bench.thresholds]\np95 = 3\n").is_err());
    }

    #[test]
    fn stages_are_read_in_order() {
        let text = "[[bench.stages]]\nduration = \"30s\"\ntarget = 20\n\
                    [[bench.stages]]\nduration = 500\ntarget = 0\n";
        let (bench, warnings) = parse(text).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            bench.stages,
            [
                BenchStage {
                    duration: Duration::from_secs(30),
                    target: 20
                },
                BenchStage {
                    duration: Duration::from_millis(500),
                    target: 0
                },
            ]
        );
        assert!(parse("").unwrap().0.stages.is_empty());
    }

    #[test]
    fn invalid_values_are_errors() {
        for bad in [
            "[bench]\nmax_vus = 0\n",
            "[bench]\nmax_rps = -1\n",
            "[bench]\nmax_duration = \"soon\"\n",
            "[bench]\nmax_duration = 0\n",
            "[bench]\nmax_users = 3\n",
            "[[bench.stages]]\nduration = \"0s\"\ntarget = 2\n",
            "[[bench.stages]]\nduration = \"1s\"\n",
            "[[bench.stages]]\nduration = \"1s\"\ntarget = -1\n",
            "[[bench.stages]]\nduration = \"1s\"\ntarget = 1\nvus = 3\n",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }
}
