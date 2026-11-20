//! `rustman monitor status` and `rustman monitor history` (monitors
//! "Status command", "Result history command", design D7): read the store
//! only, never the network. Every function takes `now`.
//!
//! Uptime is passes over executed (not skipped) runs. Recent periods read
//! the raw results; the hours before the oldest raw result read the hourly
//! aggregates compaction left.

use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use serde::Serialize;

use crate::definition::Monitor;
use crate::evaluate::elapsed;
use crate::store::{HourlyAggregate, LastRun, Outcome, RunResult, Store, StoreError, percentile};

const HOUR: SignedDuration = SignedDuration::from_secs(3600);
const DAY: Duration = Duration::from_secs(86_400);
/// Sparkline levels, lowest first.
const LEVELS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
/// At most this many due times are counted for coverage (every 30 s for
/// a day).
const MAX_EXPECTED: usize = 2 * 2880;

/// What `status` shows for one monitor.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MonitorStatus {
    pub name: String,
    pub environment: String,
    pub schedule: String,
    pub last: Option<LastRun>,
    pub uptime: Uptime,
    /// The p95 latency of the last 24 hours.
    pub p95_24h_ms: Option<f64>,
    /// The p95 of each of the last 24 hours, oldest first; `None` for an
    /// hour without runs.
    pub hourly_p95_ms: Vec<Option<f64>>,
    /// [`MonitorStatus::hourly_p95_ms`] as text.
    pub sparkline: String,
    pub firing: Vec<FiringRule>,
    /// Runs executed over runs expected in the last 24 hours (or since the
    /// first run), as a percentage.
    pub coverage_24h: Option<f64>,
}

/// Uptime percentages, rounded to 2 decimals; `None` without runs.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Uptime {
    #[serde(rename = "24h")]
    pub day: Option<f64>,
    #[serde(rename = "7d")]
    pub week: Option<f64>,
    #[serde(rename = "30d")]
    pub month: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FiringRule {
    pub rule: String,
    pub since: Timestamp,
}

/// The status of `monitor` at `now`.
pub fn status(
    store: &Store,
    monitor: &Monitor,
    now: Timestamp,
) -> Result<MonitorStatus, StoreError> {
    let name = &monitor.name;
    let raw = store.raw_since(name, now - signed(30 * DAY))?;
    let hours = compacted(store.hourly(name)?, &raw);
    let state = store.state(name)?;
    let day_ago = now - signed(DAY);
    let mut recent: Vec<f64> = executed(&raw, day_ago).map(|r| r.duration_ms).collect();
    recent.sort_by(f64::total_cmp);
    let hourly_p95_ms = hourly_p95(&raw, now);
    Ok(MonitorStatus {
        name: name.clone(),
        environment: monitor.environment.clone(),
        schedule: monitor.schedule.to_string(),
        last: state.last_run,
        uptime: Uptime {
            day: uptime(&raw, &hours, day_ago),
            week: uptime(&raw, &hours, now - signed(7 * DAY)),
            month: uptime(&raw, &hours, now - signed(30 * DAY)),
        },
        p95_24h_ms: (!recent.is_empty()).then(|| percentile(&recent, 95.0)),
        sparkline: sparkline(&hourly_p95_ms),
        hourly_p95_ms,
        firing: state
            .alerts
            .firing
            .into_iter()
            .map(|(rule, firing)| FiringRule {
                rule,
                since: firing.since,
            })
            .collect(),
        coverage_24h: coverage(monitor, &raw, now),
    })
}

/// The aggregates of hours before the oldest raw result (an hour cut by an
/// interrupted compaction is not counted twice).
fn compacted(hours: Vec<HourlyAggregate>, raw: &[RunResult]) -> Vec<HourlyAggregate> {
    match raw.first() {
        Some(first) => hours
            .into_iter()
            .filter(|h| h.hour + HOUR <= first.at)
            .collect(),
        None => hours,
    }
}

fn executed(raw: &[RunResult], since: Timestamp) -> impl Iterator<Item = &RunResult> {
    raw.iter()
        .filter(move |r| r.at >= since && r.outcome != Outcome::Skipped)
}

/// Passes over executed runs since `since`, as a percentage.
fn uptime(raw: &[RunResult], hours: &[HourlyAggregate], since: Timestamp) -> Option<f64> {
    let (mut passes, mut runs) = (0u64, 0u64);
    for run in executed(raw, since) {
        runs += 1;
        passes += u64::from(run.outcome == Outcome::Pass);
    }
    for hour in hours.iter().filter(|h| h.hour >= since) {
        runs += u64::from(hour.passes + hour.failures + hour.errors);
        passes += u64::from(hour.passes);
    }
    (runs > 0).then(|| percent(passes, runs))
}

