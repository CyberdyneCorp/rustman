//! When a monitor runs (monitors "Schedules", design D2): a 5-field cron
//! expression or `@hourly`, `@daily`, `@weekly`, evaluated in a timezone
//! (default: local), or a fixed interval from 30 s to 24 h.
//!
//! Cron times follow croner's DST rules: on the spring-forward day a fixed
//! time inside the gap runs right after it, and on the fall-back day it
//! runs once; wildcard and step fields run in real time through both.

use std::fmt;
use std::time::Duration;

use croner::Cron;
use croner::parser::{CronParser, Seconds, Year};
use jiff::Timestamp;
use jiff::tz::TimeZone;

use crate::duration;

/// The shortest `every`.
pub const MIN_EVERY: Duration = Duration::from_secs(30);
/// The longest `every`.
pub const MAX_EVERY: Duration = Duration::from_secs(24 * 3600);

/// The macros `cron` accepts besides 5-field expressions.
const MACROS: &[&str] = &["@hourly", "@daily", "@weekly"];

/// A monitor's schedule.
#[derive(Clone, Debug)]
pub enum Schedule {
    Cron(Box<CronSchedule>),
    Every(Duration),
}

/// A parsed cron expression and the timezone it is evaluated in.
#[derive(Clone, Debug)]
pub struct CronSchedule {
    expression: String,
    cron: Cron,
    tz: TimeZone,
    /// The `tz` the file names; `None` for the local timezone.
    tz_name: Option<String>,
}

impl Schedule {
    /// `every: <duration>`, between [`MIN_EVERY`] and [`MAX_EVERY`].
    pub fn every(text: &str) -> Result<Self, String> {
        let every = duration::parse(text)?;
        if every < MIN_EVERY {
            return Err("every must be at least 30s".to_owned());
        }
        if every > MAX_EVERY {
            return Err("every must be at most 24h".to_owned());
        }
        Ok(Self::Every(every))
    }

    /// `cron: <expression>` in timezone `tz` (an IANA name), or the local
    /// timezone.
    pub fn cron(expression: &str, tz: Option<&str>) -> Result<Self, String> {
        let expression = expression.trim();
        let cron = parse_cron(expression)?;
        let zone = match tz {
            Some(name) => TimeZone::get(name).map_err(|_| format!("unknown timezone '{name}'"))?,
            None => TimeZone::system(),
        };
        Ok(Self::Cron(Box::new(CronSchedule {
            expression: expression.to_owned(),
            cron,
            tz: zone,
            tz_name: tz.map(str::to_owned),
        })))
    }

    /// The first due time strictly after `after`; `None` when a cron
    /// expression never matches again.
    pub fn next_after(&self, after: Timestamp) -> Option<Timestamp> {
        match self {
            Self::Every(every) => {
                let span = jiff::SignedDuration::try_from(*every).ok()?;
                after.checked_add(span).ok()
            }
            Self::Cron(cron) => {
                let zoned = after.to_zoned(cron.tz.clone());
                let next = cron.cron.find_next_occurrence(&zoned, false).ok()?;
                Some(next.timestamp())
            }
        }
    }

    /// The `tz` of a cron schedule, as written; `None` for the local
    /// timezone and for `every`.
    pub fn tz_name(&self) -> Option<&str> {
        match self {
            Self::Cron(cron) => cron.tz_name.as_deref(),
            Self::Every(_) => None,
        }
    }

    /// The cron expression as written, for recipes; `None` for `every`.
    pub fn cron_expression(&self) -> Option<&str> {
        match self {
            Self::Cron(cron) => Some(&cron.expression),
            Self::Every(_) => None,
        }
    }
}

impl fmt::Display for Schedule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Every(every) => write!(f, "every {}", duration::format(*every)),
            Self::Cron(cron) => {
                write!(f, "cron {}", cron.expression)?;
                match &cron.tz_name {
                    Some(name) => write!(f, " ({name})"),
                    None => Ok(()),
                }
            }
        }
    }
}

