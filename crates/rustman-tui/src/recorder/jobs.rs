//! The pane's background work: reading `[record]` and the local CA,
//! running the proxy on its own thread, and saving or exporting captures.
//! Everything here runs off the render loop.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustman_core::SecretBackend;
use rustman_core::secrets::provider::config::user_config_dir;
use rustman_core::secrets::{BACKEND_VAR, keychain, workspace_id};
use rustman_proxy::ca::LocalCa;
use rustman_proxy::ca::store::CaStore;
use rustman_proxy::ca::trust::InstallRecord;
use rustman_proxy::save::{DEFAULT_ENVIRONMENT, SaveOptions};
use rustman_proxy::{Capture, CaptureStore, Proxy, ProxyConfig, RecordSettings};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use super::{Msg, Running};
use crate::app::{Action, Effect};

/// What the proxy runs with, read from the workspace.
#[derive(Clone, Debug)]
pub struct Prepared {
    pub config: ProxyConfig,
    /// The local CA, loaded only when `[record] intercept` is set.
    pub ca: Option<Arc<LocalCa>>,
    pub ca_trusted: bool,
}

/// Start the proxy of `prepared`; cancelling `stop` stops it.
#[derive(Clone, Debug)]
pub struct StartJob {
    pub prepared: Arc<Prepared>,
    pub stop: CancellationToken,
}

/// Background work of the pane.
#[derive(Clone)]
pub enum Job {
    /// Read `[record]` and, when it intercepts, the CA.
    Prepare {
        root: Option<PathBuf>,
        backend: Option<SecretBackend>,
    },
    /// Run the proxy (on its own thread, see [`intercept`]).
    Start(StartJob),
    /// Read whether the workspace's CA is installed in a trust store.
    CheckCa { root: PathBuf },
    /// Save `captures` as request files into the workspace at `root`.
    Save {
        root: PathBuf,
        captures: Vec<Arc<Capture>>,
    },
    /// Write `captures` to `path` as HAR, redacted.
    ExportHar {
        path: PathBuf,
        captures: Vec<Arc<Capture>>,
    },
}

/// Captures are shown by id only: their unredacted contents never reach a
/// debug string.
impl fmt::Debug for Job {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ids = |captures: &[Arc<Capture>]| captures.iter().map(|c| c.id).collect::<Vec<_>>();
        match self {
            Self::Prepare { root, backend } => f
                .debug_struct("Prepare")
                .field("root", root)
                .field("backend", backend)
                .finish(),
            Self::Start(start) => f.debug_tuple("Start").field(start).finish(),
            Self::CheckCa { root } => f.debug_struct("CheckCa").field("root", root).finish(),
            Self::Save { root, captures } => f
                .debug_struct("Save")
                .field("root", root)
                .field("captures", &ids(captures))
                .finish(),
            Self::ExportHar { path, captures } => f
                .debug_struct("ExportHar")
                .field("path", path)
                .field("captures", &ids(captures))
                .finish(),
        }
    }
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
        Job::Prepare { root, backend } => {
            Msg::Prepared(prepare(root.as_deref(), backend).map(Arc::new))
        }
        Job::Start(_) => Msg::Started(Err("the proxy starts on its own thread".into())),
        Job::CheckCa { root } => Msg::CaTrusted(ca_trusted(&root)),
        Job::Save { root, captures } => Msg::Done(save(&root, &captures)),
        Job::ExportHar { path, captures } => Msg::Done(export(&path, &captures)),
    };
    Action::Recorder(Box::new(msg))
}

fn prepare(root: Option<&Path>, backend: Option<SecretBackend>) -> Result<Prepared, String> {
    let Some(root) = root else {
        return Ok(Prepared {
            config: ProxyConfig::default(),
            ca: None,
            ca_trusted: false,
        });
    };
    let settings = RecordSettings::load(root).map_err(|e| e.to_string())?;
    let config = ProxyConfig {
        upstream_tls: rustman_proxy::config::workspace_upstream_tls(root)?,
        ..settings.proxy_config()
    };
    let ca = if config.intercept.is_empty() {
        None
    } else {
        Some(Arc::new(load_ca(root, backend)?))
    };
    Ok(Prepared {
        config,
        ca,
        ca_trusted: ca_trusted(root),
    })
}

/// The CA store of the workspace at `root`, `None` when it has no id yet
/// (so no CA either). Without a keychain the store has none.
fn ca_store(root: &Path, backend: Option<SecretBackend>) -> Result<Option<CaStore>, String> {
    let config = user_config_dir().ok_or("record: no user config directory")?;
    let Some(id) = workspace_id::load(root).map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    let backend = match backend {
        Some(backend) => backend,
        None => SecretBackend::from_env_value(std::env::var(BACKEND_VAR).ok().as_deref())
            .map_err(|e| e.to_string())?,
    };
    let keychain = match backend {
        SecretBackend::Keychain => keychain::os_keychain(),
        SecretBackend::File => None,
    };
    Ok(Some(CaStore::new(keychain, &config, &id)))
}

