use std::time::Duration;

use jiff::{SignedDuration, Timestamp};

use super::*;

fn at(text: &str) -> Timestamp {
    text.parse().unwrap()
}

fn result(at: Timestamp, outcome: Outcome, duration_ms: f64) -> RunResult {
    RunResult {
        at,
        outcome,
        duration_ms,
        summary: Counts {
            total: 1,
            passed: usize::from(outcome == Outcome::Pass),
            ..Counts::default()
        },
        requests: Vec::new(),
        failures: if outcome == Outcome::Fail {
            vec!["health/status: status: expected 200, got 503".into()]
        } else {
            Vec::new()
        },
        message: None,
    }
}

const THIRTY_DAYS: Duration = Duration::from_secs(30 * 86_400);

#[test]
fn results_go_to_their_utc_day_file_and_read_back_in_order() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::new(tmp.path());
    let late = result(at("2026-03-02T00:10:00Z"), Outcome::Pass, 120.0);
    let early = result(at("2026-03-01T23:50:00Z"), Outcome::Fail, 80.0);
    store.record("api", &late).unwrap();
    store.record("api", &early).unwrap();
    let day = store.day_file("api", early.at);
    assert!(
        day.ends_with("api/raw/2026-03-01.jsonl"),
        "{}",
        day.display()
    );
    assert!(
        tmp.path()
            .join(".rustman/monitors/api/raw/2026-03-02.jsonl")
            .is_file()
    );
    let all = store.raw_since("api", at("2026-01-01T00:00:00Z")).unwrap();
    assert_eq!(all, [early.clone(), late.clone()]);
    let recent = store.raw_since("api", at("2026-03-02T00:00:00Z")).unwrap();
    assert_eq!(recent, [late]);
    // The state holds the last recorded run.
    let state = store.state("api").unwrap();
    assert_eq!(state.last_run, Some(LastRun::from(&early)));
    assert!(
        store
            .raw_since("other", at("2026-01-01T00:00:00Z"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_failed_result_keeps_its_assertion_message() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::new(tmp.path());
    let failed = result(at("2026-03-01T10:00:00Z"), Outcome::Fail, 80.0);
    store.append("api-health", &failed).unwrap();
    let text = std::fs::read_to_string(store.day_file("api-health", failed.at)).unwrap();
    assert!(text.contains("\"outcome\":\"fail\""), "{text}");
    assert!(text.contains("expected 200, got 503"), "{text}");
}

#[test]
fn state_updates_keep_fields_of_other_versions() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::new(tmp.path());
    let dir = store.monitor_dir("api");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("state.json"), r#"{"future": {"r1": "firing"}}"#).unwrap();
    store
        .update_state("api", |s| s.compacted_at = Some(at("2026-03-01T00:00:00Z")))
        .unwrap();
    let state = store.state("api").unwrap();
    assert_eq!(state.compacted_at, Some(at("2026-03-01T00:00:00Z")));
    assert_eq!(state.other["future"]["r1"], "firing");
    // No temporary file is left next to it.
    let names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert!(
        names.iter().all(|n| n == "state.json" || n == "state.lock"),
        "{names:?}"
    );
    std::fs::write(dir.join("state.json"), "{nope").unwrap();
    let err = store.state("api").unwrap_err().to_string();
    assert!(err.contains("invalid state"), "{err}");
}

/// Two writers append to the same day file at once (as the daemon and a
/// one-shot run would): every line is whole and every result is there.
#[test]
fn concurrent_writers_never_corrupt_a_line() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let base = at("2026-03-01T10:00:00Z");
    // Long messages make a torn write visible.
    let message = "x".repeat(4000);
    let writers: Vec<_> = (0..2)
        .map(|w| {
            let root = root.clone();
            let message = message.clone();
            std::thread::spawn(move || {
                let store = Store::new(&root);
                for i in 0..200 {
                    let mut r = result(base + SignedDuration::from_secs(i), Outcome::Pass, 1.0);
                    r.message = Some(format!("{w}-{i}-{message}"));
                    store.record("api", &r).unwrap();
                }
            })
        })
        .collect();
    for writer in writers {
        writer.join().unwrap();
    }
    let store = Store::new(&root);
    let text = std::fs::read_to_string(store.day_file("api", base)).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 400);
    for line in lines {
        let parsed: RunResult = serde_json::from_str(line).expect("a whole line");
        assert!(parsed.message.unwrap().ends_with(&message));
    }
    assert!(store.state("api").unwrap().last_run.is_some());
}

