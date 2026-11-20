//! What the preview serves: the folder of the last successful build and
//! the error of the last build, replaced whole after every build.

use std::path::Path;
use std::sync::{Arc, PoisonError, RwLock};

use tempfile::TempDir;
use tokio::sync::{Mutex, watch};

use crate::build::BuildReport;

/// Builds the site into an empty folder.
pub trait SiteBuilder: Send + Sync {
    fn build_into(&self, out: &Path) -> BuildOutcome;
}

/// The result of one preview build. The error is a message that already
/// passed the redaction step; it is shown on stderr and in the page overlay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildOutcome {
    pub result: Result<BuildReport, String>,
    /// Problems that did not stop the build.
    pub warnings: Vec<String>,
}

impl BuildOutcome {
    /// A build that failed with `message`.
    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            result: Err(message.into()),
            warnings: Vec::new(),
        }
    }
}

/// Hears about every build (the CLI prints it).
pub type Reporter = Box<dyn Fn(&BuildOutcome) + Send + Sync>;

/// One served state.
pub(crate) struct Snapshot {
    /// The last successful build; kept when a later build fails.
    pub site: Option<Arc<TempDir>>,
    /// The error of the last build, if it failed.
    pub error: Option<String>,
    /// How many builds finished; pages reload when it changes.
    pub generation: u64,
}

pub(crate) struct Shared {
    current: RwLock<Arc<Snapshot>>,
    generations: watch::Sender<u64>,
    builder: Arc<dyn SiteBuilder>,
    reporter: Reporter,
    /// One build at a time.
    building: Mutex<()>,
}

impl Shared {
    pub fn new(builder: Arc<dyn SiteBuilder>, reporter: Reporter) -> Self {
        let empty = Snapshot {
            site: None,
            error: None,
            generation: 0,
        };
        Self {
            current: RwLock::new(Arc::new(empty)),
            generations: watch::Sender::new(0),
            builder,
            reporter,
            building: Mutex::new(()),
        }
    }

    pub fn snapshot(&self) -> Arc<Snapshot> {
        Arc::clone(&self.current.read().unwrap_or_else(PoisonError::into_inner))
    }

    /// Notified with the generation after every build.
    pub fn generations(&self) -> watch::Receiver<u64> {
        self.generations.subscribe()
    }

    pub async fn rebuild(&self) {
        let _building = self.building.lock().await;
        let builder = Arc::clone(&self.builder);
        let (outcome, dir) = tokio::task::spawn_blocking(move || build_once(builder.as_ref()))
            .await
            .unwrap_or_else(|_| (BuildOutcome::failed("docs: the build panicked"), None));
        (self.reporter)(&outcome);
        self.install(outcome.result.err(), dir);
    }

    /// Serve `dir` (or keep the previous site when the build failed).
    fn install(&self, error: Option<String>, dir: Option<TempDir>) {
        let mut current = self.current.write().unwrap_or_else(PoisonError::into_inner);
        let generation = current.generation + 1;
        let site = dir.map(Arc::new).or_else(|| current.site.clone());
        *current = Arc::new(Snapshot {
            site,
            error,
            generation,
        });
        drop(current);
        self.generations.send_replace(generation);
    }
}

/// Build into a new temporary folder, kept only when the build succeeded.
fn build_once(builder: &dyn SiteBuilder) -> (BuildOutcome, Option<TempDir>) {
    let dir = match tempfile::Builder::new().prefix("rustman-docs-").tempdir() {
        Ok(dir) => dir,
        Err(e) => {
            let message = format!("docs: cannot create a temporary folder: {e}");
            return (BuildOutcome::failed(message), None);
        }
    };
    let outcome = builder.build_into(dir.path());
    let dir = outcome.result.is_ok().then_some(dir);
    (outcome, dir)
}
