//! The Timing tab: the phase waterfall with the server's `Server-Timing`
//! metrics under the waiting phase, the trace ids and link, and the latency
//! sparkline (spec: Timing waterfall, Latency sparkline from history;
//! trace-propagation spec).

use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use rustman_core::run::TimingsRecord;
use rustman_core::trace::{ServerTiming, TraceRecord};

use crate::config::UiOptions;
use crate::theme::Token;

/// How many exchanges the sparkline shows.
pub const SPARK_POINTS: usize = 30;

/// One phase of the waterfall; `ms` is `None` when it did not happen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Phase {
    pub name: &'static str,
    pub start: f64,
    pub ms: Option<f64>,
}

/// The phases `http-requests` reports, in order, with their start offsets.
/// Waiting is the time to first byte after the connection was ready.
pub fn phases(t: &TimingsRecord) -> Vec<Phase> {
    let setup: f64 = [t.dns_ms, t.connect_ms, t.tls_ms].iter().flatten().sum();
    let mut out = Vec::new();
    let mut at = 0.0;
    for (name, ms) in [
        ("DNS", t.dns_ms),
        ("Connect", t.connect_ms),
        ("TLS", t.tls_ms),
    ] {
        out.push(Phase {
            name,
            start: at,
            ms,
        });
        at += ms.unwrap_or(0.0);
    }
    let waiting = (t.ttfb_ms - setup).max(0.0);
    out.push(Phase {
        name: "Waiting",
        start: at,
        ms: Some(waiting),
    });
    out.push(Phase {
        name: "Download",
        start: at + waiting,
        ms: Some(t.download_ms),
    });
    out
}

/// `84 ms`, `1.2 ms`, `3.0 s`.
pub fn ms_text(ms: f64) -> String {
    if ms >= 1000.0 {
        format!("{:.1} s", ms / 1000.0)
    } else if ms >= 10.0 {
        format!("{ms:.0} ms")
    } else {
        format!("{ms:.1} ms")
    }
}

/// `53.2 ms`, `12 ms`: a server metric as the server wrote it, to 0.1 ms.
pub fn server_ms_text(ms: f64) -> String {
    let rounded = (ms * 10.0).round() / 10.0;
    if rounded.fract() == 0.0 {
        format!("{rounded:.0} ms")
    } else {
        format!("{rounded:.1} ms")
    }
}

/// Places bars on a `bar_width`-column axis of `total` milliseconds.
struct Axis {
    total: f64,
    bar_width: usize,
}

impl Axis {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    fn column(&self, ms: f64) -> usize {
        ((ms / self.total) * self.bar_width as f64).round() as usize
    }

    /// Spaces up to the bar of `ms` starting at `start`, and the bar.
    fn bar(&self, start: f64, ms: f64, ui: &UiOptions) -> [Span<'static>; 2] {
        let start = self.column(start).min(self.bar_width - 1);
        let len = self.column(ms).max(1).min(self.bar_width - start);
        [
            Span::raw(" ".repeat(start)),
            Span::styled(bar(len, ui), ui.theme.fg(Token::Accent)),
        ]
    }
}

/// The waterfall rows: name, duration and a bar placed on a `width`-column
/// time axis, with each `server` metric as a sub-bar under the waiting
/// phase.
pub fn waterfall(
    t: &TimingsRecord,
    server: &[ServerTiming],
    width: usize,
    ui: &UiOptions,
) -> Vec<Line<'static>> {
    let axis = Axis {
        total: t.total_ms.max(f64::EPSILON),
        bar_width: width.saturating_sub(22).max(1),
    };
    let mut lines = vec![Line::styled(
        format!("Total {}", ms_text(t.total_ms)),
        ui.theme.fg(Token::Text).add_modifier(Modifier::BOLD),
    )];
    for phase in phases(t) {
        let label = Span::styled(format!("{:<9}", phase.name), ui.theme.fg(Token::Muted));
        let Some(ms) = phase.ms else {
            lines.push(Line::from(vec![label, Span::raw(format!("{:>10}", "—"))]));
            continue;
        };
        let mut spans = vec![
            label,
            Span::raw(format!("{:>10}", ms_text(ms))),
            Span::raw("   "),
        ];
        spans.extend(axis.bar(phase.start, ms, ui));
        lines.push(Line::from(spans));
        if phase.name == "Waiting" {
            lines.extend(server.iter().map(|m| server_row(m, phase.start, &axis, ui)));
        }
    }
    lines
}

/// One server metric under the waiting phase: `  db 53.2 ms` and a bar
/// from the start of the wait.
fn server_row(metric: &ServerTiming, start: f64, axis: &Axis, ui: &UiOptions) -> Line<'static> {
    let dur = metric.dur_ms.map_or_else(|| "—".to_owned(), server_ms_text);
    let label = format!("  {} {dur}", metric.name);
    let mut spans = vec![Span::styled(
        format!("{label:<19}   "),
        ui.theme.fg(Token::Muted),
    )];
    if let Some(ms) = metric.dur_ms {
        spans.extend(axis.bar(start, ms, ui));
    }
    if let Some(desc) = &metric.desc {
        spans.push(Span::styled(format!(" {desc}"), ui.theme.fg(Token::Muted)));
    }
    Line::from(spans)
}

