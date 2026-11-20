//! `rustman monitor daemon`: the scheduling loop of `rustman-monitor` with
//! real runs, the system clock, Ctrl+C/SIGTERM and one log line per event
//! on stderr.

use rustman_core::Workspace;
use rustman_monitor::cycle::{Cycle, CycleContext};
use rustman_monitor::daemon::{
    self, DaemonLock, DaemonOptions, Executor, RunFuture, Stop, SystemClock,
};
use rustman_monitor::{Monitor, Store, Timestamp};
use tokio_util::sync::CancellationToken;

use super::{Session, alerts, store_error};
use crate::commands::exec::{runtime, stop_then_cancel_on_ctrl_c};
use crate::commands::print_warnings;
use crate::exit::{CliError, SUCCESS};

pub(super) fn run(ws: &Workspace) -> Result<u8, CliError> {
    let _lock = DaemonLock::acquire(&ws.root)?;
    let session = Session::open(ws)?;
    compact_all(ws, session.retention)?;
    let ctx = session.context(ws);
    let stop = Stop::default();
    let notice = "stopping after the runs in flight (Ctrl+C again to cancel them)".to_owned();
    stop_then_cancel_on_ctrl_c(&stop.shutdown, &stop.kill, Some(notice));
    let executor = CliExecutor { ctx: &ctx };
    let log = |line: &str| eprintln!("{} {line}", Timestamp::now().strftime("%Y-%m-%dT%H:%M:%SZ"));
    runtime()?.block_on(daemon::run(
        &ws.root,
        &SystemClock,
        DaemonOptions::default(),
        &stop,
        &executor,
        &log,
    ))?;
    print_warnings(&session.secrets.take_warnings());
    Ok(SUCCESS)
}

/// Compaction runs at daemon start, then once a day after runs.
fn compact_all(ws: &Workspace, retention: std::time::Duration) -> Result<(), CliError> {
    let store = Store::new(&ws.root);
    let now = Timestamp::now();
    for monitor in rustman_monitor::load_all(&ws.root)? {
        store
            .compact(&monitor.name, now, retention)
            .map_err(store_error)?;
    }
    Ok(())
}

struct CliExecutor<'a> {
    ctx: &'a CycleContext<'a>,
}

impl<'a> Executor<'a> for CliExecutor<'a> {
    fn run(&self, monitor: Monitor, _due: Timestamp, cancel: CancellationToken) -> RunFuture<'a> {
        let ctx = self.ctx;
        Box::pin(async move {
            match ctx.run(&monitor, cancel).await {
                Ok(cycle) => cycle_lines(&monitor.name, &cycle),
                Err(e) => vec![format!("{}: error: {e}", monitor.name)],
            }
        })
    }

    fn skip(&self, monitor: &Monitor, due: Timestamp) -> Vec<String> {
        let name = &monitor.name;
        match self.ctx.skip(monitor, due) {
            Ok(warnings) => {
                std::iter::once(format!("{name}: skipped: previous run still in progress"))
                    .chain(warnings.into_iter().map(|w| format!("warning: {w}")))
                    .collect()
            }
            Err(e) => vec![format!("{name}: cannot record the skipped run: {e}")],
        }
    }
}

/// `api-health: fail in 120 ms (health/status: ...)`, then warnings and
/// alerts.
fn cycle_lines(name: &str, cycle: &Cycle) -> Vec<String> {
    let result = &cycle.run.result;
    let reason = result.message.iter().chain(&result.failures).next();
    let mut line = format!("{name}: {} in {:.0} ms", result.outcome, result.duration_ms);
    if let Some(reason) = reason {
        line = format!("{line} ({reason})");
    }
    std::iter::once(line)
        .chain(cycle.warnings.iter().map(|w| format!("warning: {w}")))
        .chain(
            alerts::lines(&cycle.alerts)
                .into_iter()
                .map(|l| format!("{name}: {}", l.trim_start())),
        )
        .collect()
}
