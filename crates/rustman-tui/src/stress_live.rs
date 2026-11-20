//! The live side of `rustman stress`: while the run goes on, a thread of
//! its own reads [`StressLive`] snapshots and
//!
//! - draws the dashboard ([`crate::stress_view`]) at most 4 times a second
//!   when stdout is a terminal, on the alternate screen and without raw
//!   mode, so Ctrl+C still reaches the run;
//! - or prints one line per step as steps end (stdout is not a terminal);
//! - and streams each closed second to the viz companion's `stress` view,
//!   when one is open.
//!
//! The thread only reads, so load generation never waits on it.
//! [`StressLiveHandle::finish`] stops it, restores the terminal, prints
//! the step lines not printed yet and closes the viz stream.

use std::io::{IsTerminal, Write};
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::Duration;

use rustman_bench::Metrics;
use rustman_bench::live::FRAME;
use rustman_bench::stress::live::StressLive;

use crate::bench_viz::{self, BenchViz, VizFeed};
use crate::companion::Discovery;
use crate::live_screen::LiveScreen;
use crate::stress_view;

/// The warning when the companion cannot show a stress run.
pub const NO_STRESS_VIEW: &str = "viz companion has no stress view";

/// How often step lines are looked for when stdout is not a terminal.
const LINES_EVERY: Duration = Duration::from_millis(250);

/// Start the companion and open its `stress` view titled `title`. The
/// error is the warning to show (once): the run goes on without viz.
pub fn start_viz(discovery: &Discovery, title: &str) -> Result<BenchViz, String> {
    bench_viz::start_view(
        discovery,
        title,
        rustman_viz_protocol::View::Stress,
        NO_STRESS_VIEW,
    )
}

/// What the live thread shows and where.
pub struct StressLiveOptions {
    /// The full-screen dashboard (stdout is a terminal).
    pub dashboard: bool,
    pub title: String,
    /// Where step lines go.
    pub lines: Box<dyn Write + Send>,
    pub viz: Option<BenchViz>,
}

impl StressLiveOptions {
    /// The dashboard when stdout is a terminal no report is written to.
    pub fn dashboard(report_on_stdout: bool) -> bool {
        !report_on_stdout && std::io::stdout().is_terminal()
    }
}

/// The running live thread.
pub struct StressLiveHandle {
    stop: mpsc::Sender<()>,
    thread: JoinHandle<Shown>,
    live: StressLive,
}

struct Shown {
    lines: Box<dyn Write + Send>,
    printed: usize,
    viz: Option<VizFeed>,
}

/// Start showing `live`, streaming the seconds of `metrics` to the viz
/// companion.
pub fn spawn(
    live: StressLive,
    metrics: Arc<Metrics>,
    options: StressLiveOptions,
) -> StressLiveHandle {
    let (stop, stopped) = mpsc::channel();
    let shared = live.clone();
    let thread = std::thread::spawn(move || run(&shared, &metrics, options, &stopped));
    StressLiveHandle { stop, thread, live }
}

impl StressLiveHandle {
    /// Stop showing the run (restoring the terminal), print the step lines
    /// not printed yet and close the viz stream.
    pub fn finish(self) {
        let _ = self.stop.send(());
        let Ok(mut shown) = self.thread.join() else {
            return;
        };
        print_new(&self.live, &mut shown);
        if let Some(viz) = shown.viz {
            viz.stream.finish(None);
        }
    }
}

fn print_new(live: &StressLive, shown: &mut Shown) {
    for line in live.lines_since(shown.printed) {
        let _ = writeln!(shown.lines, "{line}");
        shown.printed += 1;
    }
    let _ = shown.lines.flush();
}

fn run(
    live: &StressLive,
    metrics: &Metrics,
    options: StressLiveOptions,
    stopped: &mpsc::Receiver<()>,
) -> Shown {
    let StressLiveOptions {
        dashboard,
        title,
        lines,
        viz,
    } = options;
    let mut shown = Shown {
        lines,
        printed: 0,
        viz: viz.map(VizFeed::new),
    };
    let mut screen = if dashboard { Screen::open() } else { None };
    let tick = if screen.is_some() { FRAME } else { LINES_EVERY };
    while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(tick) {
        if let Some(viz) = &mut shown.viz {
            viz.send_new(|n| metrics.seconds_since(n), |_| None);
        }
        match &mut screen {
            Some(screen) => screen.draw(live, &title),
            None => print_new(live, &mut shown),
        }
    }
    drop(screen);
    shown
}

/// The dashboard's terminal, drawing `live`'s snapshot.
struct Screen(LiveScreen);

impl Screen {
    fn open() -> Option<Self> {
        LiveScreen::open().map(Self)
    }

    fn draw(&mut self, live: &StressLive, title: &str) {
        let snapshot = live.snapshot();
        self.0
            .draw(|frame| stress_view::render(frame, frame.area(), &snapshot, title));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use rustman_bench::LatencyKind;
    use rustman_bench::stress::ModeKind;
    use rustman_bench::stress::breakpoint::{Step, StepVerdict};

    use super::*;

    #[derive(Clone, Default)]
    struct Shared(Arc<Mutex<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn step(rate: f64, p95_ms: f64, verdict: StepVerdict) -> Step {
        Step {
            rate,
            verdict,
            failure_mode: None,
            achieved_rps: rate * 0.99,
            p95_ms,
            error_rate: 0.0,
            windows: Vec::new(),
            first_failing: None,
        }
    }

    /// The non-TTY output: one plain line per step, no escape sequences.
    #[test]
    fn without_a_terminal_each_step_is_one_line() {
        let live = StressLive::new(ModeKind::Breakpoint);
        let out = Shared::default();
        let metrics = Arc::new(Metrics::new(LatencyKind::Corrected, Vec::new()));
        let handle = spawn(
            live.clone(),
            metrics,
            StressLiveOptions {
                dashboard: false,
                title: "t".into(),
                lines: Box::new(out.clone()),
                viz: None,
            },
        );
        live.steps(&[step(10.0, 12.0, StepVerdict::Ok)]);
        live.steps(&[
            step(10.0, 12.0, StepVerdict::Ok),
            step(15.0, 1450.0, StepVerdict::Broken),
        ]);
        handle.finish();
        let text = String::from_utf8(out.0.lock().unwrap().clone()).unwrap();
        insta::assert_snapshot!(text);
        assert!(!text.contains('\x1b'));
    }
}
