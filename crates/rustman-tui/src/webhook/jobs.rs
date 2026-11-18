//! The pane's background work: reading the listener settings, running the
//! listener on its own thread, the mock file, and save, replay and history
//! jobs. Everything here runs off the render loop.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustman_core::history::HistoryStore;
use rustman_core::settings::HistorySettings;
use rustman_core::{ConfigError, Redactor, SecretBackend};
use rustman_http::{HttpEngine, SendOptions};
use rustman_webhook::mocks::MOCKS_FILE;
use rustman_webhook::{
    Capture, CaptureStore, Listener, ListenerConfig, Mocks, Verifiers, WebhookSettings, watch,
};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use super::{EventNote, Msg, Running};
use crate::app::{Action, Effect};

/// The text a new `webhook/mocks.yaml` starts with.
pub const MOCKS_TEMPLATE: &str = "\
# Mock responses for the webhook listener; the first match answers.
mocks: []
# - method: POST            # or \"*\" (the default)
#   path: /hooks/github     # exact, or a glob with * and **
#   status: 500
#   headers: { X-Mock: \"1\" }
#   body: '{\"error\":\"boom\"}'
#   delay_ms: 2000          # at most 60000
";

/// What the listener runs with, read from the workspace.
#[derive(Clone, Debug)]
pub struct Prepared {
    pub config: ListenerConfig,
    pub mocks: Mocks,
    pub verifiers: Verifiers,
    /// The workspace whose `webhook/` folder is watched for mock changes.
    pub root: Option<PathBuf>,
}

/// Start the listener of `prepared`; cancelling `stop` stops it.
#[derive(Clone, Debug)]
pub struct StartJob {
    pub prepared: Arc<Prepared>,
    pub stop: CancellationToken,
}

/// Background work of the pane.
#[derive(Clone, Debug)]
pub enum Job {
    /// Read `[webhook]`, the mocks and the verifier secrets.
    Prepare {
        root: Option<PathBuf>,
        environment: Option<String>,
        backend: Option<SecretBackend>,
    },
    /// Run the listener (on its own thread, see [`intercept`]).
    Start(StartJob),
    ReadMocks(PathBuf),
    /// Validate `text` as mocks and write it to `path`.
    WriteMocks {
        path: PathBuf,
        text: String,
    },
    Replay {
        capture: Arc<Capture>,
        url: String,
        redactor: Redactor,
    },
    SaveFile {
        capture: Arc<Capture>,
        url: String,
        path: PathBuf,
        redactor: Redactor,
    },
    SaveHistory {
        capture: Arc<Capture>,
        root: PathBuf,
        settings: HistorySettings,
        redactor: Redactor,
    },
}

/// Jobs compare by their description (effects are compared in tests).
impl PartialEq for Job {
    fn eq(&self, other: &Self) -> bool {
        format!("{self:?}") == format!("{other:?}")
    }
}

impl Eq for Job {}

/// Run `job` to completion (blocking).
pub fn execute(job: Job) -> Action {
    let msg = match job {
        Job::Prepare {
            root,
            environment,
            backend,
        } => Msg::Prepared(prepare(root, environment.as_deref(), backend).map(Arc::new)),
        Job::Start(_) => Msg::Started(Err("the listener starts on its own thread".into())),
        Job::ReadMocks(path) => Msg::MocksRead {
            result: read_mocks(&path),
            path,
        },
        Job::WriteMocks { path, text } => Msg::MocksWritten(write_mocks(&path, &text)),
        Job::Replay {
            capture,
            url,
            redactor,
        } => Msg::Done(replay(&capture, &url).map_err(|e| redactor.text(&e).into_owned())),
        other => Msg::Done(save(other)),
    };
    Action::Webhook(Box::new(msg))
}

