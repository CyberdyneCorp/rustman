//! The alert state machine (monitors "Alert rules", "Deduplication,
//! cooldown and recovery", design D4), as one pure function:
//! [`evaluate`] takes the rules, the recent results and the previous state,
//! and returns the new state and the notices to send.
//!
//! Per rule: `ok → firing` sends a notice; `firing → firing` sends a
//! reminder only once `cooldown` has passed since the last notice;
//! `firing → ok` sends a recovery notice with the downtime. The state is
//! kept in `state.json`, so the daemon and one-shot runs share it.

use std::collections::BTreeMap;
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use crate::alerts::{Alerts, Rule};
use crate::store::{Outcome, RAW_RETENTION, RunResult, Store, StoreError, percentile};

/// The firing rules of a monitor, keyed by the rule as written
/// ([`Rule`]'s `Display`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlertState {
    #[serde(default)]
    pub firing: BTreeMap<String, Firing>,
}

impl AlertState {
    pub fn is_empty(&self) -> bool {
        self.firing.is_empty()
    }
}

/// A rule that is firing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Firing {
    /// The run that made it fire.
    pub since: Timestamp,
    /// The last notice sent for it (the first one, or a reminder).
    pub last_notified: Timestamp,
}

/// What a notice says about its rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeKind {
    /// The rule started firing.
    Firing,
    /// The rule is still firing, one cooldown after the last notice.
    Reminder { since: Timestamp },
    /// The rule resolved after `downtime`.
    Resolved { downtime: Duration },
}

/// One notification to send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    /// The rule as written.
    pub rule: String,
    pub kind: NoticeKind,
    /// The run that caused it.
    pub at: Timestamp,
}

/// The new alert state and the notices to send after the latest run of
/// `window` (results oldest first). Skipped runs are ignored; with no run
/// executed, nothing changes. Rules no longer in the file are forgotten
/// without a notice.
pub fn evaluate(
    alerts: &Alerts,
    window: &[RunResult],
    prev: &AlertState,
) -> (AlertState, Vec<Notice>) {
    let runs: Vec<&RunResult> = window
        .iter()
        .filter(|r| r.outcome != Outcome::Skipped)
        .collect();
    let Some(now) = runs.last().map(|r| r.at) else {
        return (prev.clone(), Vec::new());
    };
    let mut next = AlertState::default();
    let mut notices = Vec::new();
    for rule in &alerts.rules {
        let key = rule.to_string();
        let firing = holds(rule, &runs, now);
        let (state, kind) = transition(prev.firing.get(&key), firing, now, alerts.cooldown);
        if let Some(state) = state {
            next.firing.insert(key.clone(), state);
        }
        if let Some(kind) = kind {
            notices.push(Notice {
                rule: key,
                kind,
                at: now,
            });
        }
    }
    (next, notices)
}

/// One rule's step: its new firing state and the notice to send.
fn transition(
    prev: Option<&Firing>,
    holds: bool,
    now: Timestamp,
    cooldown: Duration,
) -> (Option<Firing>, Option<NoticeKind>) {
    match (prev, holds) {
        (None, false) => (None, None),
        (None, true) => {
            let firing = Firing {
                since: now,
                last_notified: now,
            };
            (Some(firing), Some(NoticeKind::Firing))
        }
        (Some(firing), true) if elapsed(firing.last_notified, now) >= cooldown => {
            let reminded = Firing {
                last_notified: now,
                ..*firing
            };
            let kind = NoticeKind::Reminder {
                since: firing.since,
            };
            (Some(reminded), Some(kind))
        }
        (Some(firing), true) => (Some(*firing), None),
        (Some(firing), false) => {
            let downtime = elapsed(firing.since, now);
            (None, Some(NoticeKind::Resolved { downtime }))
        }
    }
}