/// A 5-field expression (no seconds, no year) or one of [`MACROS`].
fn parse_cron(expression: &str) -> Result<Cron, String> {
    if expression.starts_with('@') {
        if !MACROS.contains(&expression.to_ascii_lowercase().as_str()) {
            return Err(format!(
                "unsupported cron macro '{expression}' (use @hourly, @daily or @weekly)"
            ));
        }
    } else if expression.split_whitespace().count() != 5 {
        return Err(format!(
            "invalid cron expression '{expression}': expected 5 fields (minute hour day month weekday)"
        ));
    }
    CronParser::builder()
        .seconds(Seconds::Disallowed)
        .year(Year::Disallowed)
        .build()
        .parse(expression)
        .map_err(|e| format!("invalid cron expression '{expression}': {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    fn next(schedule: &Schedule, after: &str) -> String {
        schedule.next_after(at(after)).unwrap().to_string()
    }

    #[test]
    fn intervals_from_30s_to_24h() {
        assert_eq!(
            Schedule::every("5s").unwrap_err(),
            "every must be at least 30s"
        );
        assert_eq!(
            Schedule::every("25h").unwrap_err(),
            "every must be at most 24h"
        );
        for ok in ["30s", "5m", "24h"] {
            assert!(Schedule::every(ok).is_ok(), "{ok}");
        }
        let every = Schedule::every("5m").unwrap();
        assert_eq!(every.to_string(), "every 5m");
        assert_eq!(next(&every, "2026-01-01T00:00:00Z"), "2026-01-01T00:05:00Z");
        assert_eq!(every.cron_expression(), None);
    }

    #[test]
    fn five_field_expressions_and_three_macros() {
        let cron = Schedule::cron("*/15 * * * *", Some("UTC")).unwrap();
        assert_eq!(cron.to_string(), "cron */15 * * * * (UTC)");
        assert_eq!(cron.cron_expression(), Some("*/15 * * * *"));
        assert_eq!(next(&cron, "2026-01-01T00:07:00Z"), "2026-01-01T00:15:00Z");
        assert_eq!(next(&cron, "2026-01-01T00:15:00Z"), "2026-01-01T00:30:00Z");
        let daily = Schedule::cron("@daily", Some("UTC")).unwrap();
        assert_eq!(next(&daily, "2026-01-01T10:00:00Z"), "2026-01-02T00:00:00Z");
        let hourly = Schedule::cron("@hourly", Some("UTC")).unwrap();
        assert_eq!(
            next(&hourly, "2026-01-01T10:20:00Z"),
            "2026-01-01T11:00:00Z"
        );
        // 2026-01-01 is a Thursday: the next Sunday is the 4th.
        let weekly = Schedule::cron("@weekly", Some("UTC")).unwrap();
        assert_eq!(
            next(&weekly, "2026-01-01T10:00:00Z"),
            "2026-01-04T00:00:00Z"
        );
    }

    #[test]
    fn other_forms_are_refused() {
        let err = Schedule::cron("0 */5 * * * *", None).unwrap_err();
        assert!(err.contains("expected 5 fields"), "{err}");
        let err = Schedule::cron("@yearly", None).unwrap_err();
        assert!(err.contains("unsupported cron macro"), "{err}");
        let err = Schedule::cron("61 * * * *", None).unwrap_err();
        assert!(err.contains("invalid cron expression"), "{err}");
        let err = Schedule::cron("* * * * *", Some("Mars/Olympus")).unwrap_err();
        assert_eq!(err, "unknown timezone 'Mars/Olympus'");
    }

    #[test]
    fn a_timezone_moves_the_due_time() {
        let cron = Schedule::cron("0 9 * * *", Some("Europe/Lisbon")).unwrap();
        // Lisbon is UTC+1 in summer.
        assert_eq!(next(&cron, "2026-07-01T00:00:00Z"), "2026-07-01T08:00:00Z");
        assert_eq!(cron.to_string(), "cron 0 9 * * * (Europe/Lisbon)");
    }

    /// New York springs forward on 2026-03-08: 02:00 EST becomes 03:00 EDT.
    #[test]
    fn spring_forward_day() {
        let tz = Some("America/New_York");
        // A fixed time inside the gap runs right after it, the same day.
        let fixed = Schedule::cron("30 2 * * *", tz).unwrap();
        assert_eq!(next(&fixed, "2026-03-08T05:00:00Z"), "2026-03-08T07:00:00Z");
        // ...and at 02:30 EST again the day after.
        assert_eq!(next(&fixed, "2026-03-08T07:00:00Z"), "2026-03-09T06:30:00Z");
        // Hourly runs skip the missing hour: 01:00 EST, then 03:00 EDT,
        // one real hour apart.
        let hourly = Schedule::cron("0 * * * *", tz).unwrap();
        assert_eq!(
            next(&hourly, "2026-03-08T05:30:00Z"),
            "2026-03-08T06:00:00Z"
        );
        assert_eq!(
            next(&hourly, "2026-03-08T06:00:00Z"),
            "2026-03-08T07:00:00Z"
        );
    }

    /// New York falls back on 2026-11-01: 02:00 EDT becomes 01:00 EST.
    #[test]
    fn fall_back_day() {
        let tz = Some("America/New_York");
        // A fixed time in the repeated hour runs once, on its first pass.
        let fixed = Schedule::cron("30 1 * * *", tz).unwrap();
        assert_eq!(next(&fixed, "2026-11-01T04:00:00Z"), "2026-11-01T05:30:00Z");
        assert_eq!(next(&fixed, "2026-11-01T05:30:00Z"), "2026-11-02T06:30:00Z");
        // Hourly runs keep one real hour apart through both passes.
        let hourly = Schedule::cron("0 * * * *", tz).unwrap();
        assert_eq!(
            next(&hourly, "2026-11-01T04:30:00Z"),
            "2026-11-01T05:00:00Z"
        );
        assert_eq!(
            next(&hourly, "2026-11-01T05:00:00Z"),
            "2026-11-01T06:00:00Z"
        );
        assert_eq!(
            next(&hourly, "2026-11-01T06:00:00Z"),
            "2026-11-01T07:00:00Z"
        );
    }
}
