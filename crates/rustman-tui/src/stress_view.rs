//! The live stress dashboard (stress-testing "Live stress dashboard"):
//! mode, phase and step; offered and achieved rate; p50/p95/p99 of valid
//! and fuzzed traffic apart; error classes and the SLO status of the last
//! windows; the kill-switch state and findings as they arrive. Drawing is
//! pure: [`crate::stress_live`] decides when (at most 4 times a second).

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Row, Table};
use rustman_bench::live::clock;
use rustman_bench::metrics::StatsSummary;
use rustman_bench::stress::format_rate;
use rustman_bench::stress::kill::{DOWN_SHARE, HEALTH_FAILURES};
use rustman_bench::stress::live::StressSnapshot;

const LABEL: Style = Style::new().fg(Color::DarkGray);
const VALUE: Style = Style::new().add_modifier(Modifier::BOLD);
const PASS: Style = Style::new().fg(Color::Green);
const FAIL: Style = Style::new().fg(Color::Red);

/// Draw the dashboard of `snapshot` in `area`, titled `title`.
pub fn render(frame: &mut Frame, area: Rect, snapshot: &StressSnapshot, title: &str) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let windows = u16::try_from(snapshot.windows.len()).unwrap_or(12);
    let [stats, latency, windows_area, findings] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(4),
        Constraint::Length(windows + 2),
        Constraint::Min(2),
    ])
    .areas(inner);
    frame.render_widget(Paragraph::new(stat_lines(snapshot)), stats);
    frame.render_widget(latency_table(snapshot), latency);
    frame.render_widget(window_table(snapshot), windows_area);
    frame.render_widget(Paragraph::new(finding_lines(snapshot)), findings);
}

fn stat(label: &str, value: String, style: Style) -> Vec<Span<'static>> {
    vec![
        Span::styled(format!("{label} "), LABEL),
        Span::styled(value, style),
        Span::raw("   "),
    ]
}

/// Mode, phase, step and time; rates and errors; the kill switch.
fn stat_lines(s: &StressSnapshot) -> Vec<Line<'static>> {
    let mut first = stat("mode", s.mode.to_string(), VALUE);
    first.extend(stat("phase", s.phase.to_owned(), VALUE));
    first.extend(stat("step", s.step.to_string(), VALUE));
    first.extend(stat("elapsed", clock(s.elapsed), VALUE));
    let mut second = stat(
        "offered",
        format!("{} req/s", format_rate(s.offered)),
        VALUE,
    );
    second.extend(stat(
        "achieved",
        format!("{} req/s", format_rate(s.achieved)),
        VALUE,
    ));
    let errors: Vec<String> = s.errors.iter().map(|(c, n)| format!("{c} {n}")).collect();
    let errors = if errors.is_empty() {
        "none".to_owned()
    } else {
        errors.join(", ")
    };
    let error_style = if s.errors.is_empty() { PASS } else { FAIL };
    second.extend(stat("errors", errors, error_style));
    vec![
        Line::from(first),
        Line::from(second),
        Line::from(kill_spans(s)),
    ]
}

fn kill_spans(s: &StressSnapshot) -> Vec<Span<'static>> {
    if let Some(killed) = &s.killed {
        return stat("kill switch", killed.clone(), FAIL);
    }
    let k = &s.kill;
    let text = format!(
        "armed: connection {:.0}%, timeouts {:.0}% (trips above {:.0}% for 5s), health failures {}/{HEALTH_FAILURES}",
        k.connection_share * 100.0,
        k.timeout_share * 100.0,
        DOWN_SHARE * 100.0,
        k.health_failures
    );
    let style = if k.down.is_some() { FAIL } else { PASS };
    stat("kill switch", text, style)
}

fn ms(value: f64) -> String {
    format!("{value:.0}ms")
}

