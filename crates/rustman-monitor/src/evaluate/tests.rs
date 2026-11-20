use std::time::Duration;

use jiff::{SignedDuration, Timestamp};

use super::*;
use crate::alerts::StatusMatch;
use crate::store::{Counts, RequestResult};

const MIN: SignedDuration = SignedDuration::from_secs(60);

fn t0() -> Timestamp {
    "2026-03-01T10:00:00Z".parse().unwrap()
}

/// A run `minutes` after 10:00 with `outcome` and latency `ms`.
fn run(minutes: i32, outcome: Outcome, ms: f64) -> RunResult {
    RunResult {
        at: t0() + MIN * minutes,
        outcome,
        duration_ms: ms,
        summary: Counts::default(),
        requests: Vec::new(),
        failures: Vec::new(),
        message: None,
    }
}

fn with_request(mut run: RunResult, id: &str, status: &str, code: u16) -> RunResult {
    run.requests.push(RequestResult {
        id: id.into(),
        status: status.into(),
        code: Some(code),
        duration_ms: run.duration_ms,
    });
    run
}

fn alerts(rules: Vec<Rule>) -> Alerts {
    Alerts {
        rules,
        ..Alerts::default()
    }
}

/// Whether `rule` fires after the last of `runs`, from a clean state.
fn fires(rule: Rule, runs: &[RunResult]) -> bool {
    let (state, notices) = evaluate(&alerts(vec![rule]), runs, &AlertState::default());
    assert_eq!(state.firing.len(), notices.len());
    !notices.is_empty()
}

use Outcome::{Error, Fail, Pass, Skipped};

#[test]
fn each_rule_fires_only_when_its_condition_holds() {
    let window = Duration::from_secs(15 * 60);
    let fails = |n: u32| Rule::FailuresInWindow {
        count: n,
        window: Duration::from_secs(600),
    };
    let latency = Rule::LatencyP95 {
        threshold_ms: 800,
        window,
    };
    let health = || with_request(run(0, Fail, 10.0), "health/status", "failed", 503);
    let cases: Vec<(&str, Rule, Vec<RunResult>, bool)> = vec![
        (
            "two failures in a row",
            Rule::ConsecutiveFailures(2),
            vec![run(0, Fail, 1.0), run(5, Error, 1.0)],
            true,
        ),
        (
            "a pass between",
            Rule::ConsecutiveFailures(2),
            vec![run(0, Fail, 1.0), run(5, Pass, 1.0), run(10, Fail, 1.0)],
            false,
        ),
        (
            "skipped runs do not break a streak",
            Rule::ConsecutiveFailures(2),
            vec![run(0, Fail, 1.0), run(5, Skipped, 0.0), run(10, Fail, 1.0)],
            true,
        ),
        (
            "too few runs",
            Rule::ConsecutiveFailures(3),
            vec![run(0, Fail, 1.0), run(5, Fail, 1.0)],
            false,
        ),
        (
            "failures inside the window",
            fails(2),
            vec![run(0, Fail, 1.0), run(5, Pass, 1.0), run(10, Error, 1.0)],
            true,
        ),
        (
            "an old failure is outside the window",
            fails(2),
            vec![run(0, Fail, 1.0), run(11, Pass, 1.0), run(12, Fail, 1.0)],
            false,
        ),
        (
            "slow runs in the window",
            latency.clone(),
            vec![run(0, Pass, 950.0), run(5, Pass, 950.0)],
            true,
        ),
        (
            "fast runs",
            latency.clone(),
            vec![run(0, Pass, 100.0), run(5, Pass, 800.0)],
            false,
        ),
        (
            "slow runs before the window",
            latency,
            vec![run(0, Pass, 2000.0), run(20, Pass, 100.0)],
            false,
        ),
        (
            "the named request failed",
            Rule::AssertionFailed("health/status".into()),
            vec![health()],
            true,
        ),
        (
            "another request failed",
            Rule::AssertionFailed("users/list".into()),
            vec![health()],
            false,
        ),
        (
            "a status outside the class",
            Rule::StatusNot(StatusMatch::Class(2)),
            vec![health()],
            true,
        ),
        (
            "the expected code",
            Rule::StatusNot(StatusMatch::Code(503)),
            vec![health()],
            false,
        ),
    ];
    for (name, rule, runs, want) in cases {
        assert_eq!(fires(rule, &runs), want, "{name}");
    }
}

/// monitors "Consecutive failures": fires after the third run, not before.
#[test]
fn consecutive_failures_fire_after_the_third_run_and_not_before() {
    let rules = alerts(vec![Rule::ConsecutiveFailures(3)]);
    let mut window = Vec::new();
    let mut state = AlertState::default();
    let mut fired_after = Vec::new();
    for i in 0..4 {
        window.push(run(i * 5, Fail, 10.0));
        let (next, notices) = evaluate(&rules, &window, &state);
        if notices.iter().any(|n| n.kind == NoticeKind::Firing) {
            fired_after.push(i + 1);
        }
        state = next;
    }
    assert_eq!(fired_after, [3]);
}

/// monitors "Latency rule": p95 950 ms over 15 minutes with an 800 ms
/// threshold fires.
#[test]
fn latency_rule_fires_on_a_slow_p95() {
    let rule = Rule::LatencyP95 {
        threshold_ms: 800,
        window: Duration::from_secs(15 * 60),
    };
    let mut runs: Vec<RunResult> = (0..19).map(|i| run(i % 15, Pass, 200.0)).collect();
    runs.push(run(14, Pass, 950.0));
    runs.sort_by_key(|r| r.at);
    // 1 in 20 is slow: the nearest-rank p95 is the 19th value, 200 ms.
    assert!(!fires(rule.clone(), &runs));
    runs.push(run(15, Pass, 950.0));
    assert!(fires(rule, &runs));
}

