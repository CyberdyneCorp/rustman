//! The inspector's background work: reading the workspace's server
//! definitions, the trust check, recording an approval, saving a call as a
//! request file, and the session itself, which runs on its own thread with
//! its own runtime (see [`spawn_session`]) and reports to the UI as
//! [`Msg`] actions: lists, results, progress, notifications, stderr lines
//! and the sampling and elicitation requests waiting for the user.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rustman_core::model::{McpRequest, REQUEST_FILE_SUFFIX, Request};
use rustman_core::trust::{TrustKey, TrustStore};
use rustman_core::vars::{EnvironmentSource, Layer, Resolver, VariablesSource};
use rustman_core::{SecretBackend, Workspace};
use rustman_mcp::client::oauth::{OAuthOptions, OAuthPrompt, open_browser};
use rustman_mcp::client::protocol::LoggingLevel;
use rustman_mcp::client::{
    Approved, CallOptions, ConnectOptions, InteractiveBroker, McpOp, McpSession, Operation,
    PendingApproval, RequestOptions, ResolvedServer, TrustCheck, check_trust, load_servers,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::{Connected, Lists, Msg, Performed};
use crate::app::{Action, Effect};

/// How often the session's stderr log and status are checked.
const POLL: Duration = Duration::from_millis(500);

/// Background work of the inspector.
#[derive(Clone)]
pub enum Job {
    /// Read `[mcp_servers]` and `mcp/servers/`.
    LoadServers { root: PathBuf },
    /// Resolve the server `name` and check its trust.
    Prepare {
        root: PathBuf,
        name: String,
        environment: Option<String>,
        backend: Option<SecretBackend>,
        trust_key: TrustKey,
    },
    /// The user approved a stdio command: record it.
    Trust {
        root: PathBuf,
        trust_key: TrustKey,
        pending: PendingApproval,
        server: Box<ResolvedServer>,
    },
    /// Connect and run the session (on its own thread). OAuth tokens are
    /// kept in the secret store of the workspace at `root`.
    Connect {
        server: Box<ResolvedServer>,
        approved: Approved,
        root: Option<PathBuf>,
        backend: Option<SecretBackend>,
    },
    /// Write a call as a `kind: mcp` request file.
    Save {
        path: PathBuf,
        request: Box<McpRequest>,
    },
}

/// Jobs show only what identifies them: a resolved server holds secrets.
impl fmt::Debug for Job {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LoadServers { root } => write!(f, "LoadServers({})", root.display()),
            Self::Prepare { name, .. } => write!(f, "Prepare({name})"),
            Self::Trust { pending, .. } => write!(f, "Trust({})", pending.server),
            Self::Connect { server, .. } => write!(f, "Connect({})", server.name),
            Self::Save { path, .. } => write!(f, "Save({})", path.display()),
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

/// What the session thread is asked to do.
#[derive(Debug)]
pub enum Command {
    Perform {
        title: String,
        op: McpOp,
        validate: bool,
        cancel: CancellationToken,
    },
    SetLevel(LoggingLevel),
    Subscribe(String),
    Disconnect,
}

/// A resolved server and its trust check.
#[derive(Debug)]
pub struct Prepared {
    pub server: Box<ResolvedServer>,
    pub check: TrustCheck,
}

/// Run `job` to completion (blocking).
pub fn execute(job: Job) -> Action {
    let msg = match job {
        Job::LoadServers { root } => Msg::Servers(servers(&root)),
        Job::Prepare {
            root,
            name,
            environment,
            backend,
            trust_key,
        } => Msg::Prepared {
            result: prepare(&root, &name, environment.as_deref(), backend, trust_key).map(Box::new),
            name,
        },
        Job::Trust {
            root,
            trust_key,
            pending,
            server,
        } => Msg::Trusted {
            result: trust(&root, trust_key, pending).map(|approved| (server, approved)),
        },
        Job::Connect { server, .. } => Msg::Connected {
            name: server.name.clone(),
            result: Err("sessions run on their own thread".into()),
        },
        Job::Save { path, request } => Msg::Saved(save(&path, &request)),
    };
    Action::Mcp(Box::new(msg))
}

/// Take the effects that need the action channel: a session runs on its
/// own thread and reports every event. Others are returned.
pub fn intercept(effect: Effect, actions: &UnboundedSender<Action>) -> Option<Effect> {
    match effect {
        Effect::Mcp(job) => match *job {
            Job::Connect {
                server,
                approved,
                root,
                backend,
            } => {
                let open = |url: &str| {
                    // The toast shows the URL when no browser starts.
                    let _ = open_browser(url);
                };
                let oauth = oauth_options(root.as_deref(), backend, actions, open);
                spawn_session(*server, approved, oauth, actions.clone());
                None
            }
            other => Some(Effect::Mcp(Box::new(other))),
        },
        other => Some(other),
    }
}

fn servers(root: &Path) -> Result<Vec<(String, &'static str)>, String> {
    let servers = load_servers(root).map_err(|e| e.to_string())?;
    Ok(servers
        .into_values()
        .map(|d| (d.name.clone(), d.transport_name()))
        .collect())
}

fn prepare(
    root: &Path,
    name: &str,
    environment: Option<&str>,
    backend: Option<SecretBackend>,
    trust_key: TrustKey,
) -> Result<Prepared, String> {
    let ws = Workspace::load_with(root, &crate::files::registry()).map_err(|e| e.to_string())?;
    let store = crate::send::secret_store(root, backend)?;
    let env = ws
        .active_environment(environment, None)
        .map_err(|e| e.to_string())?;
    let mut resolver = Resolver::new();
    if let Some(env) = env {
        resolver.push(EnvironmentSource::new(env, &store));
    }
    resolver.push(VariablesSource::new(
        Layer::Settings,
        &ws.settings.variables,
    ));
    let server = rustman_mcp::client::find_server(root, name)
        .and_then(|d| d.resolve(&resolver, root))
        .map_err(|e| e.to_string())?;
    let trust = TrustStore::load_with_key(root, trust_key).map_err(|e| e.to_string())?;
    let check = check_trust(&trust, &server);
    Ok(Prepared {
        server: Box::new(server),
        check,
    })
}

fn trust(root: &Path, key: TrustKey, pending: PendingApproval) -> Result<Approved, String> {
    let mut store = TrustStore::load_with_key(root, key).map_err(|e| e.to_string())?;
    pending.approve(&mut store).map_err(|e| e.to_string())
}

fn save(path: &Path, request: &McpRequest) -> Result<String, String> {
    if path.exists() {
        return Err(format!("{} already exists", path.display()));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let text =
        rustman_core::format::write::request_to_yaml(&Request::Mcp(Box::new(request.clone())));
    std::fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| format!("request{REQUEST_FILE_SUFFIX}"));
    Ok(format!("Saved {name}"))
}

/// OAuth for the sessions of the workspace at `root`: tokens in its secret
/// store, and a login shows the authorization URL (and opens it with
/// `open`). `None` without a workspace.
pub fn oauth_options(
    root: Option<&Path>,
    backend: Option<SecretBackend>,
    actions: &UnboundedSender<Action>,
    open: fn(&str),
) -> Option<OAuthOptions> {
    let store = crate::send::secret_store(root?, backend).ok()?;
    let prompt = TuiPrompt {
        actions: actions.clone(),
        open,
    };
    Some(OAuthOptions {
        store: Arc::new(store),
        prompt: Some(Arc::new(prompt)),
    })
}

/// Shows the authorization URL in a toast and opens the browser; the
/// loopback redirect completes the login.
struct TuiPrompt {
    actions: UnboundedSender<Action>,
    open: fn(&str),
}

impl OAuthPrompt for TuiPrompt {
    fn open(&self, url: &str) {
        send(&self.actions, Msg::Login(url.to_owned()));
        (self.open)(url);
    }
}

/// Run a session for `server` on a new thread until it is disconnected or
/// the UI is gone, reporting on `actions`.
pub fn spawn_session(
    server: ResolvedServer,
    approved: Approved,
    oauth: Option<OAuthOptions>,
    actions: UnboundedSender<Action>,
) {
    let name = server.name.clone();
    let failed = actions.clone();
    let spawned = std::thread::Builder::new()
        .name("mcp".into())
        .spawn(move || {
            match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime.block_on(run(server, approved, oauth, actions)),
                Err(e) => {
                    send(&actions, connect_failed(&server.name, e.to_string()));
                }
            }
        });
    if let Err(e) = spawned {
        send(&failed, connect_failed(&name, e.to_string()));
    }
}

fn connect_failed(name: &str, message: String) -> Msg {
    Msg::Connected {
        name: name.to_owned(),
        result: Err(message),
    }
}

fn send(actions: &UnboundedSender<Action>, msg: Msg) -> bool {
    actions.send(Action::Mcp(Box::new(msg))).is_ok()
}

async fn run(
    server: ResolvedServer,
    approved: Approved,
    oauth: Option<OAuthOptions>,
    actions: UnboundedSender<Action>,
) {
    let (broker, approvals) = InteractiveBroker::new();
    let options = ConnectOptions {
        broker: Arc::new(broker),
        oauth,
        ..ConnectOptions::default()
    };
    let session = match McpSession::connect(&server, &approved, &options).await {
        Ok(session) => Arc::new(session),
        Err(e) => {
            send(&actions, connect_failed(&server.name, e.to_string()));
            return;
        }
    };
    let (commands, receiver) = unbounded_channel();
    let lists = lists(&session).await;
    let connected = Connected {
        info: session.info().clone(),
        lists,
        commands,
    };
    if !send(
        &actions,
        Msg::Connected {
            name: server.name.clone(),
            result: Ok(Box::new(connected)),
        },
    ) {
        return;
    }
    serve(&session, receiver, approvals, &actions).await;
    if let Ok(session) = Arc::try_unwrap(session) {
        session.shutdown().await;
    }
}

/// Every list the server's capabilities allow; an unanswered one is empty.
async fn lists(session: &McpSession) -> Lists {
    let options = RequestOptions::default();
    let declared = |op| session.require(op).is_ok();
    let mut lists = Lists::default();
    if declared(Operation::ListTools)
        && let Ok(e) = session.list_tools(&options).await
    {
        lists.tools = e.result.to_vec();
    }
    if declared(Operation::ListResources)
        && let Ok(e) = session.list_resources(&options).await
    {
        lists.resources = e.result;
    }
    if declared(Operation::ListResourceTemplates)
        && let Ok(e) = session.list_resource_templates(&options).await
    {
        lists.templates = e.result;
    }
    if declared(Operation::ListPrompts)
        && let Ok(e) = session.list_prompts(&options).await
    {
        lists.prompts = e.result;
    }
    lists
}

/// Forward commands, approvals, notifications and stderr until the session
/// ends or the UI disconnects it.
async fn serve(
    session: &Arc<McpSession>,
    mut commands: UnboundedReceiver<Command>,
    mut approvals: UnboundedReceiver<rustman_mcp::client::Approval>,
    actions: &UnboundedSender<Action>,
) {
    let mut notes = session.notifications().subscribe();
    let mut tools = session.tool_changes();
    let mut poll = tokio::time::interval(POLL);
    let mut stderr_seen = 0;
    loop {
        let alive = tokio::select! {
            command = commands.recv() => match command {
                None | Some(Command::Disconnect) => false,
                Some(command) => { handle(session, command, actions); true }
            },
            Some(approval) = approvals.recv() => send(actions, Msg::Approval(approval)),
            note = notes.recv() => match note {
                Ok(note) => send(actions, Msg::Notification(Box::new(note))),
                Err(_) => { notes = notes.resubscribe(); true }
            },
            Ok(()) = tools.changed() => {
                let listed = session.cached_tools().map(|t| t.to_vec()).unwrap_or_default();
                send(actions, Msg::Tools(listed))
            }
            _ = poll.tick() => poll_session(session, &mut stderr_seen, actions),
        };
        if !alive {
            return;
        }
    }
}

/// New stderr lines, and whether the server is still there.
fn poll_session(session: &McpSession, seen: &mut usize, actions: &UnboundedSender<Action>) -> bool {
    let lines = session.stderr().map(|s| s.lines()).unwrap_or_default();
    if lines.len() > *seen {
        let new = lines[*seen..].to_vec();
        *seen = lines.len();
        if !send(actions, Msg::Stderr(new)) {
            return false;
        }
    }
    match session.status() {
        rustman_mcp::client::SessionStatus::Connected => true,
        rustman_mcp::client::SessionStatus::Disconnected { exit, .. } => {
            let how = match exit.and_then(|e| e.code) {
                Some(code) => format!("exit code {code}"),
                None => "connection closed".to_owned(),
            };
            send(actions, Msg::Disconnected(format!("disconnected: {how}")));
            false
        }
    }
}

/// Run `command` without blocking the loop: a tool call may wait for the
/// user to answer the server's sampling request.
fn handle(session: &Arc<McpSession>, command: Command, actions: &UnboundedSender<Action>) {
    let session = session.clone();
    let actions = actions.clone();
    tokio::spawn(async move {
        let msg = match command {
            Command::Perform {
                title,
                op,
                validate,
                cancel,
            } => perform(&session, title, op, validate, cancel, &actions).await,
            Command::SetLevel(level) => Msg::Done(
                session
                    .set_log_level(level, &RequestOptions::default())
                    .await
                    .map(|()| "Log level set".to_owned())
                    .map_err(|e| e.to_string()),
            ),
            Command::Subscribe(uri) => Msg::Done(
                session
                    .subscribe(&uri, &RequestOptions::default())
                    .await
                    .map(|()| format!("Subscribed to {uri}"))
                    .map_err(|e| e.to_string()),
            ),
            Command::Disconnect => return,
        };
        send(&actions, msg);
    });
}

async fn perform(
    session: &McpSession,
    title: String,
    op: McpOp,
    validate: bool,
    cancel: CancellationToken,
    actions: &UnboundedSender<Action>,
) -> Msg {
    let (progress, mut watched) = watch::channel(None);
    let forward = {
        let actions = actions.clone();
        async move {
            while watched.changed().await.is_ok() {
                if let Some(p) = watched.borrow_and_update().clone() {
                    send(&actions, Msg::Progress(p));
                }
            }
        }
    };
    let options = CallOptions {
        request: RequestOptions {
            timeout: None,
            cancel: Some(cancel),
            progress: Some(progress),
        },
        validate,
    };
    let call = async {
        let result = session.perform(&op, &options).await;
        drop(options);
        result
    };
    let (result, ()) = tokio::join!(call, forward);
    Msg::Performed(Box::new(Performed {
        title,
        op,
        result: result.map(Box::new).map_err(|e| e.to_string()),
    }))
}
