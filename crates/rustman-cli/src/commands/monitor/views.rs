//! `rustman monitor status` and `rustman monitor history`: the store
//! only, no network.

use std::io::Write;

use rustman_core::{ConfigError, Workspace};
use rustman_monitor::status::{self, HistoryEntry, MonitorStatus, trend};
use rustman_monitor::{Monitor, Outcome, Store, Timestamp, duration};
use rustman_tui::companion::Discovery;
use rustman_tui::monitor_viz;

use super::{HistoryArgs, store_error};
use crate::exit::{CliError, SUCCESS};

/// Shown with `--viz` when the companion cannot show the trends, before
/// why.
const VIZ_NOTE: &str = "note: the viz companion is unavailable for monitor trends";

pub(super) fn status(
    ws: &Workspace,
    monitors: &[Monitor],
    json: bool,
    viz: bool,
) -> Result<u8, CliError> {
    let store = Store::new(&ws.root);
    let now = Timestamp::now();
    let statuses = monitors
        .iter()
        .map(|m| status::status(&store, m, now))
        .collect::<Result<Vec<_>, _>>()
        .map_err(store_error)?;
    if viz {
        open_trends(&store, &statuses, now);
    }
    let mut out = anstream::stdout().lock();
    if json {
        let text = serde_json::to_string_pretty(&serde_json::json!({ "monitors": statuses }))
            .expect("statuses serialize");
        let _ = writeln!(out, "{text}");
        return Ok(SUCCESS);
    }
    if statuses.is_empty() {
        let _ = writeln!(out, "no monitors (add monitors/<name>.rman-monitor.yaml)");
    }
    for (i, s) in statuses.iter().enumerate() {
        if i > 0 {
            let _ = writeln!(out);
        }
        for line in status_lines(s, now) {
            let _ = writeln!(out, "{line}");
        }
    }
    Ok(SUCCESS)
}

/// Open the trends in the viz companion, or say why the text status is
/// all there is. Never changes the exit code.
fn open_trends(store: &Store, statuses: &[MonitorStatus], now: Timestamp) {
    let trends = statuses
        .iter()
        .map(|s| trend::trend(store, s, now))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string());
    match trends.and_then(|t| monitor_viz::show(&Discovery::from_env(), &t)) {
        Ok(viewer) => eprintln!("monitor trends opened in {viewer}"),
        Err(why) => eprintln!("{VIZ_NOTE} ({why}); showing the text status"),
    }
}

/// One block per monitor:
///
/// ```text
/// api-health (prod, every 5m)
///   last      fail 3 minutes ago (2026-03-01T10:00:00Z)
///   uptime    98.95% 24h · 99.50% 7d · 99.90% 30d
///   p95 24h   123 ms  ▁▂▃▄▅▆▇█
///   coverage  99.65%
///   firing    consecutive_failures: 2 (since 2026-03-01T09:40:00Z)
/// ```
fn status_lines(s: &MonitorStatus, now: Timestamp) -> Vec<String> {
    let pct = |v: Option<f64>| v.map_or("-".to_owned(), |v| format!("{v:.2}%"));
    let last = s.last.as_ref().map_or("never run".to_owned(), |l| {
        format!("{} {} ({})", l.outcome, status::ago(l.at, now), l.at)
    });
    let p95 = s
        .p95_24h_ms
        .map_or("-".to_owned(), |v| format!("{v:.0} ms"));
    let mut lines = vec![
        format!("{} ({}, {})", s.name, s.environment, s.schedule),
        format!("  last      {last}"),
        format!(
            "  uptime    {} 24h · {} 7d · {} 30d",
            pct(s.uptime.day),
            pct(s.uptime.week),
            pct(s.uptime.month)
        ),
        format!("  p95 24h   {p95}  {}", s.sparkline),
        format!("  coverage  {}", pct(s.coverage_24h)),
    ];
    if s.firing.is_empty() {
        lines.push("  firing    none".to_owned());
    }
    for firing in &s.firing {
        lines.push(format!(
            "  firing    {} (since {})",
            firing.rule, firing.since
        ));
    }
    lines
}

pub(super) fn history(
    ws: &Workspace,
    monitors: &[Monitor],
    args: &HistoryArgs,
) -> Result<u8, CliError> {
    let monitor = rustman_monitor::find(monitors, &args.name)?;
    let usage = |m: String| ConfigError::Usage(m);
    let since = args
        .since
        .as_deref()
        .map(|s| duration::parse(s).map_err(|e| usage(format!("--since: {e}"))))
        .transpose()?;
    let outcome = args.outcome.as_deref().map(parse_outcome).transpose()?;
    let store = Store::new(&ws.root);
    let entries = status::history(&store, &monitor.name, since, outcome, Timestamp::now())
        .map_err(store_error)?;
    let mut out = anstream::stdout().lock();
    for entry in &entries {
        if args.json {
            let line = serde_json::to_string(entry).expect("entries serialize");
            let _ = writeln!(out, "{line}");
            continue;
        }
        for line in entry_lines(entry) {
            let _ = writeln!(out, "{line}");
        }
    }
    if entries.is_empty() && !args.json {
        let _ = writeln!(out, "no results");
    }
    Ok(SUCCESS)
}

fn parse_outcome(text: &str) -> Result<Outcome, ConfigError> {
    [
        Outcome::Pass,
        Outcome::Fail,
        Outcome::Error,
        Outcome::Skipped,
    ]
    .into_iter()
    .find(|o| o.as_str() == text)
    .ok_or_else(|| {
        ConfigError::Usage(format!(
            "--outcome: '{text}' is not one of pass, fail, error or skipped"
        ))
    })
}

/// `<time>  fail   123 ms`, then the failures indented; an aggregate as
/// `<hour>  hourly 12 runs: 11 pass, 1 fail, 0 error, 0 skipped; p95 120 ms`.
fn entry_lines(entry: &HistoryEntry) -> Vec<String> {
    match entry {
        HistoryEntry::Run(run) => {
            let head = format!(
                "{}  {:<7} {:>6.0} ms",
                run.at,
                run.outcome.as_str(),
                run.duration_ms
            );
            std::iter::once(head)
                .chain(
                    run.message
                        .iter()
                        .chain(&run.failures)
                        .map(|m| format!("    {m}")),
                )
                .collect()
        }
        HistoryEntry::Hour(h) => vec![format!(
            "{}  hourly  {} runs: {} pass, {} fail, {} error, {} skipped; p95 {:.0} ms",
            h.hour, h.runs, h.passes, h.failures, h.errors, h.skipped, h.p95_ms
        )],
    }
}
