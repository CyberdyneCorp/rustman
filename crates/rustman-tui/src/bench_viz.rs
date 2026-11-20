//! `rustman bench --viz`: per-second metric events streamed to the viz
//! companion (add-load-testing, "Live viz streaming"; design D7).
//!
//! The companion is found and greeted the way every producer does it
//! ([`crate::companion`]). Without a `bench` view in its `hello_ack`, the
//! stream is not opened and the caller warns
//! [`NO_BENCH_VIEW`]. Otherwise the stream opens the `bench` view, sends one
//! `bench_tick` per second and a final `bench_finished`.
//!
//! A slow or broken companion never holds the bench: messages go through a
//! bounded queue to a writer thread, a full queue drops the tick (progress
//! may be coalesced), and only `bench_finished` waits, for at most
//! [`FINISH_TIMEOUT`], after which the companion is killed. Viz problems
//! never change the bench result.
//!
//! Events hold counts, timings and threshold expressions only (see
//! [`rustman_bench::live`]), so they carry nothing to redact.

use std::process::Child;
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rustman_bench::BenchOutcome;
use rustman_bench::metrics::BenchSecond;
use rustman_viz_protocol::{
    BenchFinished, BenchThreshold, BenchTick, Close, Message, OpenView, RunState, View,
};

use crate::companion::{Companion, Discovery, connect, detach, write_message};

/// The warning when the companion cannot show a bench.
pub const NO_BENCH_VIEW: &str = "viz companion has no bench view";

/// Messages waiting for the writer; more ticks than this are dropped.
const QUEUE: usize = 16;

/// How long `bench_finished` and `close` may take to be written.
pub const FINISH_TIMEOUT: Duration = Duration::from_secs(5);

/// An open bench stream.
#[derive(Debug)]
pub struct BenchViz {
    queue: SyncSender<Message>,
    writer: JoinHandle<()>,
    child: Child,
    /// The companion's name, from its `hello_ack`.
    pub viewer: String,
}

/// Start the companion and open its `bench` view titled `title`. The error
/// is the warning to show (once), and the bench goes on without viz.
pub fn start(discovery: &Discovery, title: &str) -> Result<BenchViz, String> {
    let Companion {
        child,
        mut stdin,
        ack,
        ..
    } = connect(discovery).map_err(|e| format!("--viz: {e}"))?;
    if !ack.views.contains(&View::Bench) {
        let _ = write_message(&mut stdin, &Message::Close(Close::default()));
        drop(stdin);
        detach(child);
        return Err(NO_BENCH_VIEW.to_owned());
    }
    let (queue, inbox) = mpsc::sync_channel::<Message>(QUEUE);
    let writer = std::thread::spawn(move || {
        // A failed write ends the stream; the bench goes on.
        for message in inbox {
            if write_message(&mut stdin, &message).is_err() {
                return;
            }
        }
    });
    let open = OpenView {
        view: View::Bench,
        title: Some(title.to_owned()),
    };
    let _ = queue.try_send(Message::OpenView(open));
    Ok(BenchViz {
        queue,
        writer,
        child,
        viewer: ack.viewer,
    })
}

impl BenchViz {
    /// Send closed `second`, with `remaining` load time when known. Dropped
    /// when the companion is behind.
    pub fn tick(&self, second: &BenchSecond, remaining: Option<Duration>) {
        let message = Message::BenchTick(tick(second, remaining));
        match self.queue.try_send(message) {
            Ok(()) | Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {}
        }
    }

    /// Send `bench_finished` (when the bench has an outcome) and `close`,
    /// waiting at most [`FINISH_TIMEOUT`]; a companion that does not read
    /// them in time is killed. The companion's windows may stay open after
    /// this.
    pub fn finish(self, outcome: Option<&BenchOutcome>) {
        let deadline = Instant::now() + FINISH_TIMEOUT;
        let finished = outcome.map(|o| Message::BenchFinished(finished(o)));
        for message in finished
            .into_iter()
            .chain([Message::Close(Close::default())])
        {
            send_until(&self.queue, message, deadline);
        }
        let Self {
            queue,
            writer,
            mut child,
            ..
        } = self;
        drop(queue);
        while !writer.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if !writer.is_finished() {
            let _ = child.kill();
        }
        detach(child);
    }
}

/// Queue a message that must not be dropped, retrying until `deadline`.
fn send_until(queue: &SyncSender<Message>, mut message: Message, deadline: Instant) {
    loop {
        match queue.try_send(message) {
            Ok(()) | Err(TrySendError::Disconnected(_)) => return,
            Err(TrySendError::Full(back)) if Instant::now() < deadline => {
                message = back;
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(TrySendError::Full(_)) => return,
        }
    }
}

/// The `bench_tick` of one closed second.
pub fn tick(second: &BenchSecond, remaining: Option<Duration>) -> BenchTick {
    BenchTick {
        second: second.second,
        elapsed_ms: u64::from(second.second) * 1000,
        remaining_ms: remaining.map(|r| u64::try_from(r.as_millis()).unwrap_or(u64::MAX)),
        active_vus: second.active_vus,
        requests: second.requests,
        errors: second.errors,
        rps: second.rps,
        p50_ms: second.p50_ms,
        p95_ms: second.p95_ms,
        p99_ms: second.p99_ms,
    }
}

/// The `bench_finished` of a whole bench.
pub fn finished(outcome: &BenchOutcome) -> BenchFinished {
    let summary = &outcome.summary;
    let overall = &summary.overall;
    BenchFinished {
        state: if outcome.aborted.is_some() {
            RunState::Aborted
        } else {
            RunState::Completed
        },
        duration_ms: summary.elapsed_ms.round() as u64,
        iterations: summary.iterations,
        requests: overall.requests,
        errors: overall.errors.values().sum(),
        rps: summary.rps,
        p50_ms: overall.latency.p50_ms,
        p95_ms: overall.latency.p95_ms,
        p99_ms: overall.latency.p99_ms,
        thresholds: outcome
            .thresholds
            .iter()
            .map(|t| BenchThreshold {
                expression: t.expression.clone(),
                passed: t.passed,
                actual: t.actual.clone(),
            })
            .collect(),
        aborted: outcome.aborted.map(|a| a.to_string()),
    }
}