fn prepare(
    root: Option<PathBuf>,
    environment: Option<&str>,
    backend: Option<SecretBackend>,
) -> Result<Prepared, String> {
    let Some(root) = root else {
        return Ok(Prepared {
            config: ListenerConfig::default(),
            mocks: Mocks::empty(),
            verifiers: Verifiers::default(),
            root: None,
        });
    };
    let settings = WebhookSettings::load(&root).map_err(|e| e.to_string())?;
    let mocks = Mocks::from_file(&root.join(MOCKS_FILE)).map_err(|e| e.to_string())?;
    // The environment shown in the status bar, as every TUI send uses.
    let verifiers = Verifiers::load(&root, &settings, environment, None, || {
        crate::send::secret_store(&root, backend).map_err(ConfigError::Usage)
    })
    .map_err(|e| e.to_string())?;
    Ok(Prepared {
        config: settings.listener_config(),
        mocks,
        verifiers,
        root: Some(root),
    })
}

/// Take the effects that need the action channel: the listener runs on
/// its own thread and reports every event. Others are returned.
pub fn intercept(effect: Effect, actions: &UnboundedSender<Action>) -> Option<Effect> {
    match effect {
        Effect::Webhook(job) => match *job {
            Job::Start(start) => {
                spawn_listener(start, actions.clone());
                None
            }
            other => Some(Effect::Webhook(Box::new(other))),
        },
        other => Some(other),
    }
}

/// Run the listener of `job` on a new thread until `job.stop` is cancelled
/// or the UI is gone, reporting on `actions`.
pub fn spawn_listener(job: StartJob, actions: UnboundedSender<Action>) {
    let send = |actions: &UnboundedSender<Action>, msg| {
        actions.send(Action::Webhook(Box::new(msg))).is_ok()
    };
    let spawned = std::thread::Builder::new().name("webhook".into()).spawn({
        let actions = actions.clone();
        move || match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => {
                let ran = runtime.block_on(serve(job, actions.clone()));
                // Every connection and the mock watcher are gone with the
                // runtime: the port is free once the pane hears of it.
                drop(runtime);
                if ran {
                    send(&actions, Msg::Stopped);
                }
            }
            Err(e) => {
                let message = format!("cannot start the webhook runtime: {e}");
                send(&actions, Msg::Started(Err(message)));
            }
        }
    });
    if let Err(e) = spawned {
        send(
            &actions,
            Msg::Started(Err(format!("cannot start the listener: {e}"))),
        );
    }
}

/// Run the listener until stopped; false when it did not start.
async fn serve(job: StartJob, actions: UnboundedSender<Action>) -> bool {
    let send = |msg| actions.send(Action::Webhook(Box::new(msg))).is_ok();
    let prepared = &job.prepared;
    let store = CaptureStore::default();
    let mut events = store.subscribe();
    let listener =
        match Listener::bind(&prepared.config, store.clone(), prepared.mocks.clone()).await {
            Ok(listener) => listener.with_verifiers(prepared.verifiers.clone()),
            Err(e) => {
                send(Msg::Started(Err(e.to_string())));
                return false;
            }
        };
    let (_watcher, watch_error) = match &prepared.root {
        Some(root) => match watch::watch(root, prepared.mocks.clone(), store.clone()) {
            Ok(watcher) => (Some(watcher), None),
            Err(e) => (None, Some(e)),
        },
        None => (None, None),
    };
    let running = Running {
        addr: listener.local_addr(),
        store,
        stop: job.stop.clone(),
        redactor: prepared.verifiers.redactor(),
    };
    let handle = listener.spawn();
    let started = send(Msg::Started(Ok(Box::new(running))))
        && watch_error.is_none_or(|e| send(Msg::Event(watch_warning(&e))));
    if started {
        forward(&job.stop, &mut events, send).await;
    }
    handle.stop().await;
    true
}

/// The event-log line telling that mocks will not hot-reload.
fn watch_warning(error: &notify::Error) -> EventNote {
    EventNote {
        line: format!("warning: mocks will not reload on change: {error}"),
        captured: false,
        cleared: false,
    }
}

/// Hand every listener event to the UI until `stop` or the UI is gone. Each
/// event is reduced to an [`EventNote`] here, so its capture is released
/// before the action is queued.
async fn forward(
    stop: &CancellationToken,
    events: &mut tokio::sync::broadcast::Receiver<rustman_webhook::Event>,
    send: impl Fn(Msg) -> bool,
) {
    loop {
        let received = tokio::select! {
            () = stop.cancelled() => return,
            received = events.recv() => received,
        };
        match received {
            Ok(event) => {
                if !send(Msg::Event(EventNote::from(&event))) {
                    return;
                }
            }
            Err(RecvError::Closed) => return,
            Err(RecvError::Lagged(_)) => {}
        }
    }
}

