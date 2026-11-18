//! Background jobs of a session (design D8): `explore_run` and
//! `run_collection` register a job with a cancellation token, return its id
//! at once and keep working on a thread of their own. At most
//! `[agent].max_jobs` run at once across every session of the server (the
//! [`JobPool`]); more get `busy`. A job holds its slot until its thread has
//! returned, even when a cancellation stopped waiting for it. While a job runs, a
//! call that carried a progress token gets `notifications/progress` every
//! [`PROGRESS_INTERVAL`], and an MCP cancellation of that call stops the
//! job like `explore_cancel` does.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use rmcp::model::{ProgressNotificationParam, ProgressToken, RequestId};
use rmcp::service::{Peer, RoleServer};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::context::AgentContext;
use super::error::{ErrorCode, ToolError};
use super::redact::OutputRedactor;

/// How often a running job reports its progress: well within the 5 s the
/// spec allows between two notifications.
pub const PROGRESS_INTERVAL: Duration = Duration::from_secs(2);

/// How long a cancellation waits for a job to wind down.
pub const CANCEL_WAIT: Duration = Duration::from_secs(30);

/// How a job is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Running,
    Completed,
    Cancelled,
    Failed,
}

/// A runtime of its own for work whose futures are not `Send`.
fn own_runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
}

/// Run the future `work` makes on a blocking thread with its own runtime,
/// and wait for its output.
pub async fn on_own_thread<F, Fut, T>(work: F) -> Result<T, ToolError>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = T>,
    T: Send + 'static,
{
    let joined = tokio::task::spawn_blocking(move || own_runtime().map(|rt| rt.block_on(work())))
        .await
        .map_err(|e| ToolError::internal(e.to_string()))?;
    joined.map_err(|e| ToolError::internal(format!("cannot start a runtime: {e}")))
}

/// What a job runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Exploration,
    Run,
}

impl JobKind {
    /// What the job counts: probes or requests.
    fn unit(self) -> &'static str {
        match self {
            Self::Exploration => "probes",
            Self::Run => "requests",
        }
    }
}

/// How far a job got.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JobProgress {
    pub done: usize,
    pub total: usize,
}

impl JobState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug)]
struct Outcome {
    state: JobState,
    /// The reason of a failed job.
    error: Option<String>,
    /// The canonical report of a finished run.
    report: Option<Value>,
}

/// One background job.
#[derive(Debug)]
pub struct Job {
    pub id: String,
    pub kind: JobKind,
    /// The exploration a job of kind `exploration` drives.
    pub exploration: Option<String>,
    cancel: CancellationToken,
    /// Cancelled once the job is no longer running.
    ended: CancellationToken,
    /// The `tools/call` request that started the job.
    request: Mutex<Option<RequestId>>,
    progress: Mutex<JobProgress>,
    outcome: Mutex<Outcome>,
    task: Mutex<Option<JoinHandle<()>>>,
    /// Whether the job's thread is still working.
    alive: AtomicBool,
}

impl Job {
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    pub fn state(&self) -> JobState {
        self.lock().state
    }

    pub fn error(&self) -> Option<String> {
        self.lock().error.clone()
    }

    pub fn report(&self) -> Option<Value> {
        self.lock().report.clone()
    }

