//! `rustman history list` filters (request-history spec "List and search").

use std::str::FromStr;
use std::time::Duration;

use super::entry::Entry;
use super::ulid::{now_millis, timestamp_millis};

/// `--status`: one code (`404`) or a class (`5xx`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusFilter {
    Code(u16),
    Class(u16),
}

impl StatusFilter {
    pub fn matches(self, status: u16) -> bool {
        match self {
            Self::Code(code) => status == code,
            Self::Class(class) => status / 100 == class,
        }
    }
}

impl FromStr for StatusFilter {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let invalid =
            || format!("invalid status '{text}' (use a code such as 404 or a class such as 5xx)");
        let lower = text.trim().to_ascii_lowercase();
        if let Some(class) = lower.strip_suffix("xx") {
            return match class.as_bytes() {
                [digit @ b'1'..=b'5'] => Ok(Self::Class(u16::from(digit - b'0'))),
                _ => Err(invalid()),
            };
        }
        match lower.parse::<u16>() {
            Ok(code @ 100..=599) => Ok(Self::Code(code)),
            _ => Err(invalid()),
        }
    }
}

/// `--since`: `30s`, `15m`, `1h`, `2d` or `1w`.
pub fn parse_age(text: &str) -> Result<Duration, String> {
    let text = text.trim();
    let invalid = || format!("invalid duration '{text}' (use 30s, 15m, 1h, 2d or 1w)");
    let at = text
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(invalid)?;
    let (number, unit) = text.split_at(at);
    let value: u64 = number.parse().map_err(|_| invalid())?;
    let seconds = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        "w" => 604_800,
        _ => return Err(invalid()),
    };
    Ok(Duration::from_secs(value.saturating_mul(seconds)))
}

/// Which entries to list, newest first.
#[derive(Clone, Debug, Default)]
pub struct Query {
    pub status: Option<StatusFilter>,
    /// Case-insensitive.
    pub method: Option<String>,
    /// Case-insensitive text in the URL, the request body or the response body.
    pub grep: Option<String>,
    /// Only entries at most this old.
    pub since: Option<Duration>,
    pub limit: Option<usize>,
}

impl Query {
    /// The entries of `entries` (newest first) that match, up to the limit.
    pub fn apply(&self, entries: Vec<Entry>) -> Vec<Entry> {
        let cutoff = self.since.map(|age| {
            let age = u64::try_from(age.as_millis()).unwrap_or(u64::MAX);
            now_millis().saturating_sub(age)
        });
        let grep = self.grep.as_deref().map(str::to_lowercase);
        entries
            .into_iter()
            .filter(|e| cutoff.is_none_or(|cutoff| timestamp_millis(&e.id).unwrap_or(0) >= cutoff))
            .filter(|e| self.status_matches(e))
            .filter(|e| self.method_matches(e))
            .filter(|e| grep.as_deref().is_none_or(|text| grep_matches(e, text)))
            .take(self.limit.unwrap_or(usize::MAX))
            .collect()
    }

    fn status_matches(&self, entry: &Entry) -> bool {
        match self.status {
            None => true,
            Some(filter) => entry.status.is_some_and(|s| filter.matches(s)),
        }
    }

    fn method_matches(&self, entry: &Entry) -> bool {
        self.method
            .as_deref()
            .is_none_or(|m| entry.sent.method.eq_ignore_ascii_case(m))
    }
}

fn grep_matches(entry: &Entry, text: &str) -> bool {
    [
        Some(entry.sent.url.as_str()),
        entry.sent.body.as_deref(),
        entry.response_body(),
    ]
    .into_iter()
    .flatten()
    .any(|haystack| haystack.to_lowercase().contains(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_are_codes_or_classes() {
        assert_eq!("404".parse(), Ok(StatusFilter::Code(404)));
        assert_eq!("5xx".parse(), Ok(StatusFilter::Class(5)));
        assert_eq!("2XX".parse(), Ok(StatusFilter::Class(2)));
        for bad in ["6xx", "xx", "42", "abc", "15xx", "0xx"] {
            assert!(bad.parse::<StatusFilter>().is_err(), "{bad}");
        }
        assert!(StatusFilter::Class(5).matches(503));
        assert!(!StatusFilter::Class(5).matches(200));
        assert_eq!(
            "6xx".parse::<StatusFilter>().unwrap_err(),
            "invalid status '6xx' (use a code such as 404 or a class such as 5xx)"
        );
    }

    #[test]
    fn ages_take_a_unit() {
        assert_eq!(parse_age("30s"), Ok(Duration::from_secs(30)));
        assert_eq!(parse_age("15m"), Ok(Duration::from_secs(900)));
        assert_eq!(parse_age("1h"), Ok(Duration::from_secs(3600)));
        assert_eq!(parse_age("2d"), Ok(Duration::from_secs(172_800)));
        assert_eq!(parse_age("1w"), Ok(Duration::from_secs(604_800)));
        for bad in ["", "1", "h", "1y", "-1h", "1.5h"] {
            assert!(parse_age(bad).is_err(), "{bad}");
        }
    }
}
