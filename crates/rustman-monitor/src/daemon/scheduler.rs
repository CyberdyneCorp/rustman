//! When each monitor is due next. Pure: every function takes `now`.
//!
//! At start (and for a monitor added later) an `every` monitor is due at
//! once and a `cron` monitor at its next occurrence. After a run comes due,
//! the next due time follows from it; when the daemon woke up late (the
//! laptop slept), from `now` instead, so missed runs leave a gap rather
//! than a burst. A monitor whose schedule changes is rescheduled from
//! `now`; one whose schedule is unchanged keeps its due time.

use std::collections::BTreeMap;

use jiff::Timestamp;

use crate::definition::Monitor;
use crate::schedule::Schedule;

#[derive(Debug, Default)]
pub struct Scheduler {
    entries: BTreeMap<String, Entry>,
}

#[derive(Debug)]
struct Entry {
    schedule: Schedule,
    /// `None` when a cron expression never matches again.
    next: Option<Timestamp>,
}

impl Scheduler {
    /// Follow `monitors`: add new ones, reschedule changed ones and drop
    /// removed ones.
    pub fn sync(&mut self, monitors: &[Monitor], now: Timestamp) {
        self.entries
            .retain(|name, _| monitors.iter().any(|m| &m.name == name));
        for monitor in monitors {
            let key = monitor.schedule.to_string();
            match self.entries.get_mut(&monitor.name) {
                Some(entry) if entry.schedule.to_string() == key => {}
                Some(entry) => {
                    entry.next = monitor.schedule.next_after(now);
                    entry.schedule = monitor.schedule.clone();
                }
                None => {
                    let entry = Entry {
                        next: first_due(&monitor.schedule, now),
                        schedule: monitor.schedule.clone(),
                    };
                    self.entries.insert(monitor.name.clone(), entry);
                }
            }
        }
    }

    /// The earliest due time.
    pub fn next_due(&self) -> Option<Timestamp> {
        self.entries.values().filter_map(|e| e.next).min()
    }

    /// The due time of `name`.
    pub fn due(&self, name: &str) -> Option<Timestamp> {
        self.entries.get(name).and_then(|e| e.next)
    }

    /// The monitors due at `now` with their due times, each moved on to
    /// its next due time.
    pub fn take_due(&mut self, now: Timestamp) -> Vec<(String, Timestamp)> {
        let mut due = Vec::new();
        for (name, entry) in &mut self.entries {
            let Some(at) = entry.next.filter(|at| *at <= now) else {
                continue;
            };
            let next = entry.schedule.next_after(at);
            entry.next = match next {
                Some(next) if next <= now => entry.schedule.next_after(now),
                other => other,
            };
            due.push((name.clone(), at));
        }
        due
    }
}

fn first_due(schedule: &Schedule, now: Timestamp) -> Option<Timestamp> {
    match schedule {
        Schedule::Every(_) => Some(now),
        Schedule::Cron(_) => schedule.next_after(now),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use jiff::SignedDuration;

    use super::*;
    use crate::definition::parse;

    fn monitor(name: &str, schedule: &str) -> Monitor {
        let text = format!(
            "version: 1\nname: {name}\ntargets: [health/]\nenvironment: prod\nschedule: {schedule}\n"
        );
        let file = format!("monitors/{name}.rman-monitor.yaml");
        parse(Path::new(&file), &text).unwrap()
    }

    fn at(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    const MIN: SignedDuration = SignedDuration::from_secs(60);

    #[test]
    fn every_runs_at_start_and_cron_at_its_next_occurrence() {
        let now = at("2026-03-01T10:02:00Z");
        let mut s = Scheduler::default();
        s.sync(
            &[
                monitor("a", "{every: 5m}"),
                monitor("b", "{cron: \"*/15 * * * *\", tz: UTC}"),
            ],
            now,
        );
        assert_eq!(s.due("a"), Some(now));
        assert_eq!(s.due("b"), Some(at("2026-03-01T10:15:00Z")));
        assert_eq!(s.next_due(), Some(now));
        assert_eq!(s.take_due(now), [("a".to_owned(), now)]);
        assert_eq!(s.due("a"), Some(now + MIN * 5));
        assert!(s.take_due(now + MIN).is_empty());
    }

    #[test]
    fn a_late_wake_up_leaves_a_gap_instead_of_a_burst() {
        let start = at("2026-03-01T10:00:00Z");
        let mut s = Scheduler::default();
        s.sync(&[monitor("a", "{every: 5m}")], start);
        s.take_due(start);
        // Asleep for an hour: one run, then every 5 minutes from now.
        let late = start + MIN * 61;
        assert_eq!(s.take_due(late), [("a".to_owned(), start + MIN * 5)]);
        assert_eq!(s.due("a"), Some(late + MIN * 5));
    }

    /// monitors "File edited while running": a schedule changed from 5m to
    /// 1m applies from now; an unchanged one keeps its due time; removed
    /// monitors are dropped and new ones added.
    #[test]
    fn sync_applies_schedule_changes() {
        let start = at("2026-03-01T10:00:00Z");
        let mut s = Scheduler::default();
        s.sync(
            &[monitor("a", "{every: 5m}"), monitor("b", "{every: 10m}")],
            start,
        );
        s.take_due(start);
        let now = start + MIN * 2;
        s.sync(
            &[
                monitor("a", "{every: 1m}"),
                monitor("b", "{every: 10m}"),
                monitor("c", "{every: 1h}"),
            ],
            now,
        );
        assert_eq!(s.due("a"), Some(now + MIN));
        assert_eq!(s.due("b"), Some(start + MIN * 10));
        assert_eq!(s.due("c"), Some(now));
        s.sync(&[monitor("b", "{every: 10m}")], now);
        assert_eq!(s.due("a"), None);
        assert_eq!(s.next_due(), Some(start + MIN * 10));
    }
}