fn latency_row(name: &'static str, s: &StatsSummary) -> Row<'static> {
    Row::new(vec![
        name.to_owned(),
        ms(s.latency.p50_ms),
        ms(s.latency.p95_ms),
        ms(s.latency.p99_ms),
        s.requests.to_string(),
        format!("{:.2}%", s.error_rate * 100.0),
    ])
}

/// Valid traffic (the last window), then fuzzed traffic (so far).
fn latency_table(s: &StressSnapshot) -> Table<'static> {
    let mut rows = vec![latency_row("valid", &s.valid)];
    if let Some(fuzzed) = &s.fuzzed {
        rows.push(latency_row("fuzzed", fuzzed));
    }
    Table::new(rows, [Constraint::Length(8); 6])
        .header(Row::new(["traffic", "p50", "p95", "p99", "requests", "errors"]).style(LABEL))
}

/// The last windows with their SLO status.
fn window_table(s: &StressSnapshot) -> Table<'static> {
    let rows = s.windows.iter().map(|w| {
        let style = match w.status {
            "ok" => PASS,
            "failed" => FAIL,
            _ => Style::new().fg(Color::Yellow),
        };
        Row::new(vec![
            w.index.to_string(),
            format!("{} req/s", format_rate(w.offered)),
            ms(w.p95_ms),
            format!("{:.2}%", w.error_rate * 100.0),
            w.status.to_owned(),
            w.failed.join(", "),
        ])
        .style(style)
    });
    Table::new(
        rows,
        [
            Constraint::Length(6),
            Constraint::Length(14),
            Constraint::Length(9),
            Constraint::Length(8),
            Constraint::Length(17),
            Constraint::Min(10),
        ],
    )
    .header(Row::new(["window", "offered", "p95", "errors", "SLOs", "failed"]).style(LABEL))
}

fn finding_lines(s: &StressSnapshot) -> Vec<Line<'static>> {
    let mut lines = vec![Line::styled("findings", LABEL)];
    if s.findings.is_empty() {
        lines.push(Line::raw("none yet"));
    }
    lines.extend(s.findings.iter().map(|f| Line::styled(f.clone(), FAIL)));
    lines
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use rustman_bench::metrics::LatencySummary;
    use rustman_bench::stress::ModeKind;
    use rustman_bench::stress::kill::KillState;
    use rustman_bench::stress::live::WindowLine;

    use super::*;

    fn stats(requests: u64, p95: f64, error_rate: f64) -> StatsSummary {
        StatsSummary {
            requests,
            error_rate,
            latency: LatencySummary {
                p50_ms: p95 / 2.0,
                p95_ms: p95,
                p99_ms: p95 * 1.5,
                ..LatencySummary::default()
            },
            ..StatsSummary::default()
        }
    }

    fn snapshot() -> StressSnapshot {
        StressSnapshot {
            mode: ModeKind::Breakpoint,
            phase: "load",
            elapsed: Duration::from_secs(95),
            step: 4,
            offered: 33.75,
            achieved: 33.5,
            valid: stats(168, 41.0, 0.0),
            fuzzed: Some(stats(42, 120.0, 0.25)),
            errors: BTreeMap::from([("5xx".to_owned(), 3)]),
            windows: vec![
                WindowLine {
                    index: 17,
                    offered: 33.75,
                    p95_ms: 41.0,
                    error_rate: 0.0,
                    status: "ok",
                    failed: Vec::new(),
                },
                WindowLine {
                    index: 18,
                    offered: 33.75,
                    p95_ms: 1400.0,
                    error_rate: 0.02,
                    status: "failed",
                    failed: vec!["p95<300ms".into()],
                },
            ],
            steps: Vec::new(),
            kill: KillState {
                connection_share: 0.1,
                ..KillState::default()
            },
            killed: None,
            findings: vec!["high server-error-on-malformed-input: deep-nesting".into()],
        }
    }

    fn draw(snapshot: &StressSnapshot) -> String {
        let mut terminal = Terminal::new(TestBackend::new(110, 18)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), snapshot, "rustman stress breakpoint"))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn the_dashboard_shows_valid_and_fuzzed_traffic_slos_and_the_kill_switch() {
        insta::assert_snapshot!(draw(&snapshot()));
    }

    #[test]
    fn a_kill_is_shown() {
        let mut killed = snapshot();
        killed.killed =
            Some("killed: target down at 180 req/s (connection errors above 50% for 5s)".into());
        let text = draw(&killed);
        assert!(text.contains("killed: target down at 180 req/s"), "{text}");
    }
}
