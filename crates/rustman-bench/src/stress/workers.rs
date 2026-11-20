//! The worker threads VU tasks run on. A request's future in the runner is
//! not `Send` (its variable sources and the executors of other request
//! kinds are boxed without it), so a VU cannot be `tokio::spawn`ed onto a
//! multi-thread runtime. Instead, each worker thread runs a current-thread
//! runtime with a `LocalSet`, and each VU is a task of its own there
//! (`spawn_local`), its I/O driven by that worker's runtime. VUs are dealt
//! to the workers in turn, so load spreads over the cores whatever thread
//! the controller runs on.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::stress::executor::Job;

/// A VU's task, built on its worker thread.
pub(crate) type VuFuture = Pin<Box<dyn Future<Output = ()>>>;

/// Builds the task of VU `index`, fed by `jobs`, reporting itself free on
/// `done`.
pub(crate) type MakeVu =
    dyn Fn(usize, mpsc::Receiver<Job>, mpsc::UnboundedSender<usize>) -> VuFuture + Send + Sync;

type Command = (usize, mpsc::Receiver<Job>, mpsc::UnboundedSender<usize>);

/// The worker threads of one run.
pub(crate) struct Workers {
    commands: Vec<mpsc::UnboundedSender<Command>>,
    finished: Vec<oneshot::Receiver<()>>,
    threads: Vec<std::thread::JoinHandle<()>>,
    next: AtomicUsize,
}

/// How many worker threads a run starts: one per core, 2 to 16.
pub(crate) fn worker_count() -> usize {
    std::thread::available_parallelism()
        .map_or(2, std::num::NonZeroUsize::get)
        .clamp(2, 16)
}

impl Workers {
    /// Start `count` worker threads building VUs with `make`.
    pub fn start(count: usize, make: Arc<MakeVu>) -> std::io::Result<Self> {
        let mut workers = Self {
            commands: Vec::new(),
            finished: Vec::new(),
            threads: Vec::new(),
            next: AtomicUsize::new(0),
        };
        for n in 0..count.max(1) {
            let (tx, rx) = mpsc::unbounded_channel();
            let (done_tx, done_rx) = oneshot::channel();
            let make = make.clone();
            let thread = std::thread::Builder::new()
                .name(format!("rustman-stress-{n}"))
                .spawn(move || worker(rx, &*make, done_tx))?;
            workers.commands.push(tx);
            workers.finished.push(done_rx);
            workers.threads.push(thread);
        }
        Ok(workers)
    }

    /// Start VU `index` on the next worker.
    pub fn spawn(
        &self,
        index: usize,
        jobs: mpsc::Receiver<Job>,
        done: mpsc::UnboundedSender<usize>,
    ) {
        let n = self.next.fetch_add(1, Ordering::Relaxed) % self.commands.len();
        // A worker only stops once its command queue is closed.
        let _ = self.commands[n].send((index, jobs, done));
    }

    /// Wait for every VU to end (their job queues must be closed), giving
    /// iterations in flight `drain`, then cancelling them with `hard`.
    pub async fn finish(self, drain: Duration, hard: &CancellationToken) {
        drop(self.commands);
        let all = futures_util::future::join_all(self.finished);
        tokio::pin!(all);
        if tokio::time::timeout(drain, &mut all).await.is_err() {
            hard.cancel();
            all.await;
        }
        // Every worker signalled its end: the threads are exiting.
        for thread in self.threads {
            let _ = thread.join();
        }
    }
}

fn worker(
    mut commands: mpsc::UnboundedReceiver<Command>,
    make: &MakeVu,
    done: oneshot::Sender<()>,
) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    if let Ok(runtime) = runtime {
        let local = tokio::task::LocalSet::new();
        local.block_on(&runtime, async {
            let mut tasks = Vec::new();
            while let Some((index, jobs, free)) = commands.recv().await {
                tasks.push(tokio::task::spawn_local(make(index, jobs, free)));
            }
            for task in tasks {
                let _ = task.await;
            }
        });
    }
    let _ = done.send(());
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[tokio::test]
    async fn vus_run_on_several_worker_threads() {
        let seen: Arc<Mutex<Vec<(usize, String)>>> = Arc::default();
        let record = seen.clone();
        let make: Arc<MakeVu> = Arc::new(
            move |index, mut jobs: mpsc::Receiver<Job>, free: mpsc::UnboundedSender<usize>| {
                let record = record.clone();
                Box::pin(async move {
                    while jobs.recv().await.is_some() {
                        let thread = std::thread::current().name().unwrap_or_default().to_owned();
                        record.lock().unwrap().push((index, thread));
                        let _ = free.send(index);
                    }
                })
            },
        );
        let workers = Workers::start(2, make).unwrap();
        let (free_tx, _free_rx) = mpsc::unbounded_channel();
        let mut queues = Vec::new();
        for index in 0..4 {
            let (tx, rx) = mpsc::channel(1);
            workers.spawn(index, rx, free_tx.clone());
            queues.push(tx);
        }
        for queue in &queues {
            queue
                .send(Job {
                    scheduled: std::time::Instant::now(),
                    fuzz_iteration: None,
                })
                .await
                .unwrap();
        }
        drop(queues);
        workers
            .finish(Duration::from_secs(5), &CancellationToken::new())
            .await;
        let mut seen = seen.lock().unwrap().clone();
        seen.sort();
        let threads: Vec<&str> = seen.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(
            threads,
            [
                "rustman-stress-0",
                "rustman-stress-1",
                "rustman-stress-0",
                "rustman-stress-1"
            ]
        );
    }

    #[tokio::test]
    async fn finishing_cancels_vus_that_outlast_the_drain() {
        let make: Arc<MakeVu> = Arc::new(|_, _jobs, _free| {
            Box::pin(async {
                // Never ends on its own: the queue is held open.
                std::future::pending::<()>().await;
            })
        });
        let hard = CancellationToken::new();
        let stuck = hard.clone();
        let make: Arc<MakeVu> = Arc::new(move |i, jobs, free| {
            let stuck = stuck.clone();
            let inner = make(i, jobs, free);
            Box::pin(async move {
                tokio::select! {
                    () = inner => {}
                    () = stuck.cancelled() => {}
                }
            })
        });
        let workers = Workers::start(1, make).unwrap();
        let (tx, rx) = mpsc::channel(1);
        let (free, _) = mpsc::unbounded_channel();
        workers.spawn(0, rx, free);
        workers.finish(Duration::from_millis(50), &hard).await;
        assert!(hard.is_cancelled());
        drop(tx);
    }
}
