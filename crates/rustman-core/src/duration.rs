//! Durations written by users: `500ms`, `5s`, `2m`, or a bare number of
//! milliseconds. Used by CLI flags and request-file fields.

use std::time::Duration;

/// `500ms`, `5s`, `2m`, or a bare number of milliseconds. Zero is rejected.
pub fn parse_duration(text: &str) -> Result<Duration, String> {
    let text = text.trim();
    let (number, unit) = match text.find(|c: char| !c.is_ascii_digit()) {
        Some(at) => text.split_at(at),
        None => (text, "ms"),
    };
    let invalid = || format!("invalid duration '{text}' (use 500ms, 5s, 2m or milliseconds)");
    let value: u64 = number.parse().map_err(|_| invalid())?;
    let duration = match unit {
        "ms" => Duration::from_millis(value),
        "s" => Duration::from_secs(value),
        "m" => Duration::from_secs(value.saturating_mul(60)),
        _ => return Err(invalid()),
    };
    if duration.is_zero() {
        return Err(invalid());
    }
    Ok(duration)
}
