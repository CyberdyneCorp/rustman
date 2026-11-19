//! Opening the current exchange's trace link (`trace.open`, `Ctrl+O`;
//! trace-propagation spec "Showing trace ids").

use std::path::PathBuf;

use rustman_core::SecretBackend;
use rustman_core::environment::Environment;
use rustman_core::model::Variables;
use rustman_core::settings::Settings;
use rustman_core::trace::{TraceRecord, WorkspaceScope};

use crate::app::{Action, Effect, State};
use crate::toast::Severity;

/// What the redactor puts in place of a secret.
const REDACTED_PREFIX: &str = "«redacted:";

/// How a URL is opened: the system browser, or a stand-in in tests.
pub type Opener = fn(&str) -> std::io::Result<()>;

/// A link to open: the recorded one, shown in toasts, and, when it hides a
/// secret (a `{{variable}}` of the template), how to build the real one.
#[derive(Clone, Debug)]
pub struct LinkJob {
    pub shown: String,
    pub rebuild: Option<Box<Rebuild>>,
}

/// What rebuilds a link from the template configured now, as `rustman
/// trace open` does: the workspace, the environment and its secrets.
#[derive(Clone, Debug)]
pub struct Rebuild {
    pub root: PathBuf,
    pub backend: Option<SecretBackend>,
    pub settings: Settings,
    pub environment: Option<Environment>,
    pub trace: TraceRecord,
}

impl PartialEq for LinkJob {
    fn eq(&self, other: &Self) -> bool {
        self.shown == other.shown && self.rebuild.is_some() == other.rebuild.is_some()
    }
}

impl Eq for LinkJob {}

impl LinkJob {
    /// The recorded link, opened as is.
    pub fn plain(link: impl Into<String>) -> Self {
        Self {
            shown: link.into(),
            rebuild: None,
        }
    }

    /// The URL to hand to the browser.
    fn url(&self) -> Result<String, String> {
        match &self.rebuild {
            None => Ok(self.shown.clone()),
            Some(rebuild) => rebuild.url(),
        }
    }
}

impl Rebuild {
    fn url(&self) -> Result<String, String> {
        let secrets = crate::send::secret_store(&self.root, self.backend)?;
        let overrides = Variables::default();
        let scope = WorkspaceScope {
            settings: &self.settings,
            environment: self.environment.as_ref(),
            secrets: &secrets,
            overrides: &overrides,
        };
        let (link, _) = scope
            .link()?
            .ok_or("no trace link is configured any more")?;
        Ok(self.trace.render(&link))
    }
}

/// The effect that opens the shown exchange's trace link, or a toast
/// saying why there is none.
pub fn open(state: &mut State) -> Vec<Effect> {
    let trace = state
        .tabs
        .active()
        .filter(|t| t.in_flight.is_none())
        .and_then(|t| t.exchange.as_deref())
        .and_then(|r| r.record.trace.as_ref());
    let message = match trace {
        Some(trace) => match &trace.link {
            Some(link) => {
                let rebuild = link
                    .contains(REDACTED_PREFIX)
                    .then(|| rebuild(state, trace.clone()))
                    .flatten();
                let job = LinkJob {
                    shown: link.clone(),
                    rebuild,
                };
                return vec![Effect::OpenUrl(job)];
            }
            None => "No trace link: set trace.link in the environment or rustman.toml",
        },
        None => "This exchange carries no trace context",
    };
    state.notify(Severity::Info, message.to_owned());
    Vec::new()
}

/// How to rebuild the shown exchange's link in the open workspace.
fn rebuild(state: &State, trace: TraceRecord) -> Option<Box<Rebuild>> {
    let ws = crate::workbench::workspace(state)?;
    let environment = state
        .status
        .environment
        .as_deref()
        .and_then(|name| ws.environments.iter().find(|e| e.name == name))
        .cloned();
    Some(Box::new(Rebuild {
        root: ws.root.clone(),
        backend: state.secret_backend,
        settings: ws.settings.clone(),
        environment,
        trace,
    }))
}

/// Run [`Effect::OpenUrl`] with `opener`. Toasts show the recorded
/// (redacted) link; the browser gets the real one.
pub fn execute(job: LinkJob, opener: Opener) -> Action {
    let result = job
        .url()
        .and_then(|url| opener(&url).map_err(|e| e.to_string()));
    Action::UrlOpened {
        url: job.shown,
        result,
    }
}

/// The link was handed to the browser, or could not be.
pub fn on_opened(state: &mut State, url: &str, result: Result<(), String>) {
    match result {
        Ok(()) => state.notify(Severity::Info, format!("Opened {url}")),
        Err(e) => state.notify(
            Severity::Error,
            format!("Cannot open a browser ({e}): {url}"),
        ),
    }
}
