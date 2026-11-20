//! The data of the viz companion's monitor trend view (monitors "Trend
//! charts in the viz companion"): uptime gauges, the p95 latency of each
//! hour, and outcomes by UTC hour of day, over the last 30 days of the
//! store (raw results, then the hourly aggregates compaction left). Counts
//! and timings only: nothing to redact.

use std::collections::BTreeMap;

use jiff::Timestamp;
use serde::Serialize;

use super::{DAY, MonitorStatus, Uptime, compacted, signed};
use crate::store::{HourlyAggregate, Outcome, RunResult, Store, StoreError, hour_of, percentile};

const SECONDS_PER_HOUR: i64 = 3600;

/// One monitor's trend.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Trend {
    pub name: String,
    pub environment: String,
    pub schedule: String,
    pub uptime: Uptime,
    /// The p95 latency of each hour with executed runs, oldest first.
    pub latency: Vec<HourLatency>,
    /// Outcome counts by UTC hour of day: index 0 is 00:00 to 01:00.
    pub hour_of_day: [OutcomeCounts; 24],
}

/// One point of the latency plot.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct HourLatency {
    /// The start of the hour (UTC).
    pub hour: Timestamp,
    pub p95_ms: f64,
}

/// Runs by outcome.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct OutcomeCounts {
    pub passed: u64,
    pub failed: u64,
    pub errored: u64,
    pub skipped: u64,
}

impl OutcomeCounts {
    fn add(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Pass => self.passed += 1,
            Outcome::Fail => self.failed += 1,
            Outcome::Error => self.errored += 1,
            Outcome::Skipped => self.skipped += 1,
        }
    }

    fn add_hour(&mut self, hour: &HourlyAggregate) {
        self.passed += u64::from(hour.passes);
        self.failed += u64::from(hour.failures);
        self.errored += u64::from(hour.errors);
        self.skipped += u64::from(hour.skipped);
    }
}

/// The trend of the monitor of `status` at `now`; the uptime gauges are
/// the status's own.
pub fn trend(store: &Store, status: &MonitorStatus, now: Timestamp) -> Result<Trend, StoreError> {
    let since = now - signed(30 * DAY);
    let raw = store.raw_since(&status.name, since)?;
    let hours: Vec<HourlyAggregate> = compacted(store.hourly(&status.name)?, &raw)
        .into_iter()
        .filter(|h| h.hour >= since)
        .collect();
    Ok(Trend {
        name: status.name.clone(),
        environment: status.environment.clone(),
        schedule: status.schedule.clone(),
        uptime: status.uptime,
        latency: latency(&raw, &hours),
        hour_of_day: hour_of_day(&raw, &hours),
    })
}

/// Aggregated hours as they are, then the p95 of each raw hour.
fn latency(raw: &[RunResult], hours: &[HourlyAggregate]) -> Vec<HourLatency> {
    let mut by_hour: BTreeMap<Timestamp, Vec<f64>> = BTreeMap::new();
    for run in raw.iter().filter(|r| r.outcome != Outcome::Skipped) {
        by_hour
            .entry(hour_of(run.at))
            .or_default()
            .push(run.duration_ms);
    }
    let aggregated = hours
        .iter()
        .filter(|h| h.runs > h.skipped)
        .map(|h| HourLatency {
            hour: h.hour,
            p95_ms: h.p95_ms,
        });
    let recent = by_hour.into_iter().map(|(hour, mut latencies)| {
        latencies.sort_by(f64::total_cmp);
        HourLatency {
            hour,
            p95_ms: percentile(&latencies, 95.0),
        }
    });
    let mut points: Vec<HourLatency> = aggregated.chain(recent).collect();
    points.sort_by_key(|p| p.hour);
    points
}

fn hour_of_day(raw: &[RunResult], hours: &[HourlyAggregate]) -> [OutcomeCounts; 24] {
    let mut cells = [OutcomeCounts::default(); 24];
    for run in raw {
        cells[utc_hour(run.at)].add(run.outcome);
    }
    for hour in hours {
        cells[utc_hour(hour.hour)].add_hour(hour);
    }
    cells
}