    pub fn progress(&self) -> JobProgress {
        *self.progress.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn set_progress(&self, done: usize, total: usize) {
        *self.progress.lock().unwrap_or_else(PoisonError::into_inner) = JobProgress { done, total };
    }

    /// Count one more unit of work done.
    pub fn advance(&self) {
        self.progress
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .done += 1;
    }

    /// Remember the `tools/call` request that started the job, so an MCP
    /// cancellation of it reaches the job.
    pub fn bind_request(&self, id: RequestId) {
        *self.request.lock().unwrap_or_else(PoisonError::into_inner) = Some(id);
    }

    fn started_by(&self, id: &RequestId) -> bool {
        self.request
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            == Some(id)
    }

    /// One line on how the job is doing, such as `job-1 running: 3/40 probes`.
    pub fn describe(&self) -> String {
        format!("{} {}: {}", self.id, self.state().as_str(), self.counted())
    }

    /// How much is done, such as `3/40 probes`.
    pub fn counted(&self) -> String {
        let JobProgress { done, total } = self.progress();
        format!("{done}/{total} {}", self.kind.unit())
    }

    /// Record how the job ended.
    pub fn finish(&self, state: JobState, error: Option<String>, report: Option<Value>) {
        *self.lock() = Outcome {
            state,
            error,
            report,
        };
        if state != JobState::Running {
            self.ended.cancel();
        }
    }

    /// Ask the job to stop, and wait up to `wait` for it to wind down.
    pub async fn cancel(&self, wait: Duration) {
        self.cancel.cancel();
        let task = self
            .task
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(task) = task {
            let _ = tokio::time::timeout(wait, task).await;
        }
        let mut outcome = self.lock();
        if outcome.state == JobState::Running {
            outcome.state = JobState::Cancelled;
        }
        self.ended.cancel();
    }

    /// Whether the job holds one of the `max_jobs` slots: it is running, or
    /// its thread has not returned yet.
    pub fn occupies_slot(&self) -> bool {
        self.state() == JobState::Running || self.alive.load(Ordering::SeqCst)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Outcome> {
        self.outcome.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Clears [`Job::alive`] when the job's thread returns, panics included.
struct Alive(Arc<Job>);

impl Drop for Alive {
    fn drop(&mut self) {
        self.0.alive.store(false, Ordering::SeqCst);
    }
}

/// The jobs of every session of one server, for `max_jobs`.
#[derive(Debug, Default)]
pub struct JobPool {
    jobs: Mutex<Vec<Weak<Job>>>,
}

impl JobPool {
    /// Add `job` unless `max` jobs hold a slot already; `Err` carries how
    /// many do.
    fn admit(&self, max: usize, job: impl FnOnce() -> Arc<Job>) -> Result<Arc<Job>, usize> {
        let mut jobs = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        jobs.retain(|j| j.upgrade().is_some_and(|j| j.occupies_slot()));
        if jobs.len() >= max {
            return Err(jobs.len());
        }
        let job = job();
        jobs.push(Arc::downgrade(&job));
        Ok(job)
    }
}

/// The jobs of one session, admitted by the server's [`JobPool`].
#[derive(Debug, Default)]
pub struct Jobs {
    next: AtomicU64,
    jobs: Mutex<BTreeMap<String, Arc<Job>>>,
    pool: Arc<JobPool>,
}

impl Jobs {
    /// The jobs of a session of the server that owns `pool`.
    pub fn in_pool(pool: Arc<JobPool>) -> Self {
        Self {
            pool,
            ..Self::default()
        }
    }

    /// Register a new running job, unless `max` jobs of the server hold a
    /// slot already.
    pub fn start(
        &self,
        kind: JobKind,
        exploration: Option<String>,
        max: usize,
    ) -> Result<Arc<Job>, ToolError> {
        let mut jobs = self.lock();
        let number = self.next.load(Ordering::SeqCst) + 1;
        let id = format!("job-{number}");
        let job = self
            .pool
            .admit(max, || Arc::new(new_job(id.clone(), kind, exploration)))
            .map_err(|running| {
                ToolError::new(
                    ErrorCode::Busy,
                    format!(
                        "{running} background jobs are running (max_jobs = {max}); wait for one to finish or cancel it"
                    ),
                )
            })?;
        self.next.store(number, Ordering::SeqCst);
        jobs.insert(id, job.clone());
        Ok(job)
    }

    /// Run the future `work` makes for `job` on a thread of its own, with
    /// its own runtime: engine futures need not be `Send`, and a long job
    /// never holds up the session.
    pub fn spawn<F, Fut>(&self, job: &Arc<Job>, work: F)
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = ()>,
    {
        job.alive.store(true, Ordering::SeqCst);
        let alive = Alive(job.clone());
        let handle = tokio::task::spawn_blocking(move || match own_runtime() {
            Ok(runtime) => {
                runtime.block_on(work());
                drop(alive);
            }
            Err(err) => alive
                .0
                .finish(JobState::Failed, Some(err.to_string()), None),
        });
        *job.task.lock().unwrap_or_else(PoisonError::into_inner) = Some(handle);
    }

    pub fn get(&self, id: &str) -> Option<Arc<Job>> {
        self.lock().get(id).cloned()
    }

    /// The job driving exploration `id`, if this session started it.
    pub fn for_exploration(&self, id: &str) -> Option<Arc<Job>> {
        self.lock()
            .values()
            .find(|j| j.exploration.as_deref() == Some(id))
            .cloned()
    }

    /// The job the `tools/call` request `id` started.
    pub fn for_request(&self, id: &RequestId) -> Option<Arc<Job>> {
        self.lock().values().find(|j| j.started_by(id)).cloned()
    }

    /// Every job, in start order.
    pub fn all(&self) -> Vec<Arc<Job>> {
        let mut jobs: Vec<Arc<Job>> = self.lock().values().cloned().collect();
        jobs.sort_by_key(|j| j.id[4..].parse::<u64>().unwrap_or(0));
        jobs
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Arc<Job>>> {
        self.jobs.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn new_job(id: String, kind: JobKind, exploration: Option<String>) -> Job {
    Job {
        id,
        kind,
        exploration,
        cancel: CancellationToken::new(),
        ended: CancellationToken::new(),
        request: Mutex::new(None),
        progress: Mutex::new(JobProgress::default()),
        outcome: Mutex::new(Outcome {
            state: JobState::Running,
            error: None,
            report: None,
        }),
        task: Mutex::new(None),
        alive: AtomicBool::new(false),
    }
}

/// Send `job`'s progress to the client every [`PROGRESS_INTERVAL`] while
/// it runs, and once more when it ends. Messages pass the session's
/// redaction like every other output.
pub fn report_progress(
    ctx: Arc<AgentContext>,
    job: Arc<Job>,
    peer: Peer<RoleServer>,
    token: ProgressToken,
) {
    tokio::spawn(async move {
        let ended = job.ended.clone();
        loop {
            let last = tokio::select! {
                () = ended.cancelled() => true,
                () = tokio::time::sleep(PROGRESS_INTERVAL) => false,
            };
            let notification = progress_notification(&ctx.redactor(), &job, &token);
            if peer.notify_progress(notification).await.is_err() || last {
                break;
            }
        }
    });
}

fn progress_notification(
    redactor: &OutputRedactor,
    job: &Job,
    token: &ProgressToken,
) -> ProgressNotificationParam {
    let JobProgress { done, total } = job.progress();
    let mut notification = ProgressNotificationParam::new(token.clone(), done as f64);
    notification.total = (total > 0).then_some(total as f64);
    notification.message = Some(redactor.text(&job.describe()));
    notification
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jobs_beyond_the_limit_are_busy() {
        let jobs = Jobs::default();
        let first = jobs.start(JobKind::Run, None, 2).unwrap();
        jobs.start(JobKind::Run, None, 2).unwrap();
        let err = jobs.start(JobKind::Run, None, 2).unwrap_err();
        assert_eq!(err.code, ErrorCode::Busy);
        first.finish(JobState::Completed, None, None);
        let third = jobs
            .start(JobKind::Exploration, Some("exp-1".into()), 2)
            .unwrap();
        assert_eq!(third.id, "job-3");
        assert_eq!(jobs.for_exploration("exp-1").unwrap().id, "job-3");
        let ids: Vec<String> = jobs.all().iter().map(|j| j.id.clone()).collect();
        assert_eq!(ids, ["job-1", "job-2", "job-3"]);
    }

    #[test]
    fn max_jobs_counts_every_session_of_the_server() {
        let pool = Arc::new(JobPool::default());
        let (one, two) = (Jobs::in_pool(pool.clone()), Jobs::in_pool(pool));
        let first = one.start(JobKind::Run, None, 2).unwrap();
        two.start(JobKind::Run, None, 2).unwrap();
        let err = two.start(JobKind::Run, None, 2).unwrap_err();
        assert_eq!(err.code, ErrorCode::Busy);
        assert!(one.start(JobKind::Run, None, 2).is_err());
        first.finish(JobState::Completed, None, None);
        assert_eq!(two.start(JobKind::Run, None, 2).unwrap().id, "job-2");
        // A refused start uses no id.
        assert!(one.start(JobKind::Run, None, 2).is_err());
        assert_eq!(one.all().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_job_still_working_after_cancel_keeps_its_slot() {
        let jobs = Jobs::default();
        let job = jobs.start(JobKind::Run, None, 1).unwrap();
        let (release, released) = std::sync::mpsc::channel::<()>();
        // The work ignores cancellation until released.
        jobs.spawn(&job, move || async move {
            let _ = released.recv();
        });
        job.cancel(Duration::from_millis(50)).await;
        assert_eq!(job.state(), JobState::Cancelled);
        assert!(job.occupies_slot());
        let err = jobs.start(JobKind::Run, None, 1).unwrap_err();
        assert_eq!(err.code, ErrorCode::Busy);
        release.send(()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while job.occupies_slot() && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!job.occupies_slot());
        assert!(jobs.start(JobKind::Run, None, 1).is_ok());
    }

    #[tokio::test]
    async fn cancelling_a_job_stops_its_task() {
        let jobs = Jobs::default();
        let job = jobs.start(JobKind::Run, None, 2).unwrap();
        let token = job.cancel_token();
        jobs.spawn(&job, move || async move { token.cancelled().await });
        job.cancel(Duration::from_secs(10)).await;
        assert_eq!(job.state(), JobState::Cancelled);
        assert!(job.ended.is_cancelled());
    }

    #[test]
    fn progress_notifications_carry_counts_and_pass_redaction() {
        let jobs = Jobs::default();
        let job = jobs.start(JobKind::Exploration, None, 2).unwrap();
        job.set_progress(4, 10);
        let token = ProgressToken(rmcp::model::NumberOrString::Number(1));
        let plain = progress_notification(&OutputRedactor::new(), &job, &token);
        assert_eq!(plain.progress, 4.0);
        assert_eq!(plain.total, Some(10.0));
        assert_eq!(plain.message.as_deref(), Some("job-1 running: 4/10 probes"));
        // Canary: a secret whose value shows up in the message is masked.
        let mut redactor = OutputRedactor::new();
        let secret = rustman_core::Secret::new("canary", "4/10 probes".to_owned());
        redactor.add_secret(&secret);
        let masked = progress_notification(&redactor, &job, &token);
        let message = masked.message.unwrap();
        assert!(!message.contains("4/10 probes"), "{message}");
        assert!(message.contains("«redacted:canary»"), "{message}");
    }

    #[test]
    fn jobs_track_progress_and_the_request_that_started_them() {
        let jobs = Jobs::default();
        let job = jobs.start(JobKind::Run, None, 2).unwrap();
        job.set_progress(0, 3);
        job.advance();
        assert_eq!(job.progress(), JobProgress { done: 1, total: 3 });
        assert_eq!(job.describe(), "job-1 running: 1/3 requests");
        let id = RequestId::Number(7);
        assert!(jobs.for_request(&id).is_none());
        job.bind_request(id.clone());
        assert_eq!(jobs.for_request(&id).unwrap().id, "job-1");
        assert!(jobs.for_request(&RequestId::Number(8)).is_none());
        assert!(!job.ended.is_cancelled());
        job.finish(JobState::Completed, None, None);
        assert!(job.ended.is_cancelled());
        assert_eq!(job.describe(), "job-1 completed: 1/3 requests");
    }
}
