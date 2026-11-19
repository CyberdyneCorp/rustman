//! OTLP export of client spans (the `otlp` feature; trace-propagation spec
//! "Exported client spans" and "Export failures never affect requests",
//! design D6).
//!
//! One [`Exporter`] per collector, created the first time a run enables
//! export ([`exporter`]) and kept for the process. Spans are queued (at
//! most 2048, the oldest dropped first) and sent in batches by a worker
//! thread with its own runtime every 5 seconds, when a batch fills up, or
//! on [`Exporter::flush`]. Exchanges never wait for it, and a failed
//! export only produces a warning, at most once a minute.

mod encode;
mod send;

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use rustman_core::trace::{ClientSpan, ExportTarget, SpanQueue, WarnLimiter};

use self::send::Sender;

/// How long spans wait before a batch is sent.
pub const SCHEDULED_DELAY: Duration = Duration::from_secs(5);
/// A batch this large is sent without waiting.
const MAX_BATCH: usize = 512;

type Exporters = Mutex<HashMap<ExportTarget, Arc<Exporter>>>;

fn exporters() -> &'static Exporters {
    static EXPORTERS: OnceLock<Exporters> = OnceLock::new();
    EXPORTERS.get_or_init(Mutex::default)
}

/// The exporter of `target`, created (with its worker) on first use.
pub fn exporter(target: &ExportTarget) -> Arc<Exporter> {
    lock(exporters())
        .entry(target.clone())
        .or_insert_with(|| Arc::new(Exporter::start(target.clone())))
        .clone()
}

/// Flush every exporter of the process, each waiting at most `wait`, and
/// return their pending warnings: for a front end that quits (the TUI).
pub fn flush_all(wait: Duration) -> Vec<String> {
    let all: Vec<Arc<Exporter>> = lock(exporters()).values().cloned().collect();
    all.iter()
        .flat_map(|exporter| {
            exporter.flush(wait);
            exporter.take_warnings()
        })
        .collect()
}

/// Queues spans for one collector and reports failures.
pub struct Exporter {
    shared: Arc<Shared>,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    /// Wakes the worker: a full batch or a flush.
    wake: Condvar,
    /// Wakes flushes: a batch was handled.
    handled: Condvar,
}

#[derive(Default)]
struct State {
    queue: SpanQueue<ClientSpan>,
    /// Flushes asked for, and the latest one the worker completed.
    requested: u64,
    completed: u64,
    limiter: WarnLimiter,
    warnings: Vec<String>,
}

impl Exporter {
    fn start(target: ExportTarget) -> Self {
        let shared = Arc::new(Shared::default());
        let worker = shared.clone();
        let spawned = std::thread::Builder::new()
            .name("rustman-otlp".into())
            .spawn(move || run_worker(&worker, target));
        if let Err(e) = spawned {
            shared.no_worker(&e);
        }
        Self { shared }
    }

    /// Queue `span` for export; never blocks on the network.
    pub fn record(&self, span: ClientSpan) {
        let mut state = lock(&self.shared.state);
        state.queue.push(span);
        if state.queue.len() >= MAX_BATCH {
            self.shared.wake.notify_all();
        }
    }

    /// Send everything queued now, waiting at most `wait`; `false` when
    /// the export did not finish in time.
    pub fn flush(&self, wait: Duration) -> bool {
        let mut state = lock(&self.shared.state);
        state.requested += 1;
        let ticket = state.requested;
        self.shared.wake.notify_all();
        let (state, timeout) = self
            .shared
            .handled
            .wait_timeout_while(state, wait, |s| s.completed < ticket)
            .unwrap_or_else(PoisonError::into_inner);
        drop(state);
        !timeout.timed_out()
    }

    /// The warnings shown since the last call, at most one a minute.
    pub fn take_warnings(&self) -> Vec<String> {
        std::mem::take(&mut lock(&self.shared.state).warnings)
    }
}

impl Shared {
    fn warn(&self, message: &str) {
        let mut state = lock(&self.state);
        if state.limiter.allow(Instant::now()) {
            tracing::warn!("{message}");
            state.warnings.push(message.to_owned());
        }
    }

    /// Without a worker nothing is sent: warn, and complete every flush
    /// now instead of letting each one wait its full timeout.
    fn no_worker(&self, error: &std::io::Error) {
        self.warn(&format!("cannot start the export thread: {error}"));
        lock(&self.state).completed = u64::MAX;
    }

    /// The next batch, once the delay passed, a batch filled up or a flush
    /// was asked for; with the flush it completes.
    fn next_batch(&self) -> (Vec<ClientSpan>, u64) {
        let state = lock(&self.state);
        let (mut state, _) = self
            .wake
            .wait_timeout_while(state, SCHEDULED_DELAY, |s| {
                s.requested == s.completed && s.queue.len() < MAX_BATCH
            })
            .unwrap_or_else(PoisonError::into_inner);
        (state.queue.drain(), state.requested)
    }

    fn complete(&self, ticket: u64) {
        lock(&self.state).completed = ticket;
        self.handled.notify_all();
    }
}

fn run_worker(shared: &Shared, target: ExportTarget) {
    let host = target.host();
    let service = target.service_name.clone();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    let sender = match runtime {
        Ok(runtime) => Sender::new(target).map(|sender| (runtime, sender)),
        Err(e) => Err(e.to_string()),
    };
    loop {
        let (batch, ticket) = shared.next_batch();
        if !batch.is_empty() {
            let body = encode::encode(&batch, &service);
            let result = match &sender {
                Ok((runtime, sender)) => runtime.block_on(sender.export(body)),
                Err(e) => Err(e.clone()),
            };
            if let Err(e) = result {
                shared.warn(&format!(
                    "trace export to {host} failed: {e} ({} span(s) dropped; requests are unaffected)",
                    batch.len()
                ));
            }
        }
        shared.complete(ticket);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: when the worker thread could not start, every flush
    /// waited its full timeout for a batch no worker would handle.
    #[test]
    fn flush_returns_at_once_without_a_worker() {
        let exporter = Exporter {
            shared: Arc::new(Shared::default()),
        };
        exporter
            .shared
            .no_worker(&std::io::Error::other("no threads"));
        let started = Instant::now();
        assert!(exporter.flush(Duration::from_secs(30)));
        assert!(exporter.flush(Duration::from_secs(30)));
        assert!(started.elapsed() < Duration::from_secs(10));
        let warnings = exporter.take_warnings();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("cannot start the export thread"));
    }
}
