//! The pane's background work: reading `[mock]`, and running the server
//! on its own thread with hot reload, forwarding its request log.

use std::path::PathBuf;
use std::sync::Arc;

use rustman_mock::watch::watch;
use rustman_mock::{MockConfig, MockEvent, MockServer, MockSettings, Overrides, Reload};
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use super::{Msg, Running};
use crate::app::{Action, Effect};

/// What the server runs with, read from the workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prepared {
    pub config: MockConfig,
    pub document: String,
    /// The `*.rman-mock.yaml` directory (`[mock] dir`, else `mocks/`).
    pub mocks_dir: Option<PathBuf>,
}

/// Start the server of `prepared`; cancelling `stop` stops it.
#[derive(Clone, Debug)]
pub struct StartJob {
    pub prepared: Arc<Prepared>,
    pub stop: CancellationToken,
}

/// Background work of the pane.
#[derive(Clone, Debug)]
pub enum Job {
    /// Read `[mock]` of the workspace at `root`.
    Prepare {
        root: Option<PathBuf>,
        document: String,
    },
    /// Run the server (on its own thread, see [`intercept`]).
    Start(StartJob),
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
        Job::Prepare { root, document } => Msg::Prepared(prepare(root, document).map(Arc::new)),
        Job::Start(_) => Msg::Started(Err("the mock server starts on its own thread".into())),
    };
    Action::Mock(Box::new(msg))
}

fn prepare(root: Option<PathBuf>, document: String) -> Result<Prepared, String> {
    let Some(root) = root else {
        return Ok(Prepared {
            config: MockConfig::default(),
            document,
            mocks_dir: None,
        });
    };
    let settings = MockSettings::load(&root).map_err(|e| e.to_string())?;
    settings.latency().map_err(|e| e.to_string())?;
    // A relative document is read from the workspace root.
    let path = root.join(&document);
    let document = if path.is_file() {
        path.to_string_lossy().into_owned()
    } else {
        document
    };
    Ok(Prepared {
        config: settings.config(),
        document,
        mocks_dir: Some(settings.mocks_dir(&root)),
    })
}

/// Take the effects that need the action channel: the server runs on its
/// own thread and reports every request. Others are returned.
pub fn intercept(effect: Effect, actions: &UnboundedSender<Action>) -> Option<Effect> {
    match effect {
        Effect::Mock(job) => match *job {
            Job::Start(start) => {
                spawn_server(start, actions.clone());
                None
            }
            other => Some(Effect::Mock(Box::new(other))),
        },
        other => Some(other),
    }
}

fn send(actions: &UnboundedSender<Action>, msg: Msg) -> bool {
    actions.send(Action::Mock(Box::new(msg))).is_ok()
}

/// Run the server of `job` on a new thread until `job.stop` is cancelled
/// or the UI is gone, reporting on `actions`.
pub fn spawn_server(job: StartJob, actions: UnboundedSender<Action>) {
    let spawned = std::thread::Builder::new().name("mock".into()).spawn({
        let actions = actions.clone();
        move || match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => {
                let ran = runtime.block_on(serve(job, &actions));
                // Every connection and the watcher are gone with the
                // runtime: the port is free once the pane hears of it.
                drop(runtime);
                if ran {
                    send(&actions, Msg::Stopped);
                }
            }
            Err(e) => {
                let message = format!("cannot start the mock runtime: {e}");
                send(&actions, Msg::Started(Err(message)));
            }
        }
    });
    if let Err(e) = spawned {
        let message = format!("cannot start the mock server: {e}");
        send(&actions, Msg::Started(Err(message)));
    }
}

/// Load, bind and serve until stopped; false when it did not start.
async fn serve(job: StartJob, actions: &UnboundedSender<Action>) -> bool {
    let server = match bind(&job.prepared).await {
        Ok(server) => server,
        Err(message) => {
            send(actions, Msg::Started(Err(message)));
            return false;
        }
    };
    let mut events = server.subscribe();
    let prepared = &job.prepared;
    let reload = Reload {
        document: prepared.document.clone(),
        allow_remote_refs: false,
        mocks_dir: prepared.mocks_dir.clone(),
    };
    let watcher = watch(server.reloader(reload));
    let running = Running {
        addr: server.local_addr(),
        operations: server.operations(),
        stop: job.stop.clone(),
    };
    let handle = server.spawn();
    let mut started = send(actions, Msg::Started(Ok(Box::new(running))));
    if let Err(e) = &watcher {
        let line = format!("warning: changes will not be reloaded: {e}");
        started = started && send(actions, Msg::Notice(line));
    }
    if started {
        forward(&job.stop, &mut events, actions).await;
    }
    drop(watcher);
    handle.stop().await;
    true
}

async fn bind(prepared: &Prepared) -> Result<MockServer, String> {
    let overrides = match &prepared.mocks_dir {
        Some(dir) => Overrides::load_dir(dir).map_err(|e| e.to_string())?,
        None => Overrides::default(),
    };
    let state = rustman_mock::load(&prepared.document, false)
        .await
        .map_err(|e| e.to_string())?
        .with_overrides(overrides);
    MockServer::bind(&prepared.config, Arc::new(state))
        .await
        .map_err(|e| e.to_string())
}

/// Hand every log entry and notice to the UI until `stop` or the UI is
/// gone.
async fn forward(
    stop: &CancellationToken,
    events: &mut broadcast::Receiver<MockEvent>,
    actions: &UnboundedSender<Action>,
) {
    loop {
        let received = tokio::select! {
            () = stop.cancelled() => return,
            received = events.recv() => received,
        };
        let msg = match received {
            Ok(MockEvent::Request(entry)) => Msg::Entry(entry),
            Ok(event) => Msg::Notice(event.notice().unwrap_or_default()),
            Err(RecvError::Lagged(missed)) => Msg::Notice(format!("{missed} requests not shown")),
            Err(RecvError::Closed) => return,
        };
        if !send(actions, msg) {
            return;
        }
    }
}
