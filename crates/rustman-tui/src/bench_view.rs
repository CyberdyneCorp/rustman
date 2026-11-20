//! The live load-test dashboard (add-load-testing, "Live TUI dashboard"):
//! elapsed and remaining time, active VUs, RPS, p50/p95/p99, error rate,
//! per-second sparklines of p95 and RPS, threshold status and a per-file
//! table, drawn from a [`Snapshot`] of the run so far. Drawing is pure:
//! [`crate::bench_live`] decides when (at most 4 times a second) and where.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Row, Sparkline, Table};
use rustman_bench::live::clock;
use rustman_bench::{BenchSummary, Snapshot, interrupt_notice};

const LABEL: Style = Style::new().fg(Color::DarkGray);
const VALUE: Style = Style::new().add_modifier(Modifier::BOLD);
const PASS: Style = Style::new().fg(Color::Green);
const FAIL: Style = Style::new().fg(Color::Red);

/// Draw the dashboard of `snapshot` in `area`, titled `title`.
pub fn render(frame: &mut Frame, area: Rect, snapshot: &Snapshot, title: &str) {
    let title = if snapshot.interrupted {
        format!(" {title} | {} ", interrupt_notice())
    } else {
        format!(" {title} ")
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let threshold_rows = u16::try_from(snapshot.thresholds.len().min(6)).unwrap_or(6);
    let [stats, spark_p95, spark_rps, thresholds, files] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Length(threshold_rows + u16::from(threshold_rows > 0)),
        Constraint::Min(3),
    ])
    .areas(inner);
    frame.render_widget(Paragraph::new(stat_lines(snapshot)), stats);
    let p95: Vec<u64> = snapshot
        .recent
        .iter()
        .map(|s| (s.p95_ms * 1000.0) as u64)
        .collect();
    let rps: Vec<u64> = snapshot
        .recent
        .iter()
        .map(|s| s.rps.round() as u64)
        .collect();
    let p95_now = snapshot.recent.last().map_or(0.0, |s| s.p95_ms);
    spark(
        frame,
        spark_p95,
        &format!("p95 {}", ms(p95_now)),
        &p95,
        Color::Yellow,
    );
    spark(
        frame,
        spark_rps,
        &format!("RPS {:.1}", snapshot.rps),
        &rps,
        Color::Cyan,
    );
    frame.render_widget(Paragraph::new(threshold_lines(snapshot)), thresholds);
    frame.render_widget(file_table(&snapshot.summary), files);
}

/// Time, VUs, rate and errors, then the latency percentiles.
fn stat_lines(snapshot: &Snapshot) -> Vec<Line<'static>> {
    let time = match snapshot.remaining {
        Some(left) => format!("{} ({} left)", clock(snapshot.elapsed), clock(left)),
        None => clock(snapshot.elapsed),
    };
    let overall = &snapshot.summary.overall;
    let latency = &overall.latency;
    let errors = format!("{:.2}%", snapshot.error_rate() * 100.0);
    let error_style = if snapshot.error_rate() > 0.0 {
        FAIL
    } else {
        VALUE
    };
    let kind = snapshot.summary.latency_kind.as_str();
    vec![
        Line::from(
            [
                stat("Time", time, VALUE),
                stat("VUs", snapshot.active_vus.to_string(), VALUE),
                stat("RPS", format!("{:.1}", snapshot.rps), VALUE),
                stat("Requests", overall.requests.to_string(), VALUE),
                stat("Errors", errors, error_style),
            ]
            .concat(),
        ),
        Line::from(
            [
                stat(&format!("Latency ({kind})  p50"), ms(latency.p50_ms), VALUE),
                stat("p95", ms(latency.p95_ms), VALUE),
                stat("p99", ms(latency.p99_ms), VALUE),
            ]
            .concat(),
        ),
    ]
}

fn stat(label: &str, value: String, style: Style) -> Vec<Span<'static>> {
    vec![
        Span::styled(format!("{label} "), LABEL),
        Span::styled(value, style),
        Span::raw("   "),
    ]
}

fn ms(value: f64) -> String {
    format!("{value:.2}ms")
}

fn spark(frame: &mut Frame, area: Rect, title: &str, data: &[u64], color: Color) {
    let widget = Sparkline::default()
        .block(Block::default().title(Span::styled(title.to_owned(), LABEL)))
        .data(data)
        .style(Style::new().fg(color));
    frame.render_widget(widget, area);
}