/// `part / whole` as a percentage with 2 decimals.
#[allow(clippy::cast_precision_loss)]
fn percent(part: u64, whole: u64) -> f64 {
    (part as f64 * 10_000.0 / whole as f64).round() / 100.0
}

/// The p95 of each of the 24 hours before `now`, oldest first.
fn hourly_p95(raw: &[RunResult], now: Timestamp) -> Vec<Option<f64>> {
    (0..24)
        .map(|i| {
            let start = now - HOUR * (24 - i);
            let end = start + HOUR;
            let mut latencies: Vec<f64> = executed(raw, start)
                .filter(|r| r.at < end)
                .map(|r| r.duration_ms)
                .collect();
            latencies.sort_by(f64::total_cmp);
            (!latencies.is_empty()).then(|| percentile(&latencies, 95.0))
        })
        .collect()
}

/// One block per value, scaled between the lowest and highest; a space
/// for a missing one.
pub fn sparkline(values: &[Option<f64>]) -> String {
    let present = values.iter().flatten();
    let low = present.clone().copied().fold(f64::INFINITY, f64::min);
    let high = present.copied().fold(f64::NEG_INFINITY, f64::max);
    values
        .iter()
        .map(|v| match v {
            None => ' ',
            Some(_) if high <= low => LEVELS[3],
            Some(v) => {
                let scaled = (v - low) / (high - low) * 7.0;
                // `scaled` is between 0 and 7.
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let level = scaled.round() as usize;
                LEVELS[level.min(7)]
            }
        })
        .collect()
}

/// Executed runs over due times in the last 24 hours, or since the first
/// run of that period.
fn coverage(monitor: &Monitor, raw: &[RunResult], now: Timestamp) -> Option<f64> {
    let day_ago = now - signed(DAY);
    let start = raw.iter().find(|r| r.at >= day_ago)?.at;
    let mut expected = 1u64;
    let mut at = start;
    while let Some(next) = monitor.schedule.next_after(at) {
        if next > now || expected as usize >= MAX_EXPECTED {
            break;
        }
        expected += 1;
        at = next;
    }
    let executed = executed(raw, start).count() as u64;
    Some(percent(executed.min(expected), expected))
}

/// One line of `history`: a stored run, or an hour compaction aggregated.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum HistoryEntry {
    Run(RunResult),
    Hour(HourlyAggregate),
}

impl HistoryEntry {
    pub fn at(&self) -> Timestamp {
        match self {
            Self::Run(run) => run.at,
            Self::Hour(hour) => hour.hour,
        }
    }
}

/// The stored results of `name` since `now - since` (all without), newest
/// first, keeping only `outcome` when given; compacted hours appear as
/// aggregates holding at least one such run.
pub fn history(
    store: &Store,
    name: &str,
    since: Option<Duration>,
    outcome: Option<Outcome>,
    now: Timestamp,
) -> Result<Vec<HistoryEntry>, StoreError> {
    let start = since
        .and_then(|since| now.checked_sub(signed(since)).ok())
        .unwrap_or(Timestamp::MIN);
    let raw = store.raw_since(name, start)?;
    let hours = compacted(store.hourly(name)?, &store.raw_since(name, Timestamp::MIN)?);
    let mut entries: Vec<HistoryEntry> = hours
        .into_iter()
        .filter(|h| h.hour + HOUR > start && outcome.is_none_or(|o| count(h, o) > 0))
        .map(HistoryEntry::Hour)
        .chain(
            raw.into_iter()
                .filter(|r| outcome.is_none_or(|o| r.outcome == o))
                .map(HistoryEntry::Run),
        )
        .collect();
    entries.sort_by_key(|e| std::cmp::Reverse(e.at()));
    Ok(entries)
}

fn count(hour: &HourlyAggregate, outcome: Outcome) -> u32 {
    match outcome {
        Outcome::Pass => hour.passes,
        Outcome::Fail => hour.failures,
        Outcome::Error => hour.errors,
        Outcome::Skipped => hour.skipped,
    }
}

fn signed(duration: Duration) -> SignedDuration {
    SignedDuration::try_from(duration).unwrap_or(SignedDuration::MAX)
}

/// How long ago `at` was, for people: `3 minutes ago`.
pub fn ago(at: Timestamp, now: Timestamp) -> String {
    format!("{} ago", crate::evaluate::describe(elapsed(at, now)))
}

pub mod trend;

#[cfg(test)]
mod tests;
