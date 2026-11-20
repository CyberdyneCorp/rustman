//! Line charts for the HTML bench report, as inline SVG geometry computed
//! here: the template only draws the polylines, so the page needs no
//! script or chart library and works offline.

use serde::Serialize;

/// The drawing area, in SVG user units.
const WIDTH: u32 = 640;
const HEIGHT: u32 = 180;

/// A chart over a per-second series.
#[derive(Debug, Serialize)]
pub struct Chart {
    pub width: u32,
    pub height: u32,
    pub lines: Vec<Line>,
    /// The value at the top of the chart, formatted.
    pub top: String,
    /// The last second, formatted (`42 s`).
    pub end: String,
}

/// One series of a chart.
#[derive(Debug, Serialize)]
pub struct Line {
    /// What it shows, such as `p95`; also its CSS class suffix.
    pub name: &'static str,
    /// `x,y x,y ...`.
    pub points: String,
}

/// A chart of `series` (each one value per second), scaled to the
/// largest value of any of them; `unit` follows the top value.
pub fn line_chart(series: &[(&'static str, Vec<f64>)], unit: &str) -> Chart {
    let len = series.iter().map(|(_, v)| v.len()).max().unwrap_or(0);
    let top = nice_ceiling(series.iter().flat_map(|(_, v)| v.iter().copied()));
    let lines = series
        .iter()
        .map(|(name, values)| Line {
            name,
            points: points(values, len, top),
        })
        .collect();
    Chart {
        width: WIDTH,
        height: HEIGHT,
        lines,
        top: format!("{} {unit}", trim(top)),
        end: format!("{len} s"),
    }
}

fn points(values: &[f64], len: usize, top: f64) -> String {
    let (width, height) = (f64::from(WIDTH), f64::from(HEIGHT));
    let step = if len > 1 {
        width / (len - 1) as f64
    } else {
        0.0
    };
    values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let x = i as f64 * step;
            let y = height - (v.max(0.0) / top) * height;
            format!("{x:.1},{y:.1}")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The smallest of 1, 2 or 5 times a power of ten at or above every
/// value (1 when there is none above 0).
fn nice_ceiling(values: impl Iterator<Item = f64>) -> f64 {
    let max = values.filter(|v| v.is_finite()).fold(0.0_f64, f64::max);
    if max <= 0.0 {
        return 1.0;
    }
    let magnitude = 10f64.powf(max.log10().floor());
    [1.0, 2.0, 5.0, 10.0]
        .iter()
        .map(|m| m * magnitude)
        .find(|v| *v >= max)
        .unwrap_or(10.0 * magnitude)
}

/// `200`, `0.5`: no trailing zeros.
fn trim(value: f64) -> String {
    let text = format!("{value:.3}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ceilings_are_round() {
        assert_eq!(nice_ceiling([0.0].into_iter()), 1.0);
        assert_eq!(nice_ceiling([3.2, 1.0].into_iter()), 5.0);
        assert_eq!(nice_ceiling([412.0].into_iter()), 500.0);
        assert_eq!(nice_ceiling([100.0].into_iter()), 100.0);
        assert_eq!(nice_ceiling([0.07].into_iter()), 0.1);
    }

    #[test]
    fn points_span_the_width_from_the_bottom() {
        let chart = line_chart(&[("rps", vec![0.0, 5.0, 10.0])], "req/s");
        assert_eq!(chart.lines[0].points, "0.0,180.0 320.0,90.0 640.0,0.0");
        assert_eq!(chart.top, "10 req/s");
        assert_eq!(chart.end, "3 s");
        let single = line_chart(&[("p95", vec![2.0])], "ms");
        assert_eq!(single.lines[0].points, "0.0,0.0");
        assert_eq!(single.top, "2 ms");
    }
}