#[test]
fn old_points_are_replaced_by_hourly_aggregates_with_the_same_counts() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::new(tmp.path());
    let now = at("2026-03-20T12:00:00Z");
    // Ten days ago: two hours of runs.
    let old = [
        result(at("2026-03-10T09:00:00Z"), Outcome::Pass, 100.0),
        result(at("2026-03-10T09:05:00Z"), Outcome::Fail, 300.0),
        result(at("2026-03-10T09:10:00Z"), Outcome::Pass, 200.0),
        result(at("2026-03-10T09:15:00Z"), Outcome::Error, 50.0),
        RunResult::skipped(at("2026-03-10T09:20:00Z")),
        result(at("2026-03-10T10:00:00Z"), Outcome::Fail, 400.0),
    ];
    // Yesterday: stays raw.
    let recent = result(at("2026-03-19T09:00:00Z"), Outcome::Pass, 90.0);
    for r in old.iter().chain([&recent]) {
        store.append("api", r).unwrap();
    }
    let report = store.compact("api", now, THIRTY_DAYS).unwrap();
    assert_eq!(
        report,
        Compaction {
            days_compacted: 1,
            hours_written: 2,
            hours_expired: 0
        }
    );
    assert!(!store.day_file("api", old[0].at).exists());
    let raw = store.raw_since("api", at("2026-01-01T00:00:00Z")).unwrap();
    assert_eq!(raw, [recent]);
    let hours = store.hourly("api").unwrap();
    assert_eq!(hours.len(), 2);
    let nine = &hours[0];
    assert_eq!(nine.hour, at("2026-03-10T09:00:00Z"));
    assert_eq!(
        (
            nine.runs,
            nine.passes,
            nine.failures,
            nine.errors,
            nine.skipped
        ),
        (5, 2, 1, 1, 1)
    );
    // Latency over the 4 executed runs: 50, 100, 200, 300.
    assert_eq!(
        (nine.p50_ms, nine.p95_ms, nine.max_ms),
        (100.0, 300.0, 300.0)
    );
    assert_eq!((hours[1].runs, hours[1].failures), (1, 1));
    assert_eq!(store.state("api").unwrap().compacted_at, Some(now));
    // Running again changes nothing.
    let again = store.compact("api", now, THIRTY_DAYS).unwrap();
    assert_eq!(again, Compaction::default());
    assert_eq!(store.hourly("api").unwrap(), hours);
}

/// A compaction cut short after writing the aggregates (the day file is
/// still there) gives the same aggregates when it runs again.
#[test]
fn an_interrupted_compaction_is_not_counted_twice() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::new(tmp.path());
    let point = result(at("2026-03-10T09:00:00Z"), Outcome::Pass, 100.0);
    store.append("api", &point).unwrap();
    let day = store.day_file("api", point.at);
    let kept = std::fs::read(&day).unwrap();
    let now = at("2026-03-20T12:00:00Z");
    store.compact("api", now, THIRTY_DAYS).unwrap();
    std::fs::write(&day, kept).unwrap();
    store.compact("api", now, THIRTY_DAYS).unwrap();
    let hours = store.hourly("api").unwrap();
    assert_eq!(hours.len(), 1);
    assert_eq!(hours[0].runs, 1);
}

#[test]
fn aggregates_past_the_retention_are_deleted() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::new(tmp.path());
    for day in ["2026-01-05", "2026-02-10", "2026-03-01"] {
        let r = result(at(&format!("{day}T08:00:00Z")), Outcome::Pass, 10.0);
        store.append("api", &r).unwrap();
    }
    // The first compaction, on Feb 20: Jan 5 and Feb 10 are older than 7
    // days; Jan 5 is also past the 30 days, so it is deleted at once.
    let report = store
        .compact("api", at("2026-02-20T00:00:00Z"), THIRTY_DAYS)
        .unwrap();
    assert_eq!(report.days_compacted, 2);
    assert_eq!(report.hours_expired, 1);
    let hours: Vec<_> = store
        .hourly("api")
        .unwrap()
        .iter()
        .map(|h| h.hour)
        .collect();
    assert_eq!(hours, [at("2026-02-10T08:00:00Z")]);
    // On Mar 15 the Feb 10 aggregate is 33 days old; Mar 1 is compacted.
    let report = store
        .compact("api", at("2026-03-15T00:00:00Z"), THIRTY_DAYS)
        .unwrap();
    assert_eq!((report.days_compacted, report.hours_expired), (1, 1));
    let hours: Vec<_> = store
        .hourly("api")
        .unwrap()
        .iter()
        .map(|h| h.hour)
        .collect();
    assert_eq!(hours, [at("2026-03-01T08:00:00Z")]);
    // A longer retention keeps more.
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::new(tmp.path());
    store
        .append(
            "api",
            &result(at("2026-01-05T08:00:00Z"), Outcome::Pass, 10.0),
        )
        .unwrap();
    let ninety = Duration::from_secs(90 * 86_400);
    store
        .compact("api", at("2026-02-20T00:00:00Z"), ninety)
        .unwrap();
    assert_eq!(store.hourly("api").unwrap().len(), 1);
}

#[test]
fn compaction_runs_once_a_day() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::new(tmp.path());
    let first = at("2026-03-01T00:00:00Z");
    assert!(compaction_due(&State::default(), first));
    assert!(
        store
            .compact_if_due("api", first, THIRTY_DAYS)
            .unwrap()
            .is_some()
    );
    let later = first + SignedDuration::from_hours(23);
    assert!(
        store
            .compact_if_due("api", later, THIRTY_DAYS)
            .unwrap()
            .is_none()
    );
    let next_day = first + SignedDuration::from_hours(24);
    assert!(
        store
            .compact_if_due("api", next_day, THIRTY_DAYS)
            .unwrap()
            .is_some()
    );
    assert_eq!(store.state("api").unwrap().compacted_at, Some(next_day));
}

#[test]
fn percentiles_use_the_nearest_rank() {
    assert_eq!(percentile(&[], 95.0), 0.0);
    assert_eq!(percentile(&[7.0], 95.0), 7.0);
    let values: Vec<f64> = (1..=100).map(f64::from).collect();
    assert_eq!(percentile(&values, 95.0), 95.0);
    assert_eq!(percentile(&values, 50.0), 50.0);
    assert_eq!(percentile(&values, 100.0), 100.0);
}
