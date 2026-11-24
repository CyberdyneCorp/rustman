//! `search_history` (spec "History tool"): recorded exchanges filtered by
//! URL text, method, status range and time range, newest first, as redacted
//! summaries.

use rustman_core::history::{Entry, HistoryStore, parse_age};
use rustman_core::history::{StatusFilter, timestamp_millis};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Call, output};
use crate::server::context::ToolOutput;
use crate::server::error::{ErrorCode, ToolError};
use crate::server::limits::{paginate, snapshot};

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchHistoryArgs {
    /// Only URLs containing this text (any case).
    pub url: Option<String>,
    /// Only this method.
    pub method: Option<String>,
    /// A status range (`500-599`), class (`5xx`) or code (`404`).
    pub status: Option<String>,
    /// Only exchanges at or after this time: an age (`30s`, `15m`, `1h`,
    /// `2d`, `1w`) or an RFC 3339 timestamp.
    pub since: Option<String>,
    /// Only exchanges at or before this RFC 3339 timestamp.
    pub until: Option<String>,
    /// The `next_cursor` of the previous page.
    pub cursor: Option<String>,
    /// Items per page (default 50).
    pub limit: Option<usize>,
}

/// One recorded exchange, redacted.
#[derive(Debug, Serialize, JsonSchema)]
pub struct HistoryItem {
    pub id: String,
    pub timestamp: String,
    /// The request's id, or `adhoc`.
    pub request: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    pub method: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct HistoryPage {
    pub items: Vec<HistoryItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// How many exchanges match, over every page.
    pub total: usize,
}

/// An inclusive range of statuses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatusRange(pub u16, pub u16);

impl StatusRange {
    /// `500-599`, `5xx` or `404`.
    pub fn parse(text: &str) -> Result<Self, ToolError> {
        let invalid = || {
            ToolError::new(
                ErrorCode::InvalidInput,
                format!("invalid status '{text}' (use 500-599, 5xx or 404)"),
            )
        };
        if let Some((low, high)) = text.split_once('-') {
            let low: u16 = low.trim().parse().map_err(|_| invalid())?;
            let high: u16 = high.trim().parse().map_err(|_| invalid())?;
            return if low <= high {
                Ok(Self(low, high))
            } else {
                Err(invalid())
            };
        }
        match text.parse::<StatusFilter>().map_err(|_| invalid())? {
            StatusFilter::Code(code) => Ok(Self(code, code)),
            StatusFilter::Class(class) => Ok(Self(class * 100, class * 100 + 99)),
        }
    }

    fn contains(self, status: Option<u16>) -> bool {
        status.is_some_and(|s| (self.0..=self.1).contains(&s))
    }
}

/// The filters of a search, parsed.
struct Filters {
    url: Option<String>,
    method: Option<String>,
    status: Option<StatusRange>,
    /// Milliseconds since the epoch.
    since: Option<u64>,
    /// Milliseconds since the epoch, to the second.
    until: Option<u64>,
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

pub(super) fn since_millis(text: &str) -> Result<u64, ToolError> {
    if let Ok(age) = parse_age(text) {
        let age = u64::try_from(age.as_millis()).unwrap_or(u64::MAX);
        return Ok(now_millis().saturating_sub(age));
    }
    rfc3339_millis(text).ok_or_else(|| {
        ToolError::new(
            ErrorCode::InvalidInput,
            format!("invalid since '{text}' (use 30s, 15m, 1h, 2d, 1w or an RFC 3339 time)"),
        )
    })
}

fn until_millis(text: &str) -> Result<u64, ToolError> {
    rfc3339_millis(text).ok_or_else(|| {
        ToolError::new(
            ErrorCode::InvalidInput,
            format!("invalid until '{text}' (use an RFC 3339 time)"),
        )
    })
}

/// Milliseconds since the epoch of an RFC 3339 time, to the second; a
/// `+HH:MM`/`-HH:MM` offset is applied, `Z` or no zone is UTC.
fn rfc3339_millis(text: &str) -> Option<u64> {
    let digits = |range: std::ops::Range<usize>| text.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (digits(0..4)?, digits(5..7)?, digits(8..10)?);
    let (hour, minute, second) = (digits(11..13)?, digits(14..16)?, digits(17..19)?);
    let offset = offset_seconds(text.get(19..)?)?;
    let shifted = if month <= 2 { year - 1 } else { year };
    let era = shifted.div_euclid(400);
    let year_of_era = shifted - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second - offset;
    u64::try_from(seconds).ok().map(|s| s * 1000)
}

/// The zone offset in seconds of what follows the seconds of an RFC 3339
/// time: optional `.fraction`, then `Z`, `+HH:MM`, `-HH:MM` or nothing.
fn offset_seconds(rest: &str) -> Option<i64> {
    let zone = match rest.strip_prefix('.') {
        Some(fraction) => fraction.trim_start_matches(|c: char| c.is_ascii_digit()),
        None => rest,
    };
    if zone.is_empty() || zone.eq_ignore_ascii_case("z") {
        return Some(0);
    }
    let sign = match zone.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let (hours, minutes) = zone.get(1..)?.split_once(':')?;
    if hours.len() != 2 || minutes.len() != 2 {
        return None;
    }
    let (hours, minutes): (i64, i64) = (hours.parse().ok()?, minutes.parse().ok()?);
    Some(sign * (hours * 3600 + minutes * 60))
}

impl Filters {
    fn of(args: &SearchHistoryArgs) -> Result<Self, ToolError> {
        Ok(Self {
            url: args.url.as_deref().map(str::to_lowercase),
            method: args.method.clone(),
            status: args.status.as_deref().map(StatusRange::parse).transpose()?,
            since: args.since.as_deref().map(since_millis).transpose()?,
            until: args.until.as_deref().map(until_millis).transpose()?,
        })
    }