fn read_mocks(path: &Path) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(MOCKS_TEMPLATE.to_owned()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Write `text` only when it parses: an invalid file is never saved.
fn write_mocks(path: &Path, text: &str) -> Result<usize, String> {
    let mocks = rustman_webhook::mocks::parse(path, text).map_err(|e| {
        // The editor shows which file it is; keep the message short.
        let message = e.to_string();
        let prefix = format!("{}: ", path.display());
        message
            .strip_prefix(&prefix)
            .map_or_else(|| message.clone(), str::to_owned)
    })?;
    let io = |e: std::io::Error| format!("{}: {e}", path.display());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    std::fs::write(path, text).map_err(io)?;
    Ok(mocks.len())
}

fn replay(capture: &Capture, url: &str) -> Result<String, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("cannot start the async runtime: {e}"))?;
    let engine = HttpEngine::new();
    let exchange = runtime
        .block_on(rustman_webhook::replay(
            &engine,
            capture,
            url,
            &SendOptions::default(),
        ))
        .map_err(|e| format!("Replay of #{} failed: {e}", capture.id))?;
    Ok(format!(
        "Replayed #{} to {url}: {}",
        capture.id, exchange.status
    ))
}

fn save(job: Job) -> Result<String, String> {
    match job {
        Job::SaveFile {
            capture,
            url,
            path,
            redactor,
        } => {
            let saved = rustman_webhook::save::save_request_file(&capture, &url, &path, &redactor)
                .map_err(|e| e.to_string())?;
            let mut message = format!("Saved #{} as {}", capture.id, path.display());
            if !saved.withheld.is_empty() {
                message.push_str(&format!(" (left out: {})", saved.withheld.join(", ")));
            }
            Ok(message)
        }
        Job::SaveHistory {
            capture,
            root,
            settings,
            redactor,
        } => {
            let store = HistoryStore::for_workspace(&root, &settings);
            let id = rustman_webhook::save::save_to_history(&capture, &store, &redactor)
                .map_err(|e| e.to_string())?;
            Ok(format!("Saved #{} to history as {id}", capture.id))
        }
        other => Err(format!("unexpected webhook job {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    #[test]
    fn a_mock_watcher_that_fails_is_reported_on_the_event_log() {
        let dir = tempfile::tempdir().unwrap();
        let prepared = Prepared {
            config: ListenerConfig {
                port: 0,
                ..ListenerConfig::default()
            },
            mocks: Mocks::empty(),
            verifiers: Verifiers::default(),
            root: Some(dir.path().join("missing")),
        };
        let stop = CancellationToken::new();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        spawn_listener(
            StartJob {
                prepared: Arc::new(prepared),
                stop: stop.clone(),
            },
            tx,
        );
        let mut next = || match rx.blocking_recv() {
            Some(Action::Webhook(msg)) => *msg,
            _ => panic!("expected a webhook message"),
        };
        assert!(matches!(next(), Msg::Started(Ok(_))));
        let Msg::Event(note) = next() else {
            panic!("expected the watcher warning")
        };
        assert!(
            note.line
                .starts_with("warning: mocks will not reload on change: "),
            "{}",
            note.line
        );
        stop.cancel();
        assert!(matches!(next(), Msg::Stopped));
    }

    #[tokio::test]
    async fn forwarded_events_do_not_keep_their_capture() {
        let store = CaptureStore::default();
        let mut events = store.subscribe();
        let capture = store.push(super::super::tests::capture("POST", "/x", b"{}", None));
        drop(store);
        let sent = RefCell::new(Vec::new());
        let stop = CancellationToken::new();
        forward(&stop, &mut events, |msg| {
            sent.borrow_mut().push(msg);
            stop.cancel();
            true
        })
        .await;
        let sent = sent.into_inner();
        assert!(matches!(&sent[..], [Msg::Event(note)] if note.captured));
        assert_eq!(Arc::strong_count(&capture), 1, "only this test holds it");
    }
}