/// Whether `rule`'s condition holds after the last of `runs` (executed
/// runs, oldest first), at `now`.
fn holds(rule: &Rule, runs: &[&RunResult], now: Timestamp) -> bool {
    let last = runs.last().expect("at least one run");
    match rule {
        Rule::ConsecutiveFailures(n) => {
            let n = *n as usize;
            runs.len() >= n && runs.iter().rev().take(n).all(|r| failed(r))
        }
        Rule::FailuresInWindow { count, window } => {
            let failures = within(runs, now, *window).filter(|r| failed(r)).count();
            failures >= *count as usize
        }
        Rule::LatencyP95 {
            threshold_ms,
            window,
        } => {
            let mut latencies: Vec<f64> =
                within(runs, now, *window).map(|r| r.duration_ms).collect();
            latencies.sort_by(f64::total_cmp);
            // Thresholds are milliseconds, far below 2^52.
            #[allow(clippy::cast_precision_loss)]
            let threshold = *threshold_ms as f64;
            percentile(&latencies, 95.0) > threshold
        }
        Rule::AssertionFailed(request) => last
            .requests
            .iter()
            .any(|r| r.id == *request && r.status == "failed"),
        Rule::StatusNot(status) => last
            .requests
            .iter()
            .filter_map(|r| r.code)
            .any(|code| !status.matches(code)),
    }
}

fn failed(run: &RunResult) -> bool {
    matches!(run.outcome, Outcome::Fail | Outcome::Error)
}

/// The runs at or after `now - window`.
fn within<'a>(
    runs: &'a [&'a RunResult],
    now: Timestamp,
    window: Duration,
) -> impl Iterator<Item = &'a RunResult> + 'a {
    let start = now - SignedDuration::try_from(window).unwrap_or(SignedDuration::MAX);
    runs.iter().copied().filter(move |r| r.at >= start)
}

/// The time from `from` to `to`, zero when `to` is earlier.
pub fn elapsed(from: Timestamp, to: Timestamp) -> Duration {
    Duration::try_from(to.duration_since(from)).unwrap_or(Duration::ZERO)
}

/// A downtime for people: `42 minutes`, `3 hours 5 minutes`, `2 days 1
/// hour`, `45 seconds`.
pub fn describe(duration: Duration) -> String {
    let secs = duration.as_secs();
    let unit = |n: u64, name: &str| {
        let plural = if n == 1 { "" } else { "s" };
        format!("{n} {name}{plural}")
    };
    let (days, hours, minutes) = (secs / 86_400, secs % 86_400 / 3600, secs % 3600 / 60);
    let (big, small) = match (days, hours, minutes) {
        (0, 0, 0) => return unit(secs, "second"),
        (0, 0, m) => return unit(m, "minute"),
        (0, h, m) => (unit(h, "hour"), (m > 0).then(|| unit(m, "minute"))),
        (d, h, _) => (unit(d, "day"), (h > 0).then(|| unit(h, "hour"))),
    };
    match small {
        Some(small) => format!("{big} {small}"),
        None => big,
    }
}

/// Evaluate `alerts` for monitor `name` after a run at `now` stored in
/// `store`: the recent results are read and the alert state is changed
/// both under the state lock, so overlapping daemon and one-shot runs each
/// see the results and the state of the runs evaluated before them and
/// never send the same notice twice. The window is every stored result up
/// to the newest one, so a run evaluated late changes nothing a newer run
/// already decided.
pub fn after_run(
    store: &Store,
    name: &str,
    alerts: &Alerts,
    now: Timestamp,
) -> Result<Vec<Notice>, StoreError> {
    let lookback = SignedDuration::try_from(RAW_RETENTION).unwrap_or(SignedDuration::MAX);
    store.update_state(name, |state| {
        let window = store.raw_since(name, now - lookback)?;
        let (next, notices) = evaluate(alerts, &window, &state.alerts);
        state.alerts = next;
        Ok(notices)
    })?
}

#[cfg(test)]
mod tests;