/// monitors "No repeat spam": fired 10 minutes ago with a 1h cooldown, a
/// failing run sends nothing; after the cooldown a reminder goes out.
#[test]
fn a_firing_rule_notifies_once_then_reminds_after_the_cooldown() {
    let rules = alerts(vec![Rule::ConsecutiveFailures(1)]);
    let (state, notices) = evaluate(&rules, &[run(0, Fail, 1.0)], &AlertState::default());
    assert_eq!(notices.len(), 1);
    let window = [run(0, Fail, 1.0), run(10, Fail, 1.0)];
    let (state, notices) = evaluate(&rules, &window, &state);
    assert!(notices.is_empty(), "{notices:?}");
    let window = [run(0, Fail, 1.0), run(10, Fail, 1.0), run(60, Fail, 1.0)];
    let (state, notices) = evaluate(&rules, &window, &state);
    assert_eq!(
        notices[0].kind,
        NoticeKind::Reminder { since: t0() },
        "{notices:?}"
    );
    let firing = state.firing["consecutive_failures: 1"];
    assert_eq!(firing.since, t0());
    assert_eq!(firing.last_notified, t0() + MIN * 60);
}

/// monitors "Recovery notice": firing since 10:00, resolved by a passing
/// run at 10:42, the notice says 42 minutes.
#[test]
fn a_recovery_notice_carries_the_downtime() {
    let rules = alerts(vec![Rule::ConsecutiveFailures(1)]);
    let (state, _) = evaluate(&rules, &[run(0, Fail, 1.0)], &AlertState::default());
    let window = [run(0, Fail, 1.0), run(42, Pass, 1.0)];
    let (state, notices) = evaluate(&rules, &window, &state);
    let downtime = Duration::from_secs(42 * 60);
    assert_eq!(
        notices,
        [Notice {
            rule: "consecutive_failures: 1".into(),
            kind: NoticeKind::Resolved { downtime },
            at: t0() + MIN * 42,
        }]
    );
    assert_eq!(describe(downtime), "42 minutes");
    assert!(state.is_empty());
}

#[test]
fn skipped_runs_and_removed_rules_change_nothing_or_are_forgotten() {
    let rules = alerts(vec![Rule::ConsecutiveFailures(1)]);
    let (state, _) = evaluate(&rules, &[run(0, Fail, 1.0)], &AlertState::default());
    let (same, notices) = evaluate(&rules, &[run(5, Skipped, 0.0)], &state);
    assert_eq!((same, notices), (state.clone(), Vec::new()));
    let other = alerts(vec![Rule::ConsecutiveFailures(5)]);
    let (next, notices) = evaluate(&other, &[run(0, Fail, 1.0), run(5, Fail, 1.0)], &state);
    assert!(next.is_empty() && notices.is_empty());
}

#[test]
fn downtimes_read_naturally() {
    let d = |s: u64| describe(Duration::from_secs(s));
    assert_eq!(d(45), "45 seconds");
    assert_eq!(d(60), "1 minute");
    assert_eq!(d(3600), "1 hour");
    assert_eq!(d(3 * 3600 + 300), "3 hours 5 minutes");
    assert_eq!(d(2 * 86_400 + 3600), "2 days 1 hour");
}

/// The state lives in `state.json`: a second store over the same folder
/// (another process, a one-shot run) sees the rule firing and does not
/// notify again.
#[test]
fn alert_state_persists_between_runs_and_processes() {
    let tmp = tempfile::tempdir().unwrap();
    let rules = alerts(vec![Rule::ConsecutiveFailures(2)]);
    let daemon = Store::new(tmp.path());
    let cron = Store::new(tmp.path());
    let mut sent = Vec::new();
    for (i, (store, outcome)) in [
        (&daemon, Fail),
        (&cron, Fail),
        (&daemon, Fail),
        (&cron, Pass),
    ]
    .into_iter()
    .enumerate()
    {
        let result = run(i32::try_from(i).unwrap() * 5, outcome, 1.0);
        store.record("api", &result).unwrap();
        sent.extend(after_run(store, "api", &rules, result.at).unwrap());
    }
    let kinds: Vec<NoticeKind> = sent.iter().map(|n| n.kind).collect();
    assert_eq!(
        kinds,
        [
            NoticeKind::Firing,
            NoticeKind::Resolved {
                downtime: Duration::from_secs(600)
            }
        ]
    );
    assert!(daemon.state("api").unwrap().alerts.is_empty());
}

/// Overlapping daemon and cron runs: the cron run at 10 passes and is
/// evaluated before the daemon's earlier failing run at 5. The late
/// evaluation reads the window under the state lock, sees the newer pass,
/// and sends no stale `[FIRING]`.
#[test]
fn a_late_evaluation_of_an_earlier_run_sends_no_stale_notice() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::new(tmp.path());
    let rules = alerts(vec![Rule::ConsecutiveFailures(2)]);
    for (minutes, outcome) in [(0, Fail), (5, Fail), (10, Pass)] {
        store.record("api", &run(minutes, outcome, 1.0)).unwrap();
    }
    let at = |minutes: i32| t0() + MIN * minutes;
    assert!(after_run(&store, "api", &rules, at(10)).unwrap().is_empty());
    assert!(after_run(&store, "api", &rules, at(5)).unwrap().is_empty());
    assert!(store.state("api").unwrap().alerts.is_empty());
}
