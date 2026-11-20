//! Durations in monitor files and `[monitors]`: `500ms`, `30s`, `5m`, `1h`,
//! `30d`. Unlike request timeouts, a bare number is refused: `every: 5`
//! would be ambiguous.

use std::time::Duration;

const UNITS: &[(&str, u64)] = &[
    ("ms", 1),
    ("s", 1_000),
    ("m", 60_000),
    ("h", 3_600_000),
    ("d", 86_400_000),
];

/// Parse `text` as a positive number followed by `ms`, `s`, `m`, `h` or `d`.
pub fn parse(text: &str) -> Result<Duration, String> {
    let text = text.trim();
    let invalid = || format!("invalid duration '{text}' (use 30s, 5m, 1h or 30d)");
    let at = text
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(invalid)?;
    let (number, unit) = text.split_at(at);
    let value: u64 = number.parse().map_err(|_| invalid())?;
    let millis = UNITS
        .iter()
        .find(|(name, _)| *name == unit)
        .map(|(_, factor)| value.saturating_mul(*factor))
        .ok_or_else(invalid)?;
    if millis == 0 {
        return Err(invalid());
    }
    Ok(Duration::from_millis(millis))
}

/// `duration` in the largest unit that divides it: `5m`, `90s`, `1500ms`.
pub fn format(duration: Duration) -> String {
    let millis = duration.as_millis();
    for (name, factor) in UNITS.iter().rev() {
        let factor = u128::from(*factor);
        if millis >= factor && millis.is_multiple_of(factor) {
            return format!("{}{name}", millis / factor);
        }
    }
    format!("{millis}ms")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_parse() {
        assert_eq!(parse("500ms"), Ok(Duration::from_millis(500)));
        assert_eq!(parse("30s"), Ok(Duration::from_secs(30)));
        assert_eq!(parse(" 5m "), Ok(Duration::from_secs(300)));
        assert_eq!(parse("24h"), Ok(Duration::from_secs(86_400)));
        assert_eq!(parse("30d"), Ok(Duration::from_secs(30 * 86_400)));
    }

    #[test]
    fn bad_durations_are_refused() {
        for text in ["", "5", "0s", "5x", "m", "-5s", "1.5h"] {
            let err = parse(text).unwrap_err();
            assert!(err.contains("invalid duration"), "{text}: {err}");
        }
    }

    #[test]
    fn durations_format_in_their_largest_unit() {
        assert_eq!(format(Duration::from_secs(300)), "5m");
        assert_eq!(format(Duration::from_secs(90)), "90s");
        assert_eq!(format(Duration::from_secs(3600)), "1h");
        assert_eq!(format(Duration::from_millis(1500)), "1500ms");
        assert_eq!(format(Duration::from_secs(86_400 * 30)), "30d");
    }
}
