//! The daemon loop on tokio's paused clock: runs, skips and reloads happen
//! at exact virtual times, with no wall-clock wait.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use jiff::SignedDuration;

use super::*;

/// Virtual time: a fixed start plus tokio's (paused) elapsed time.
struct TokioClock {
    base: Timestamp,
    start: tokio::time::Instant,
}

impl TokioClock {
    fn new() -> Self {
        Self {
            base: "2026-03-01T10:00:00Z".parse().unwrap(),
            start: tokio::time::Instant::now(),
        }
    }

    fn secs(&self, at: Timestamp) -> i64 {
        at.duration_since(self.base).as_secs()
    }
}

impl Clock for TokioClock {
    fn now(&self) -> Timestamp {
        let elapsed = SignedDuration::try_from(self.start.elapsed()).unwrap();
        self.base + elapsed
    }
}

/// Runs take `takes`; records when each run and skip happened (seconds
/// since the start).
struct FakeExecutor {
    takes: Duration,
    clock: Rc<TokioClock>,
    started: Rc<RefCell<Vec<i64>>>,
    skipped: RefCell<Vec<i64>>,
}

impl FakeExecutor {
    fn new(takes: Duration, clock: Rc<TokioClock>) -> Self {
        Self {
            takes,
            clock,
            started: Rc::default(),
            skipped: RefCell::default(),
        }
    }
}

impl Executor<'static> for FakeExecutor {
    fn run(
        &self,
        monitor: Monitor,
        _due: Timestamp,
        cancel: CancellationToken,
    ) -> RunFuture<'static> {
        self.started
            .borrow_mut()
            .push(self.clock.secs(self.clock.now()));
        let takes = self.takes;
        Box::pin(async move {
            tokio::select! {
                () = tokio::time::sleep(takes) => vec![format!("{}: pass", monitor.name)],
                () = cancel.cancelled() => vec![format!("{}: error (interrupted)", monitor.name)],
            }
        })
    }

    fn skip(&self, monitor: &Monitor, due: Timestamp) -> Vec<String> {
        self.skipped.borrow_mut().push(self.clock.secs(due));
        vec![format!("{}: skipped", monitor.name)]
    }
}

fn write_monitor(root: &Path, every: &str) {
    let dir = root.join(MONITORS_DIR);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("api-health.rman-monitor.yaml"),
        format!(
            "version: 1\nname: api-health\ntargets: [health/]\nenvironment: prod\nschedule: {{every: {every}}}\n"
        ),
    )
    .unwrap();
}

/// Run the daemon over `root` until `stop_after`; `during` runs alongside
/// (to edit files). Returns the log.
async fn daemon(
    root: &Path,
    executor: &FakeExecutor,
    stop_after: Duration,
    during: impl Future<Output = ()>,
) -> Vec<String> {
    let log = RefCell::new(Vec::new());
    let stop = Stop::default();
    let options = DaemonOptions::default();
    let stopper = async {
        tokio::join!(during, tokio::time::sleep(stop_after));
        stop.shutdown.cancel();
    };
    let logger = |line: &str| log.borrow_mut().push(line.to_owned());
    let (result, ()) = tokio::join!(
        run(
            root,
            executor.clock.as_ref(),
            options,
            &stop,
            executor,
            &logger
        ),
        stopper
    );
    result.unwrap();
    log.into_inner()
}

/// monitors "Overlap skipped": every 30 s with runs of 45 s, the run due
/// while one is going is skipped and recorded, and the next starts on its
/// own due time.
#[tokio::test(start_paused = true)]
async fn a_run_due_during_the_previous_one_is_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    write_monitor(tmp.path(), "30s");
    let clock = Rc::new(TokioClock::new());
    let executor = FakeExecutor::new(Duration::from_secs(45), clock);
    let log = daemon(tmp.path(), &executor, Duration::from_secs(100), async {}).await;
    assert_eq!(*executor.started.borrow(), [0, 60]);
    assert_eq!(*executor.skipped.borrow(), [30, 90]);
    let passes = log.iter().filter(|l| l.ends_with(": pass")).count();
    assert_eq!(passes, 2, "{log:?}");
    assert!(log[0].contains("no listening port"), "{log:?}");
}

/// monitors "File edited while running": `every: 5m` becomes `every: 1m`
/// and applies without a restart.
#[tokio::test(start_paused = true)]
async fn an_edited_schedule_applies_without_a_restart() {
    let tmp = tempfile::tempdir().unwrap();
    write_monitor(tmp.path(), "5m");
    let clock = Rc::new(TokioClock::new());
    let executor = FakeExecutor::new(Duration::from_secs(1), clock);
    let edit = async {
        tokio::time::sleep(Duration::from_secs(10)).await;
        write_monitor(tmp.path(), "1m");
    };
    let log = daemon(tmp.path(), &executor, Duration::from_secs(200), edit).await;
    let started = executor.started.borrow().clone();
    assert_eq!(started.len(), 4, "{started:?}");
    assert_eq!(started[0], 0);
    assert!((70..=72).contains(&started[1]), "{started:?}");
    assert_eq!(started[2] - started[1], 60);
    assert!(
        log.iter().any(|l| l == "reloaded monitors: 1 monitor(s)"),
        "{log:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn an_invalid_edit_keeps_the_previous_monitors() {
    let tmp = tempfile::tempdir().unwrap();
    write_monitor(tmp.path(), "1m");
    let clock = Rc::new(TokioClock::new());
    let executor = FakeExecutor::new(Duration::from_secs(1), clock);
    let edit = async {
        tokio::time::sleep(Duration::from_secs(10)).await;
        write_monitor(tmp.path(), "5s");
    };
    let log = daemon(tmp.path(), &executor, Duration::from_secs(130), edit).await;
    assert_eq!(*executor.started.borrow(), [0, 60, 120]);
    assert!(
        log.iter()
            .any(|l| l.contains("keeping the previous monitors")
                && l.contains("every must be at least 30s")),
        "{log:?}"
    );
}

/// On shutdown the run in flight gets the drain time, then is cancelled.
#[tokio::test(start_paused = true)]
async fn shutdown_drains_then_cancels_the_run_in_flight() {
    let tmp = tempfile::tempdir().unwrap();
    write_monitor(tmp.path(), "5m");
    let clock = Rc::new(TokioClock::new());
    let executor = FakeExecutor::new(Duration::from_secs(60), clock.clone());
    let log = daemon(tmp.path(), &executor, Duration::from_secs(5), async {}).await;
    assert_eq!(clock.secs(clock.now()), 15, "5 s, then the 10 s drain");
    assert_eq!(
        log[1..],
        [
            "stopping: waiting up to 10s for 1 run(s) in flight",
            "api-health: error (interrupted)",
            "monitor daemon stopped",
        ]
    );
    // A run that finishes within the drain time completes normally.
    let executor = FakeExecutor::new(Duration::from_secs(8), clock);
    let log = daemon(tmp.path(), &executor, Duration::from_secs(5), async {}).await;
    assert!(log.contains(&"api-health: pass".to_owned()), "{log:?}");
}
