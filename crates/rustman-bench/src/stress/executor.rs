//! The arrival-rate scheduler of stress runs: the open model of
//! `constant-arrival-rate` (design D1), with a target rate the controller
//! changes while it runs. Each scheduled instant goes to a free VU; each VU
//! is a task of its own, started on demand up to the pool size (on the
//! worker threads of [`crate::stress::workers`] in a run), so VUs do not
//! share the scheduler's thread. With no VU free and the pool full, the
//! iteration is dropped.
//!
//! The scheduler also measures its own lag (how late each iteration is
//! dispatched), for the client-saturation guard.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::metrics::Metrics;
use crate::stress::window::Recorder;

/// Lag past which the scheduler yields between dispatches.
const CATCH_UP: Duration = Duration::from_millis(1);

/// One iteration for a VU: its scheduled start, and its number in the fuzz
/// phase (`None` before it, or without fuzzing).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Job {
    pub scheduled: std::time::Instant,
    pub fuzz_iteration: Option<u64>,
}

/// The worst scheduling lag since it was last taken.
#[derive(Debug, Default)]
pub(crate) struct LagMeter {
    worst_us: AtomicU64,
}

impl LagMeter {
    fn record(&self, lag: Duration) {
        let us = u64::try_from(lag.as_micros()).unwrap_or(u64::MAX);
        self.worst_us.fetch_max(us, Ordering::Relaxed);
    }

    /// The worst lag since the last call, and reset it.
    pub fn take(&self) -> Duration {
        Duration::from_micros(self.worst_us.swap(0, Ordering::Relaxed))
    }
}

/// Starts a VU task: its index, its job queue and where it reports itself
/// free again. The task ends once its queue is closed.
pub(crate) type SpawnVu<'a> =
    dyn Fn(usize, mpsc::Receiver<Job>, mpsc::UnboundedSender<usize>) + Send + Sync + 'a;

/// The VU pool, grown on demand.
pub(crate) struct Pool<'a> {
    spawn: &'a SpawnVu<'a>,
    max: usize,
    queues: Vec<mpsc::Sender<Job>>,
    free: Vec<usize>,
    done_tx: mpsc::UnboundedSender<usize>,
    done_rx: mpsc::UnboundedReceiver<usize>,
}

impl<'a> Pool<'a> {
    pub fn new(spawn: &'a SpawnVu<'a>, max: usize) -> Self {
        let (done_tx, done_rx) = mpsc::unbounded_channel();
        Self {
            spawn,
            max: max.max(1),
            queues: Vec::new(),
            free: Vec::new(),
            done_tx,
            done_rx,
        }
    }

    /// Hand `job` to a free VU, starting one when none is free; `false`
    /// when the pool is full and busy.
    fn dispatch(&mut self, job: Job) -> bool {
        let index = match self.free.pop() {
            Some(index) => index,
            None if self.queues.len() < self.max => self.grow(),
            None => return false,
        };
        // A free VU's queue is empty, so this never waits.
        self.queues[index].try_send(job).is_ok()
    }

    fn grow(&mut self) -> usize {
        let index = self.queues.len();
        let (tx, rx) = mpsc::channel(1);
        self.queues.push(tx);
        (self.spawn)(index, rx, self.done_tx.clone());
        index
    }

    /// How many VUs were started.
    #[cfg(test)]
    pub fn size(&self) -> usize {
        self.queues.len()
    }

    /// Stop feeding the VUs: each ends after its iteration in flight.
    pub fn close(self) {}
}

/// What the scheduler shares with the run.
pub(crate) struct Scheduler<'a> {
    pub metrics: &'a Metrics,
    pub recorder: &'a Recorder,
    pub lag: &'a LagMeter,
    /// Pauses the scheduler after each dispatch (tests: a client that
    /// cannot keep up).
    pub dispatch_delay: Duration,
    /// Set once the fuzz phase started: iterations are numbered from then,
    /// in dispatch order, so fuzzed slots follow the seed.
    pub fuzzing: Option<&'a AtomicBool>,
}