fn load_ca(root: &Path, backend: Option<SecretBackend>) -> Result<LocalCa, String> {
    let missing = rustman_proxy::ca::CaError::NotFound.to_string();
    let store = ca_store(root, backend)?.ok_or_else(|| missing.clone())?;
    store.load().map_err(|e| e.to_string())?.ok_or(missing)
}

/// Whether Rustman installed the CA of the workspace at `root` anywhere;
/// reads the install record only, never the keychain.
fn ca_trusted(root: &Path) -> bool {
    let Ok(Some(store)) = ca_store(root, Some(SecretBackend::File)) else {
        return false;
    };
    InstallRecord::load(&store.installed_path()).is_ok_and(|r| r.is_installed())
}

fn save(root: &Path, captures: &[Arc<Capture>]) -> Result<String, String> {
    let options = SaveOptions {
        root,
        into: None,
        environment: DEFAULT_ENVIRONMENT.to_owned(),
    };
    let saved = rustman_proxy::save::save(captures, &options, None).map_err(|e| e.to_string())?;
    if saved.is_nothing() {
        return Ok("No requests among the chosen captures; nothing saved".to_owned());
    }
    let mut message = format!(
        "Saved {} request files ({} unchanged), environment '{DEFAULT_ENVIRONMENT}'",
        saved.written, saved.unchanged
    );
    if !saved.secrets.is_empty() {
        message.push_str(&format!(
            "; secrets parameterized (set them with 'rustman secret set'): {}",
            saved.secrets.join(", ")
        ));
    }
    Ok(message)
}

fn export(path: &Path, captures: &[Arc<Capture>]) -> Result<String, String> {
    rustman_proxy::har::write_har(path, captures, false)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(format!(
        "Exported {} captures to {}",
        captures.len(),
        path.display()
    ))
}

/// Take the effects that need the action channel: the proxy runs on its
/// own thread and reports every capture. Others are returned.
pub fn intercept(effect: Effect, actions: &UnboundedSender<Action>) -> Option<Effect> {
    match effect {
        Effect::Recorder(job) => match *job {
            Job::Start(start) => {
                spawn_proxy(start, actions.clone());
                None
            }
            other => Some(Effect::Recorder(Box::new(other))),
        },
        other => Some(other),
    }
}

fn send(actions: &UnboundedSender<Action>, msg: Msg) -> bool {
    actions.send(Action::Recorder(Box::new(msg))).is_ok()
}

/// Run the proxy of `job` on a new thread until `job.stop` is cancelled or
/// the UI is gone, reporting on `actions`.
pub fn spawn_proxy(job: StartJob, actions: UnboundedSender<Action>) {
    let spawned = std::thread::Builder::new().name("recorder".into()).spawn({
        let actions = actions.clone();
        move || match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => {
                let ran = runtime.block_on(serve(job, &actions));
                // Every connection is gone with the runtime: the port is
                // free once the pane hears of it.
                drop(runtime);
                if ran {
                    send(&actions, Msg::Stopped);
                }
            }
            Err(e) => {
                let message = format!("cannot start the recorder runtime: {e}");
                send(&actions, Msg::Started(Err(message)));
            }
        }
    });
    if let Err(e) = spawned {
        let message = format!("cannot start the recording proxy: {e}");
        send(&actions, Msg::Started(Err(message)));
    }
}

/// Bind and serve until stopped; false when it did not start.
async fn serve(job: StartJob, actions: &UnboundedSender<Action>) -> bool {
    let config = &job.prepared.config;
    let store = CaptureStore::new(config.max_captures, config.max_buffer_bytes);
    let mut live = store.subscribe();
    let proxy = match Proxy::bind(config, store.clone(), job.prepared.ca.as_deref()).await {
        Ok(proxy) => proxy,
        Err(err) => {
            send(actions, Msg::Started(Err(err.to_string())));
            return false;
        }
    };
    let running = Running {
        addr: proxy.local_addr(),
        store,
        stop: job.stop.clone(),
        intercept: config.intercept.clone(),
    };
    tokio::spawn(proxy.run(job.stop.clone()));
    if !send(actions, Msg::Started(Ok(Box::new(running)))) {
        job.stop.cancel();
        return true;
    }
    loop {
        let received = tokio::select! {
            () = job.stop.cancelled() => return true,
            received = live.recv() => received,
        };
        if matches!(received, Err(RecvError::Closed)) || !send(actions, Msg::Captured) {
            job.stop.cancel();
            return true;
        }
    }
}
