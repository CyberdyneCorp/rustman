//! Retention and compaction (monitors "Retention and compaction"): raw
//! results are kept for 7 days, then replaced by hourly aggregates, which
//! are kept for `[monitors].retention` (30 days by default).
//!
//! A raw day file is compacted once the whole UTC day is older than 7
//! days. Its aggregates replace any aggregates of the same hours, so a
//! compaction interrupted between writing `hourly.jsonl` and deleting the
//! day file gives the same result when it runs again.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use super::{
    COMPACT_LOCK, Outcome, RunResult, State, Store, StoreError, io_error, lock_file, read_lines,
    replace_file,
};

/// How long raw results are kept before compaction.
pub const RAW_RETENTION: Duration = Duration::from_secs(7 * 86_400);
/// How often compaction runs.
const INTERVAL: Duration = Duration::from_secs(86_400);

/// The runs of one hour.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HourlyAggregate {
    /// The start of the hour (UTC).
    pub hour: Timestamp,
    /// Every run of the hour, skipped ones included.
    pub runs: u32,
    pub passes: u32,
    pub failures: u32,
    pub errors: u32,
    pub skipped: u32,
    /// Latency over the runs that executed (not skipped).
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub max_ms: f64,
}

/// What one compaction did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Compaction {
    /// Raw day files replaced by aggregates.
    pub days_compacted: usize,
    /// Aggregates written for those days.
    pub hours_written: usize,
    /// Aggregates deleted because they passed the retention.
    pub hours_expired: usize,
}

/// Whether compaction should run: never ran, or last ran a day ago or more.
pub fn compaction_due(state: &State, now: Timestamp) -> bool {
    state
        .compacted_at
        .is_none_or(|at| now.duration_since(at) >= signed(INTERVAL))
}

impl Store {
    /// Compact the raw days of `name` older than [`RAW_RETENTION`] and
    /// delete aggregates older than `retention`, then record the time.
    pub fn compact(
        &self,
        name: &str,
        now: Timestamp,
        retention: Duration,
    ) -> Result<Compaction, StoreError> {
        let dir = self.monitor_dir(name);
        let _lock = lock_file(&dir.join(COMPACT_LOCK))?;
        let raw_cutoff = now - signed(RAW_RETENTION);
        let keep_after = now - signed(retention);
        let old_days: Vec<_> = self
            .day_files(name)?
            .into_iter()
            .filter(|(date, _)| day_end(*date).is_some_and(|end| end <= raw_cutoff))
            .collect();
        let mut points = Vec::new();
        for (_, path) in &old_days {
            points.extend(read_lines::<RunResult>(path)?);
        }
        let fresh = aggregate(&points);
        let fresh_hours: BTreeSet<Timestamp> = fresh.iter().map(|h| h.hour).collect();
        let existing = self.hourly(name)?;
        let before = existing.len();
        let mut kept: Vec<HourlyAggregate> = existing
            .into_iter()
            .filter(|h| !fresh_hours.contains(&h.hour))
            .chain(fresh)
            .filter(|h| h.hour >= keep_after)
            .collect();
        kept.sort_by_key(|h| h.hour);
        let report = Compaction {
            days_compacted: old_days.len(),
            hours_written: fresh_hours.len(),
            hours_expired: (before + fresh_hours.len()).saturating_sub(kept.len()),
        };
        if report != Compaction::default() {
            replace_file(&self.hourly_file(name), jsonl(&kept).as_bytes())?;
        }
        for (_, path) in &old_days {
            fs::remove_file(path).map_err(io_error(path))?;
        }
        self.update_state(name, |state| state.compacted_at = Some(now))?;
        Ok(report)
    }

    /// Compact when [`compaction_due`]: at the end of a one-shot run, and
    /// daily in the daemon.
    pub fn compact_if_due(
        &self,
        name: &str,
        now: Timestamp,
        retention: Duration,
    ) -> Result<Option<Compaction>, StoreError> {
        if !compaction_due(&self.state(name)?, now) {
            return Ok(None);
        }
        self.compact(name, now, retention).map(Some)
    }
}

/// The hourly aggregates of `points`, oldest first.
pub(crate) fn aggregate(points: &[RunResult]) -> Vec<HourlyAggregate> {
    let mut hours: BTreeMap<Timestamp, Vec<&RunResult>> = BTreeMap::new();
    for point in points {
        hours.entry(hour_of(point.at)).or_default().push(point);
    }
    hours
        .into_iter()
        .map(|(hour, runs)| hourly(hour, &runs))
        .collect()
}

fn hourly(hour: Timestamp, runs: &[&RunResult]) -> HourlyAggregate {
    let count = |outcome| u32::try_from(runs.iter().filter(|r| r.outcome == outcome).count());
    let count = |outcome| count(outcome).unwrap_or(u32::MAX);
    let mut latencies: Vec<f64> = runs
        .iter()
        .filter(|r| r.outcome != Outcome::Skipped)
        .map(|r| r.duration_ms)
        .collect();
    latencies.sort_by(f64::total_cmp);
    HourlyAggregate {
        hour,
        runs: u32::try_from(runs.len()).unwrap_or(u32::MAX),
        passes: count(Outcome::Pass),
        failures: count(Outcome::Fail),
        errors: count(Outcome::Error),
        skipped: count(Outcome::Skipped),
        p50_ms: percentile(&latencies, 50.0),
        p95_ms: percentile(&latencies, 95.0),
        max_ms: latencies.last().copied().unwrap_or(0.0),
    }
}

/// The nearest-rank percentile `p` (0-100) of `sorted`; 0 when empty.
pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (p / 100.0 * sorted.len() as f64).ceil();
    // `rank` is between 0 and len, so the cast cannot truncate.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let index = (rank as usize).clamp(1, sorted.len()) - 1;
    sorted[index]
}

/// The start of the UTC hour of `at`.
pub(crate) fn hour_of(at: Timestamp) -> Timestamp {
    let second = at.as_second().div_euclid(3600) * 3600;
    Timestamp::from_second(second).expect("an hour start is in range")
}

/// The first instant after the UTC day `date`.
fn day_end(date: jiff::civil::Date) -> Option<Timestamp> {
    let next = date.tomorrow().ok()?;
    Some(next.to_zoned(jiff::tz::TimeZone::UTC).ok()?.timestamp())
}

fn signed(duration: Duration) -> SignedDuration {
    SignedDuration::try_from(duration).unwrap_or(SignedDuration::MAX)
}

fn jsonl<T: Serialize>(values: &[T]) -> String {
    values
        .iter()
        .map(|v| serde_json::to_string(v).expect("aggregates serialize") + "\n")
        .collect()
}