/// The trace ids and link of an exchange; a link longer than `width`
/// columns wraps, so it can be read (and copied) whole.
pub fn trace_lines(trace: &TraceRecord, width: usize, ui: &UiOptions) -> Vec<Line<'static>> {
    let row = |name: &str, value: &str| {
        Line::from(vec![
            Span::styled(format!("{name:<9}"), ui.theme.fg(Token::Muted)),
            Span::styled(value.to_owned(), ui.theme.fg(Token::Text)),
        ])
    };
    let mut lines = vec![
        Line::styled(
            "Trace",
            ui.theme.fg(Token::Text).add_modifier(Modifier::BOLD),
        ),
        row("Trace id", &trace.trace_id),
        row("Span id", &trace.span_id),
    ];
    if let Some(backend) = &trace.backend_trace_id {
        lines.push(row("Backend", backend));
    }
    if let Some(link) = &trace.link {
        let chars: Vec<char> = link.chars().collect();
        let pieces = chars.chunks(width.saturating_sub(9).max(20));
        for (i, piece) in pieces.enumerate() {
            let name = if i == 0 { "Link" } else { "" };
            lines.push(row(name, &piece.iter().collect::<String>()));
        }
    }
    lines
}

fn bar(len: usize, ui: &UiOptions) -> String {
    let full = ui.glyphs.sparkline.last().copied().unwrap_or("#");
    full.repeat(len)
}

/// Min, median, p95 and max of `points`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stats {
    pub min: f64,
    pub median: f64,
    pub p95: f64,
    pub max: f64,
}

impl Stats {
    pub fn of(points: &[f64]) -> Option<Self> {
        if points.is_empty() {
            return None;
        }
        let mut sorted = points.to_vec();
        sorted.sort_by(f64::total_cmp);
        let at = |q: f64| {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_precision_loss
            )]
            let i = ((q * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len()) - 1;
            sorted[i]
        };
        Some(Self {
            min: sorted[0],
            median: at(0.5),
            p95: at(0.95),
            max: sorted[sorted.len() - 1],
        })
    }
}

/// The sparkline of the last [`SPARK_POINTS`] total times, newest
/// highlighted, and its statistics; `not enough history` below 2 points.
pub fn sparkline(points: &[f64], ui: &UiOptions) -> Vec<Line<'static>> {
    let title = Line::styled(
        "Latency (last exchanges of this request)",
        ui.theme.fg(Token::Text).add_modifier(Modifier::BOLD),
    );
    let points = &points[points.len().saturating_sub(SPARK_POINTS)..];
    let Some(stats) = Stats::of(points).filter(|_| points.len() >= 2) else {
        return vec![
            title,
            Line::styled("not enough history", ui.theme.fg(Token::Muted)),
        ];
    };
    let levels = ui.glyphs.sparkline;
    let range = (stats.max - stats.min).max(f64::EPSILON);
    let mut spans: Vec<Span<'static>> = points
        .iter()
        .map(|p| {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_precision_loss
            )]
            let level = (((p - stats.min) / range) * (levels.len() - 1) as f64).round() as usize;
            Span::styled(
                levels[level.min(levels.len() - 1)],
                ui.theme.fg(Token::Muted),
            )
        })
        .collect();
    if let Some(last) = spans.last_mut() {
        last.style = ui.theme.fg(Token::Accent).add_modifier(Modifier::BOLD);
    }
    // Two short lines, so all four stats fit the narrow response pane.
    let dot = ui.glyphs.dot;
    let low = format!(
        "min {} {dot} median {}",
        ms_text(stats.min),
        ms_text(stats.median)
    );
    let high = format!(
        "p95 {} {dot} max {} {dot} {} points",
        ms_text(stats.p95),
        ms_text(stats.max),
        points.len()
    );
    vec![
        title,
        Line::from(spans),
        Line::styled(low, ui.theme.fg(Token::Muted)),
        Line::styled(high, ui.theme.fg(Token::Muted)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timings(dns: Option<f64>, connect: Option<f64>, tls: Option<f64>) -> TimingsRecord {
        TimingsRecord {
            auth_ms: None,
            dns_ms: dns,
            connect_ms: connect,
            tls_ms: tls,
            ttfb_ms: 60.0,
            download_ms: 20.0,
            total_ms: 80.0,
        }
    }

    #[test]
    fn phases_start_where_the_previous_ended() {
        let p = phases(&timings(Some(5.0), Some(10.0), Some(15.0)));
        let starts: Vec<f64> = p.iter().map(|p| p.start).collect();
        assert_eq!(starts, [0.0, 5.0, 15.0, 30.0, 60.0]);
        assert_eq!(p[3].ms, Some(30.0));
    }

    #[test]
    fn stats_and_ms_text() {
        let s = Stats::of(&[30.0, 10.0, 20.0, 40.0]).unwrap();
        assert_eq!((s.min, s.median, s.p95, s.max), (10.0, 20.0, 40.0, 40.0));
        assert!(Stats::of(&[]).is_none());
        assert_eq!(ms_text(3000.0), "3.0 s");
        assert_eq!(ms_text(84.2), "84 ms");
        assert_eq!(ms_text(1.25), "1.2 ms");
    }
}
