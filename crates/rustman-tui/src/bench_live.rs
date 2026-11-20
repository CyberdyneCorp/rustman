//! The live side of `rustman bench`: while load runs, a thread of its own
//! reads [`Snapshot`]s of the metrics and
//!
//! - draws the dashboard ([`crate::bench_view`]) at most 4 times a second
//!   when stdout is a terminal ([`Display::Dashboard`]), on the alternate
//!   screen and without raw mode, so Ctrl+C still reaches the bench;
//! - or prints a [`progress_line`] every 10 seconds ([`Display::Lines`]);
//! - and streams each closed second to the viz companion
//!   ([`crate::bench_viz`]), when one is open.
//!
//! The thread only reads the metrics, so the bench never waits on
//! rendering, and [`LiveHandle::finish`] stops it, restores the terminal
//! and sends the final viz event.

use std::io::{IsTerminal, Write};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rustman_bench::live::{FRAME, PROGRESS_EVERY, progress_line};
use rustman_bench::{BenchOutcome, Live};

use crate::bench_view;
use crate::bench_viz::{BenchViz, VizFeed};
use crate::live_screen::LiveScreen;

/// How the run is shown while it goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Display {
    /// The full-screen dashboard on stdout (a terminal).
    Dashboard,
    /// One progress line per period.
    Lines { every: Duration },
}

impl Display {
    /// The dashboard when stdout is a terminal that no report is written
    /// to, else a progress line every 10 seconds.
    pub fn detect(report_on_stdout: bool) -> Self {
        if !report_on_stdout && std::io::stdout().is_terminal() {
            Self::Dashboard
        } else {
            Self::Lines {
                every: PROGRESS_EVERY,
            }
        }
    }
}

/// What the live thread shows and where.
pub struct LiveOptions {
    pub display: Display,
    /// The dashboard title, such as `rustman bench https://staging.example.com`.
    pub title: String,
    /// Where progress lines go.
    pub lines: Box<dyn Write + Send>,
    pub viz: Option<BenchViz>,
}

/// The running live thread.
pub struct LiveHandle {
    stop: mpsc::Sender<()>,
    thread: JoinHandle<Option<VizFeed>>,
}

fn send_new(viz: &mut VizFeed, live: &Live) {
    viz.send_new(|n| live.seconds_since(n), |elapsed| live.remaining(elapsed));
}

/// Start showing `live`.
pub fn spawn(live: Live, options: LiveOptions) -> LiveHandle {
    let (stop, stopped) = mpsc::channel();
    let thread = std::thread::spawn(move || run(&live, options, &stopped));
    LiveHandle { stop, thread }
}

impl LiveHandle {
    /// Stop showing the run (restoring the terminal), then send the
    /// seconds not sent yet and `bench_finished` to the viz companion. A
    /// bench that failed (`None`) just closes the stream.
    pub fn finish(self, live: &Live, outcome: Option<&BenchOutcome>) {
        let _ = self.stop.send(());
        let Ok(Some(mut viz)) = self.thread.join() else {
            return;
        };
        send_new(&mut viz, live);
        viz.stream.finish(outcome);
    }
}

fn run(live: &Live, options: LiveOptions, stopped: &mpsc::Receiver<()>) -> Option<VizFeed> {
    let LiveOptions {
        display,
        title,
        mut lines,
        viz,
    } = options;
    let mut viz = viz.map(VizFeed::new);
    let mut screen = match display {
        Display::Dashboard => Screen::open(),
        Display::Lines { .. } => None,
    };
    let period = match (display, &screen) {
        (Display::Lines { every }, _) => every,
        (Display::Dashboard, Some(_)) => FRAME,
        // No terminal after all: lines instead.
        (Display::Dashboard, None) => PROGRESS_EVERY,
    };
    let tick = FRAME.min(period);
    let mut next_line = Instant::now() + period;
    // Until finish() (or a dropped handle) ends the wait early.
    while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(tick) {
        if let Some(viz) = &mut viz {
            send_new(viz, live);
        }
        if let Some(screen) = &mut screen {
            screen.draw(live, &title);
        } else if Instant::now() >= next_line {
            next_line += period;
            let _ = writeln!(lines, "{}", progress_line(&live.snapshot()));
            let _ = lines.flush();
        }
    }
    drop(screen);
    viz
}

/// The dashboard's terminal, drawing `live`'s snapshot.
struct Screen(LiveScreen);

impl Screen {
    fn open() -> Option<Self> {
        LiveScreen::open().map(Self)
    }

    fn draw(&mut self, live: &Live, title: &str) {
        let snapshot = live.snapshot();
        self.0
            .draw(|frame| bench_view::render(frame, frame.area(), &snapshot, title));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use rustman_bench::metrics::{LatencyKind, Metrics, Sample};

    use super::*;

    /// A writer the test can read back.
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

    impl Shared {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    fn live() -> Live {
        let metrics = Arc::new(Metrics::new(LatencyKind::Service, vec!["a".into()]));
        metrics.record(&Sample {
            latency: Duration::from_millis(5),
            ..Sample::default()
        });
        Live::new(metrics, vec!["p95<1s".parse().unwrap()], None)
    }

    fn outcome(live: &Live) -> BenchOutcome {
        BenchOutcome {
            summary: live.metrics().summary(),
            series: Vec::new(),
            thresholds: Vec::new(),
            error_samples: Vec::new(),
            backoffs: 0,
            aborted: None,
            generator_lag: Duration::ZERO,
            warnings: Vec::new(),
            started_at: String::new(),
            finished_at: String::new(),
        }
    }

    /// The non-TTY output: one plain progress line per period, never the
    /// dashboard (no escape sequences).
    #[test]
    fn without_a_terminal_progress_is_one_line_per_period() {
        let live = live();
        let out = Shared::default();
        let handle = spawn(
            live.clone(),
            LiveOptions {
                display: Display::Lines {
                    every: Duration::from_millis(300),
                },
                title: "t".into(),
                lines: Box::new(out.clone()),
                viz: None,
            },
        );
        let deadline = Instant::now() + Duration::from_secs(20);
        while out.text().lines().count() < 2 {
            assert!(Instant::now() < deadline, "no progress lines");
            std::thread::sleep(Duration::from_millis(20));
        }
        handle.finish(&live, Some(&outcome(&live)));
        let text = out.text();
        assert!(!text.contains('\x1b'), "{text:?}");
        for line in text.lines() {
            assert!(line.starts_with("bench "), "{line}");
            assert!(line.contains(" 1 requests, "), "{line}");
            assert!(line.ends_with("thresholds 1/1 passing"), "{line}");
        }
    }

    #[test]
    fn a_short_run_prints_no_progress_line() {
        let live = live();
        let out = Shared::default();
        let handle = spawn(
            live.clone(),
            LiveOptions {
                display: Display::Lines {
                    every: PROGRESS_EVERY,
                },
                title: "t".into(),
                lines: Box::new(out.clone()),
                viz: None,
            },
        );
        std::thread::sleep(Duration::from_millis(300));
        handle.finish(&live, Some(&outcome(&live)));
        assert_eq!(out.text(), "");
    }

    #[test]
    fn a_report_on_stdout_never_gets_the_dashboard() {
        assert_eq!(
            Display::detect(true),
            Display::Lines {
                every: PROGRESS_EVERY
            }
        );
    }
}