impl Scheduler<'_> {
    /// Schedule iterations at the rate of `rate` (per second; 0 pauses)
    /// until `stop`.
    pub async fn run(
        &self,
        pool: &mut Pool<'_>,
        mut rate: watch::Receiver<f64>,
        stop: &CancellationToken,
    ) {
        let mut next: Option<Instant> = None;
        let mut fuzz_iterations = 0u64;
        loop {
            let per_second = *rate.borrow_and_update();
            next = match (per_second > 0.0, next) {
                (false, _) => None,
                (true, None) => Some(Instant::now()),
                (true, Some(at)) => Some(at),
            };
            tokio::select! {
                biased;
                () = stop.cancelled() => break,
                Some(index) = pool.done_rx.recv() => pool.free.push(index),
                changed = rate.changed() => {
                    if changed.is_err() {
                        break;
                    }
                }
                () = sleep_until(next) => {
                    let Some(at) = next else { continue };
                    self.dispatch(pool, at, &mut fuzz_iterations).await;
                    next = Some(at + Duration::from_secs_f64(1.0 / per_second));
                }
            }
        }
    }

    async fn dispatch(&self, pool: &mut Pool<'_>, at: Instant, fuzz_iterations: &mut u64) {
        let lag = Instant::now().saturating_duration_since(at);
        self.lag.record(lag);
        self.metrics.scheduled();
        self.recorder.scheduled();
        let fuzz_iteration = self
            .fuzzing
            .is_some_and(|on| on.load(Ordering::Relaxed))
            .then(|| {
                *fuzz_iterations += 1;
                *fuzz_iterations - 1
            });
        let job = Job {
            scheduled: at.into_std(),
            fuzz_iteration,
        };
        if !pool.dispatch(job) {
            self.metrics.dropped();
        }
        if !self.dispatch_delay.is_zero() {
            tokio::time::sleep(self.dispatch_delay).await;
        } else if lag > CATCH_UP {
            // Behind schedule, iterations are due at once: let the run's
            // controller (on the same task) keep its clock meanwhile.
            tokio::task::yield_now().await;
        }
    }
}

async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    use rustman_report::bench::LatencyKind;

    use super::*;

    type Tasks = Arc<std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>>;

    /// VUs that count their jobs and take `busy` each.
    fn counting(count: Arc<AtomicUsize>, busy: Duration, tasks: Tasks) -> Box<SpawnVu<'static>> {
        Box::new(
            move |index, mut jobs: mpsc::Receiver<Job>, done: mpsc::UnboundedSender<usize>| {
                let count = count.clone();
                let task = tokio::spawn(async move {
                    while jobs.recv().await.is_some() {
                        count.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(busy).await;
                        if done.send(index).is_err() {
                            break;
                        }
                    }
                });
                tasks.lock().unwrap().push(task);
            },
        )
    }

    async fn schedule(
        rate: f64,
        pool_size: usize,
        busy: Duration,
        for_: Duration,
    ) -> (usize, Metrics, usize) {
        let count = Arc::new(AtomicUsize::new(0));
        let tasks = Tasks::default();
        let spawn = counting(count.clone(), busy, tasks.clone());
        let mut pool = Pool::new(&*spawn, pool_size);
        let metrics = Metrics::new(LatencyKind::Corrected, vec!["a".into()]);
        let recorder = Recorder::new(1, false);
        let lag = LagMeter::default();
        let scheduler = Scheduler {
            metrics: &metrics,
            recorder: &recorder,
            lag: &lag,
            dispatch_delay: Duration::ZERO,
            fuzzing: None,
        };
        let (tx, rx) = watch::channel(rate);
        let stop = CancellationToken::new();
        let stopper = async {
            tokio::time::sleep(for_).await;
            stop.cancel();
        };
        tokio::join!(scheduler.run(&mut pool, rx, &stop), stopper);
        drop(tx);
        let spawned = pool.size();
        pool.close();
        let handles = std::mem::take(&mut *tasks.lock().unwrap());
        for task in handles {
            task.await.unwrap();
        }
        (count.load(Ordering::SeqCst), metrics, spawned)
    }

    #[tokio::test(start_paused = true)]
    async fn the_rate_is_kept_with_vus_spawned_on_demand() {
        let (count, metrics, spawned) = schedule(
            50.0,
            100,
            Duration::from_millis(30),
            Duration::from_millis(1990),
        )
        .await;
        assert_eq!(count, 100);
        assert_eq!(metrics.summary().scheduled, 100);
        assert_eq!(metrics.summary().dropped, 0);
        // 30 ms per iteration at 20 ms intervals: two VUs are enough.
        assert_eq!(spawned, 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_full_pool_drops_iterations() {
        let (count, metrics, spawned) =
            schedule(10.0, 2, Duration::from_secs(10), Duration::from_millis(990)).await;
        assert_eq!(spawned, 2);
        assert_eq!(count, 2);
        let summary = metrics.summary();
        assert_eq!(summary.scheduled, 10);
        assert_eq!(summary.dropped, 8);
    }

    #[test]
    fn the_lag_meter_keeps_the_worst_and_resets() {
        let lag = LagMeter::default();
        lag.record(Duration::from_millis(3));
        lag.record(Duration::from_millis(120));
        lag.record(Duration::from_millis(7));
        assert_eq!(lag.take(), Duration::from_millis(120));
        assert_eq!(lag.take(), Duration::ZERO);
    }
}
