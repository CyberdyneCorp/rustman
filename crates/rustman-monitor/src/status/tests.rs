use std::path::Path;

use super::*;
use crate::definition::parse;
use crate::store::Counts;

const MIN: SignedDuration = SignedDuration::from_secs(60);

fn now() -> Timestamp {
    "2026-03-20T12:00:00Z".parse().unwrap()
}

fn monitor() -> Monitor {
    let text =
        "version: 1\nname: api\ntargets: [health/]\nenvironment: prod\nschedule: {every: 5m}\n";
    parse(Path::new("monitors/api.rman-monitor.yaml"), text).unwrap()
}

fn result(at: Timestamp, outcome: Outcome, ms: f64) -> RunResult {
    RunResult {
        at,
        outcome,
        duration_ms: ms,
        summary: Counts::default(),
        requests: Vec::new(),
        failures: if outcome == Outcome::Fail {
            vec!["health/status: status: expected 200, got 503".into()]
        } else {
            Vec::new()
        },
        message: None,
    }
}

/// 288 runs every 5 minutes over the last 24 hours, newest first: runs 10,
/// 20 and 30 failed, run 40 was skipped.
fn day_of_runs(store: &Store) {
    for i in 0..288 {
        let outcome = match i {
            10 | 20 | 30 => Outcome::Fail,
            40 => Outcome::Skipped,
            _ => Outcome::Pass,
        };
        let at = now() - MIN * (5 * i + 1);
        store
            .record("api", &result(at, outcome, 100.0 + f64::from(i % 24)))
            .unwrap();
    }
}

/// monitors "Uptime calculation": 288 runs with 3 failures and 1 skipped
/// is 98.95% (284/287).
#[test]
fn uptime_counts_passes_over_executed_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::new(tmp.path());
    day_of_runs(&store);
    let status = status(&store, &monitor(), now()).unwrap();
    assert_eq!(status.uptime.day, Some(98.95));
    assert_eq!(status.uptime.week, Some(98.95));
    assert_eq!(status.coverage_24h, Some(99.65), "287 of 288");
    assert_eq!(status.last.unwrap().at, now() - MIN * (5 * 287 + 1));
    assert_eq!(status.hourly_p95_ms.len(), 24);
    assert!(status.hourly_p95_ms.iter().all(Option::is_some));
    assert_eq!(status.sparkline.chars().count(), 24);
    assert!(status.p95_24h_ms.unwrap() >= 121.0);
}

/// Older periods count the hourly aggregates compaction left.
#[test]
fn longer_periods_include_compacted_hours() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::new(tmp.path());
    let old = now() - SignedDuration::from_hours(24 * 10);
    for i in 0..4 {
        let outcome = if i == 0 {
            Outcome::Error
        } else {
            Outcome::Pass
        };
        store
            .record("api", &result(old + MIN * i, outcome, 50.0))
            .unwrap();
    }
    store
        .record("api", &result(now() - MIN, Outcome::Pass, 50.0))
        .unwrap();
    store
        .compact("api", now(), Duration::from_secs(30 * 86_400))
        .unwrap();
    assert_eq!(store.hourly("api").unwrap().len(), 1);
    let status = status(&store, &monitor(), now()).unwrap();
    assert_eq!(status.uptime.day, Some(100.0));
    assert_eq!(status.uptime.week, Some(100.0));
    assert_eq!(status.uptime.month, Some(80.0), "4 of 5");
}

#[test]
fn a_monitor_without_runs_has_no_figures() {
    let tmp = tempfile::tempdir().unwrap();
    let status = status(&Store::new(tmp.path()), &monitor(), now()).unwrap();
    assert_eq!(status.uptime.day, None);
    assert_eq!(status.p95_24h_ms, None);
    assert_eq!(status.coverage_24h, None);
    assert_eq!(status.sparkline, " ".repeat(24));
}

#[test]
fn sparklines_scale_between_the_lowest_and_highest_hour() {
    assert_eq!(sparkline(&[Some(1.0), None, Some(8.0), Some(4.5)]), "▁ █▅");
    assert_eq!(sparkline(&[Some(3.0), Some(3.0)]), "▄▄");
}

/// monitors "Filter failures": `--since 24h --outcome fail` lists only the
/// failed results of the last 24 hours, newest first.
#[test]
fn history_filters_by_age_and_outcome() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::new(tmp.path());
    day_of_runs(&store);
    let older = now() - SignedDuration::from_hours(30);
    store
        .record("api", &result(older, Outcome::Fail, 1.0))
        .unwrap();
    let day = Some(Duration::from_secs(86_400));
    let failed = history(&store, "api", day, Some(Outcome::Fail), now()).unwrap();
    let times: Vec<Timestamp> = failed.iter().map(HistoryEntry::at).collect();
    assert_eq!(
        times,
        [now() - MIN * 51, now() - MIN * 101, now() - MIN * 151]
    );
    let all = history(&store, "api", None, Some(Outcome::Fail), now()).unwrap();
    assert_eq!(all.len(), 4);
    let everything = history(&store, "api", day, None, now()).unwrap();
    assert_eq!(everything.len(), 288);
}

/// Compacted points are listed as hourly aggregates.
#[test]
fn history_shows_compacted_hours_as_aggregates() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::new(tmp.path());
    let old = now() - SignedDuration::from_hours(24 * 10);
    store
        .record("api", &result(old, Outcome::Fail, 50.0))
        .unwrap();
    store
        .record("api", &result(old + MIN, Outcome::Pass, 50.0))
        .unwrap();
    store
        .record("api", &result(now() - MIN, Outcome::Fail, 50.0))
        .unwrap();
    store
        .compact("api", now(), Duration::from_secs(30 * 86_400))
        .unwrap();
    let failed = history(&store, "api", None, Some(Outcome::Fail), now()).unwrap();
    assert!(matches!(failed[0], HistoryEntry::Run(_)));
    let HistoryEntry::Hour(hour) = &failed[1] else {
        panic!("{failed:?}");
    };
    assert_eq!((hour.runs, hour.failures), (2, 1));
    let passes = history(
        &store,
        "api",
        Some(Duration::from_secs(86_400)),
        Some(Outcome::Pass),
        now(),
    );
    assert!(passes.unwrap().is_empty());
}
