//! Small statistics shared by the views.

/// The `p`-th percentile (0..=100) of `values` by nearest rank; `None`
/// when empty.
pub fn percentile(values: &[f64], p: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    Some(sorted[rank.clamp(1, sorted.len()) - 1])
}

/// `values` as a one-line text sparkline of block characters, for table
/// cells (the last `width` values).
pub fn text_sparkline(values: &[f64], width: usize) -> String {
    const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let start = values.len().saturating_sub(width);
    let shown = &values[start..];
    let (min, max) = shown
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
            (lo.min(*v), hi.max(*v))
        });
    shown
        .iter()
        .map(|v| {
            if max <= min {
                BLOCKS[0]
            } else {
                let level = ((v - min) / (max - min) * 7.0).round() as usize;
                BLOCKS[level.min(7)]
            }
        })
        .collect()
}

/// Milliseconds as shown in tiles and tables: `84 ms`, `1.2 s`.
pub fn format_ms(ms: f64) -> String {
    if ms >= 1000.0 {
        format!("{:.1} s", ms / 1000.0)
    } else {
        format!("{ms:.0} ms")
    }
}

/// A percentage with no decimals unless it is below 10: `75%`, `2.5%`.
pub fn format_percent(percent: f64) -> String {
    if percent > 0.0 && percent < 10.0 && percent.fract() != 0.0 {
        format!("{percent:.1}%")
    } else {
        format!("{percent:.0}%")
    }
}

/// `part` of `whole` in percent (0 when `whole` is 0).
pub fn percent(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 * 100.0 / whole as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_by_nearest_rank() {
        let values: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(percentile(&values, 50.0), Some(50.0));
        assert_eq!(percentile(&values, 95.0), Some(95.0));
        assert_eq!(percentile(&[7.0], 95.0), Some(7.0));
        assert_eq!(percentile(&[], 50.0), None);
    }

    #[test]
    fn sparklines_and_formats() {
        assert_eq!(text_sparkline(&[1.0, 2.0, 3.0], 10), "▁▅█");
        assert_eq!(text_sparkline(&[5.0, 5.0], 10), "▁▁");
        assert_eq!(text_sparkline(&[1.0, 2.0, 3.0], 2), "▁█");
        assert_eq!(format_ms(84.4), "84 ms");
        assert_eq!(format_ms(1234.0), "1.2 s");
        assert_eq!(format_percent(75.0), "75%");
        assert_eq!(format_percent(2.5), "2.5%");
        assert_eq!(percent(18, 24), 75.0);
        assert_eq!(percent(1, 0), 0.0);
    }
}
