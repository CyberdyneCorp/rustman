//! `rustman monitor daemon` (monitors "Local daemon", design D2): every
//! monitor of the workspace on its schedule, until stopped.
//!
//! - A monitor that comes due while its previous run is still going is
//!   skipped and recorded as `skipped`.
//! - Monitor files are re-read every [`DaemonOptions::reload_every`]; a
//!   change applies without a restart, and an invalid edit keeps the
//!   previous monitors.
//! - One daemon per workspace: [`DaemonLock`] holds
//!   `.rustman/monitors/daemon.lock`.
//! - On shutdown no run starts, and the runs in flight get
//!   [`DaemonOptions::drain`] to finish before they are cancelled.
//! - No port is opened: the daemon only sends.
//!
//! The loop takes its clock and its run function as arguments, so its
//! scheduling is tested on tokio's paused clock without wall-clock waits.

mod lock;
pub mod scheduler;

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::time::Duration;

use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use jiff::Timestamp;
use rustman_core::ConfigError;
use tokio_util::sync::CancellationToken;

use crate::definition::{MONITORS_DIR, Monitor, SUFFIX, load_all};
use crate::evaluate::elapsed;

pub use lock::{DaemonLock, LOCK_FILE, PID_FILE};
pub use scheduler::Scheduler;

/// How long runs in flight may take to finish after a shutdown signal.
pub const DRAIN: Duration = Duration::from_secs(10);
/// How often monitor files are checked for changes.
pub const RELOAD_EVERY: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug)]
pub struct DaemonOptions {
    pub reload_every: Duration,
    pub drain: Duration,
}

impl Default for DaemonOptions {
    fn default() -> Self {
        Self {
            reload_every: RELOAD_EVERY,
            drain: DRAIN,
        }
    }
}

/// The time the daemon schedules by.
pub trait Clock {
    fn now(&self) -> Timestamp;
}

/// The system clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}

/// A run: the lines to log when it ends.
pub type RunFuture<'a> = Pin<Box<dyn Future<Output = Vec<String>> + 'a>>;

/// The runs in flight, each resolving to its monitor's name and lines.
type InFlight<'a> = FuturesUnordered<Pin<Box<dyn Future<Output = (String, Vec<String>)> + 'a>>>;

/// What the daemon does with a monitor that comes due.
pub trait Executor<'a> {
    /// Run `monitor`, due at `due`; `cancel` fires when the drain time is
    /// over. Resolves to the lines to log.
    fn run(&self, monitor: Monitor, due: Timestamp, cancel: CancellationToken) -> RunFuture<'a>;
    /// Record a run of `monitor` due at `due` as skipped; the lines to log.
    fn skip(&self, monitor: &Monitor, due: Timestamp) -> Vec<String>;
}

/// How the daemon is stopped.
#[derive(Clone, Debug, Default)]
pub struct Stop {
    /// Start no more runs, drain the ones in flight and return.
    pub shutdown: CancellationToken,
    /// Cancel the runs in flight now (a second Ctrl+C).
    pub kill: CancellationToken,
}

/// Run the monitors of the workspace at `root` until `stop.shutdown`
/// fires. `log` receives one line per event. Fails only when the monitor
/// files are invalid at start.
pub async fn run<'a>(
    root: &Path,
    clock: &dyn Clock,
    options: DaemonOptions,
    stop: &Stop,
    executor: &dyn Executor<'a>,
    log: &dyn Fn(&str),
) -> Result<(), ConfigError> {
    let mut monitors = load_all(root)?;
    let mut files = monitor_files(root);
    let mut scheduler = Scheduler::default();
    scheduler.sync(&monitors, clock.now());
    log(&format!(
        "monitor daemon started: {} monitor(s), no listening port",
        monitors.len()
    ));
    let cancel_runs = stop.kill.child_token();
    let mut running: InFlight<'a> = FuturesUnordered::new();
    let mut busy: Vec<String> = Vec::new();
    let mut last_reload = clock.now();
    loop {
        let now = clock.now();
        for (name, due) in scheduler.take_due(now) {
            let Some(monitor) = monitors.iter().find(|m| m.name == name) else {
                continue;
            };
            if busy.contains(&name) {
                executor.skip(monitor, due).iter().for_each(|l| log(l));
                continue;
            }
            busy.push(name.clone());
            let run = executor.run(monitor.clone(), due, cancel_runs.child_token());
            running.push(Box::pin(async move { (name, run.await) }));
        }
        let wait = scheduler
            .next_due()
            .map_or(options.reload_every, |due| elapsed(now, due))
            .min(options.reload_every);
        tokio::select! {
            () = stop.shutdown.cancelled() => break,
            Some((name, lines)) = running.next(), if !running.is_empty() => {
                busy.retain(|b| *b != name);
                lines.iter().for_each(|l| log(l));
            }
            () = tokio::time::sleep(wait) => {}
        }
        let now = clock.now();
        if elapsed(last_reload, now) >= options.reload_every {
            last_reload = now;
            reload(root, &mut files, &mut monitors, &mut scheduler, now, log);
        }
    }
    drain(running, options.drain, &cancel_runs, log).await;
    Ok(())
}

/// Re-read the monitor files when they changed.
fn reload(
    root: &Path,
    files: &mut Vec<(String, Vec<u8>)>,
    monitors: &mut Vec<Monitor>,
    scheduler: &mut Scheduler,
    now: Timestamp,
    log: &dyn Fn(&str),
) {
    let current = monitor_files(root);
    if current == *files {
        return;
    }
    *files = current;
    match load_all(root) {
        Ok(loaded) => {
            scheduler.sync(&loaded, now);
            log(&format!("reloaded monitors: {} monitor(s)", loaded.len()));
            *monitors = loaded;
        }
        Err(e) => log(&format!(
            "monitor files changed but do not load; keeping the previous monitors: {e}"
        )),
    }
}

/// Wait up to `drain` for the runs in flight, then cancel them and wait
/// for what they record.
async fn drain<'a>(
    mut running: InFlight<'a>,
    drain: Duration,
    cancel_runs: &CancellationToken,
    log: &dyn Fn(&str),
) {
    if !running.is_empty() {
        log(&format!(
            "stopping: waiting up to {} for {} run(s) in flight",
            crate::duration::format(drain),
            running.len()
        ));
    }
    let deadline = tokio::time::sleep(drain);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            next = running.next() => match next {
                Some((_, lines)) => lines.iter().for_each(|l| log(l)),
                None => break,
            },
            () = &mut deadline, if !cancel_runs.is_cancelled() => cancel_runs.cancel(),
        }
    }
    log("monitor daemon stopped");
}

/// The monitor files with their contents, sorted: what a reload compares.
fn monitor_files(root: &Path) -> Vec<(String, Vec<u8>)> {
    let Ok(entries) = std::fs::read_dir(root.join(MONITORS_DIR)) else {
        return Vec::new();
    };
    let mut files: Vec<(String, Vec<u8>)> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let text = name
                .ends_with(SUFFIX)
                .then(|| std::fs::read(e.path()).ok())??;
            Some((name, text))
        })
        .collect();
    files.sort();
    files
}

#[cfg(test)]
mod tests;