/// The UTC hour of day of `at`, 0 to 23.
fn utc_hour(at: Timestamp) -> usize {
    let hour = at.as_second().rem_euclid(24 * SECONDS_PER_HOUR) / SECONDS_PER_HOUR;
    usize::try_from(hour).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use jiff::SignedDuration;

    use super::*;
    use crate::store::Counts;

    const MIN: SignedDuration = SignedDuration::from_secs(60);

    fn now() -> Timestamp {
        "2026-03-20T12:30:00Z".parse().unwrap()
    }

    fn result(at: Timestamp, outcome: Outcome, ms: f64) -> RunResult {
        RunResult {
            at,
            outcome,
            duration_ms: ms,
            summary: Counts::default(),
            requests: Vec::new(),
            failures: Vec::new(),
            message: None,
        }
    }

    fn status() -> MonitorStatus {
        MonitorStatus {
            name: "api".into(),
            environment: "prod".into(),
            schedule: "every 5m".into(),
            last: None,
            uptime: Uptime {
                day: Some(50.0),
                week: Some(75.0),
                month: None,
            },
            p95_24h_ms: None,
            hourly_p95_ms: Vec::new(),
            sparkline: String::new(),
            firing: Vec::new(),
            coverage_24h: None,
        }
    }

    /// Raw runs give per-hour p95 points and heatmap cells by UTC hour;
    /// skipped runs count in the heatmap but not the latency.
    #[test]
    fn raw_runs_become_hourly_points_and_heatmap_cells() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::new(tmp.path());
        let runs = [
            (now() - MIN * 20, Outcome::Pass, 100.0),
            (now() - MIN * 25, Outcome::Fail, 300.0),
            (now() - MIN * 100, Outcome::Error, 50.0),
            (now() - MIN * 95, Outcome::Skipped, 0.0),
        ];
        for (at, outcome, ms) in runs {
            store.record("api", &result(at, outcome, ms)).unwrap();
        }
        let trend = trend(&store, &status(), now()).unwrap();
        assert_eq!(trend.uptime, status().uptime);
        let hour = |text: &str| text.parse::<Timestamp>().unwrap();
        assert_eq!(
            trend.latency,
            [
                HourLatency {
                    hour: hour("2026-03-20T10:00:00Z"),
                    p95_ms: 50.0
                },
                HourLatency {
                    hour: hour("2026-03-20T12:00:00Z"),
                    p95_ms: 300.0
                },
            ]
        );
        let cell = |passed, failed, errored, skipped| OutcomeCounts {
            passed,
            failed,
            errored,
            skipped,
        };
        assert_eq!(trend.hour_of_day[12], cell(1, 1, 0, 0));
        assert_eq!(trend.hour_of_day[10], cell(0, 0, 1, 1));
        let others: u64 = trend
            .hour_of_day
            .iter()
            .enumerate()
            .filter(|(i, _)| ![10, 12].contains(i))
            .map(|(_, c)| c.passed + c.failed + c.errored + c.skipped)
            .sum();
        assert_eq!(others, 0);
    }

    /// Compacted hours count too, and nothing older than 30 days does.
    #[test]
    fn aggregates_fill_older_hours_within_30_days() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::new(tmp.path());
        store
            .record("api", &result(now() - MIN * 10, Outcome::Pass, 80.0))
            .unwrap();
        let aggregate = |hour: &str, passes| HourlyAggregate {
            hour: hour.parse().unwrap(),
            runs: passes + 1,
            passes,
            failures: 1,
            errors: 0,
            skipped: 0,
            p50_ms: 10.0,
            p95_ms: 42.0,
            max_ms: 60.0,
        };
        let lines = [
            aggregate("2026-03-10T03:00:00Z", 11),
            aggregate("2026-01-01T03:00:00Z", 5),
        ];
        let text: String = lines
            .iter()
            .map(|h| format!("{}\n", serde_json::to_string(h).unwrap()))
            .collect();
        std::fs::write(store.monitor_dir("api").join("hourly.jsonl"), text).unwrap();
        let trend = trend(&store, &status(), now()).unwrap();
        assert_eq!(trend.latency.len(), 2, "{:?}", trend.latency);
        assert_eq!(trend.latency[0].p95_ms, 42.0);
        assert_eq!(trend.hour_of_day[3].passed, 11);
        assert_eq!(trend.hour_of_day[3].failed, 1);
    }
}