    fn keeps(&self, entry: &Entry) -> bool {
        let url = self
            .url
            .as_deref()
            .is_none_or(|text| entry.sent.url.to_lowercase().contains(text));
        let method = self
            .method
            .as_deref()
            .is_none_or(|m| entry.sent.method.eq_ignore_ascii_case(m));
        let status = self.status.is_none_or(|range| range.contains(entry.status));
        let since = self
            .since
            .is_none_or(|cutoff| timestamp_millis(&entry.id).unwrap_or(0) >= cutoff);
        let until = self
            .until
            .is_none_or(|until| rfc3339_millis(&entry.timestamp).unwrap_or(0) <= until);
        url && method && status && since && until
    }
}

fn item(entry: Entry) -> HistoryItem {
    HistoryItem {
        id: entry.id,
        timestamp: entry.timestamp,
        request: entry.request,
        environment: entry.environment,
        method: entry.sent.method,
        url: entry.sent.url,
        status: entry.status,
        total_ms: entry.total_ms,
        error: entry.error,
    }
}

pub fn search_history(call: &Call) -> Result<ToolOutput, ToolError> {
    let args: SearchHistoryArgs = call.args()?;
    let filters = Filters::of(&args)?;
    let ws = call.ctx.workspace();
    let store = HistoryStore::for_workspace(&ws.root, &ws.settings.history);
    let listing = store
        .list()
        .map_err(|e| ToolError::internal(e.to_string()))?;
    let found: Vec<HistoryItem> = listing
        .entries
        .into_iter()
        .filter(|e| filters.keeps(e))
        .map(item)
        .collect();
    let total = found.len();
    let keys: Vec<String> = found.iter().map(|i| i.id.clone()).collect();
    let page = paginate(found, snapshot(&keys), args.cursor.as_deref(), args.limit)?;
    let shown = page.items.len();
    let result = HistoryPage {
        items: page.items,
        next_cursor: page.next_cursor,
        total,
    };
    output(
        &result,
        format!("{shown} of {total} exchanges, newest first"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_are_ranges_classes_or_codes() {
        assert_eq!(
            StatusRange::parse("500-599").unwrap(),
            StatusRange(500, 599)
        );
        assert_eq!(StatusRange::parse("5xx").unwrap(), StatusRange(500, 599));
        assert_eq!(StatusRange::parse("404").unwrap(), StatusRange(404, 404));
        for bad in ["599-500", "abc", "5-x", "7xx"] {
            assert_eq!(
                StatusRange::parse(bad).unwrap_err().code,
                ErrorCode::InvalidInput
            );
        }
        assert!(StatusRange(500, 599).contains(Some(503)));
        assert!(!StatusRange(500, 599).contains(None));
    }

    #[test]
    fn rfc3339_times_are_read_to_the_second() {
        assert_eq!(rfc3339_millis("1970-01-01T00:00:01Z"), Some(1000));
        assert_eq!(
            rfc3339_millis("2026-11-18T10:04:12.311Z"),
            Some(1_794_996_252_000)
        );
        assert_eq!(rfc3339_millis("yesterday"), None);
        assert!(since_millis("1h").unwrap() < now_millis());
        assert!(since_millis("soon").is_err());
    }

    #[test]
    fn offsets_are_applied_and_bad_until_values_are_refused() {
        let utc = rfc3339_millis("2026-11-18T10:04:12Z");
        assert_eq!(rfc3339_millis("2026-11-18T12:04:12+02:00"), utc);
        assert_eq!(rfc3339_millis("2026-11-18T07:34:12.5-02:30"), utc);
        assert_eq!(rfc3339_millis("2026-11-18T10:04:12"), utc);
        assert_eq!(rfc3339_millis("2026-11-18T10:04:12+2"), None);
        assert_eq!(rfc3339_millis("2026-11-18T10:04:12 junk"), None);
        for bad in ["yesterday", "2026-11-18"] {
            let args = SearchHistoryArgs {
                until: Some(bad.into()),
                ..SearchHistoryArgs::default()
            };
            let err = Filters::of(&args).err().expect(bad);
            assert_eq!(err.code, ErrorCode::InvalidInput, "{bad}");
        }
        let args = SearchHistoryArgs {
            until: Some("2026-11-18T12:04:12+02:00".into()),
            ..SearchHistoryArgs::default()
        };
        assert_eq!(Filters::of(&args).unwrap().until, utc);
    }
}