fn threshold_lines(snapshot: &Snapshot) -> Vec<Line<'static>> {
    if snapshot.thresholds.is_empty() {
        return Vec::new();
    }
    let breached = snapshot.breached();
    let header = if breached == 0 {
        Span::styled("Thresholds: all passing", PASS)
    } else {
        Span::styled(format!("Thresholds: {breached} breached"), FAIL)
    };
    let rows = snapshot.thresholds.iter().take(5).map(|t| {
        let style = if t.passed { PASS } else { FAIL };
        Line::from(Span::styled(format!("  {}", t.line()), style))
    });
    std::iter::once(Line::from(header)).chain(rows).collect()
}

/// One row per request file, then the overall row.
fn file_table(summary: &BenchSummary) -> Table<'static> {
    let header = Row::new(["File", "Requests", "RPS", "p50", "p95", "p99", "Errors"]).style(LABEL);
    let files = summary
        .files
        .iter()
        .map(|f| (f.file.clone(), &f.stats))
        .chain(std::iter::once(("overall".to_owned(), &summary.overall)));
    let rows: Vec<Row> = files
        .map(|(name, stats)| {
            Row::new([
                name,
                stats.requests.to_string(),
                format!("{:.1}", summary.rps_of(stats)),
                ms(stats.latency.p50_ms),
                ms(stats.latency.p95_ms),
                ms(stats.latency.p99_ms),
                format!("{:.2}%", stats.error_rate * 100.0),
            ])
        })
        .collect();
    Table::new(
        rows,
        [
            Constraint::Fill(1),
            Constraint::Length(9),
            Constraint::Length(8),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(8),
        ],
    )
    .header(header)
    .block(Block::default().borders(Borders::TOP).title(" Per file "))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use rustman_bench::metrics::{ErrorClass, LatencyKind, Metrics, Sample};
    use rustman_bench::{Live, Snapshot};

    use super::*;
    use crate::snapshot::buffer_text;

    /// A run with two files, three closed seconds and two thresholds, one
    /// of them breached, with its clock pinned.
    fn snapshot() -> Snapshot {
        let metrics = Arc::new(Metrics::new(
            LatencyKind::Service,
            vec![
                "api/1-login.rman.yaml".into(),
                "api/2-list.rman.yaml".into(),
            ],
        ));
        let thresholds = ["p95<300ms", "error_rate<1%"]
            .iter()
            .map(|t| t.parse().unwrap())
            .collect();
        let live = Live::new(metrics.clone(), thresholds, Some(Duration::from_secs(30)));
        for (second, ms) in [(0, 10), (1, 40), (2, 20)] {
            for i in 0..10 {
                metrics.record(&Sample {
                    file: i % 2,
                    latency: Duration::from_millis(ms + i as u64),
                    error: (second == 1 && i == 3).then_some(ErrorClass::Status5xx),
                    ..Sample::default()
                });
            }
            metrics.tick();
        }
        let mut snap = live.snapshot();
        snap.elapsed = Duration::from_secs(12);
        snap.remaining = Some(Duration::from_secs(18));
        snap.active_vus = 5;
        snap.rps = 10.0;
        snap.summary.elapsed_ms = 3000.0;
        for (i, second) in snap.recent.iter_mut().enumerate() {
            second.rps = 10.0 + i as f64;
        }
        snap
    }

    fn draw(snap: &Snapshot, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), snap, "rustman bench http://127.0.0.1"))
            .unwrap();
        buffer_text(terminal.backend().buffer())
    }

    #[test]
    fn the_dashboard_shows_every_live_figure() {
        let text = draw(&snapshot(), 100, 24);
        insta::assert_snapshot!(text);
        for needle in [
            "Time 12s (18s left)",
            "VUs 5",
            "RPS 10.0",
            "Requests 30",
            "Errors 3.33%",
            "p95",
            "Thresholds: 1 breached",
            "✗ error_rate<1%",
            "✓ p95<300ms",
            "api/1-login.rman.yaml",
            "overall",
        ] {
            assert!(text.contains(needle), "{needle}:\n{text}");
        }
    }

    #[test]
    fn a_bench_without_thresholds_or_a_fixed_duration() {
        let mut snap = snapshot();
        snap.thresholds.clear();
        snap.remaining = None;
        let text = draw(&snap, 80, 20);
        insta::assert_snapshot!(text);
        assert!(!text.contains("Thresholds"), "{text}");
        assert!(text.contains("Time 12s "), "{text}");
    }

    #[test]
    fn an_interrupted_bench_says_so_in_its_title() {
        let mut snap = snapshot();
        assert!(!snap.interrupted);
        assert!(!draw(&snap, 200, 20).contains("interrupted"));
        snap.interrupted = true;
        let text = draw(&snap, 200, 20);
        assert!(
            text.contains("| interrupted: no new iterations; waiting up to 10s"),
            "{text}"
        );
    }
}
