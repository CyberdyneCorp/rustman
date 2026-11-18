//! `rustman webhook listen`: run the webhook listener and print the event
//! log, one line per capture, rejection and mock match: human-readable by
//! default, one JSON object per line with `--jsonl` (spec: CLI streaming
//! output). Signature verifiers from `[webhook]` in `rustman.toml` take
//! their keys from the active environment; printed lines are redacted with
//! those keys.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::time::Duration;

use clap::{Args, Subcommand};
use rustman_core::environment::ENV_VAR;
use rustman_core::{ConfigError, Redactor};
use rustman_webhook::jsonl::event_json;
use rustman_webhook::listener::{is_remote, remote_warning};
use rustman_webhook::mocks::MOCKS_FILE;
use rustman_webhook::{
    BufferLimits, CaptureStore, Event, Limits, Listener, ListenerConfig, Mocks, Verifiers,
    WebhookSettings, watch,
};
use tokio::sync::broadcast::{self, error::RecvError, error::TryRecvError};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::current_dir;
use super::exec::{cancel_on_ctrl_c, runtime, secret_store};
use crate::exit::{CliError, INTERRUPTED, SUCCESS};

/// How long `--max-captures` waits for responses still being written
/// (a mock may delay its reply by up to 60 s) before exiting.
const FINISH_GRACE: Duration = Duration::from_secs(65);

#[derive(Debug, Args)]
#[command(subcommand_required = true, arg_required_else_help = true)]
pub struct WebhookArgs {
    #[command(subcommand)]
    command: WebhookCommand,
}

#[derive(Debug, Subcommand)]
enum WebhookCommand {
    /// Capture inbound webhooks on a local port and print each event
    Listen(ListenArgs),
}

#[derive(Debug, Args)]
pub struct ListenArgs {
    /// Address to bind: an IP address or localhost (default: [webhook] host
    /// in rustman.toml, else 127.0.0.1)
    #[arg(long, value_name = "HOST")]
    host: Option<String>,
    /// Port to bind; 0 picks a free port (default: [webhook] port in
    /// rustman.toml, else 9090)
    #[arg(long, value_name = "PORT")]
    port: Option<u16>,
    /// Allow binding an address reachable from the network
    #[arg(long)]
    allow_remote: bool,
    /// Largest accepted request body in bytes; larger bodies get 413
    #[arg(long, value_name = "BYTES", default_value_t = Limits::default().max_body)]
    max_body: usize,
    /// Exit with code 0 once N requests were captured
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    max_captures: Option<u64>,
    /// Print one JSON object per line per event (the banner goes to stderr)
    #[arg(long)]
    jsonl: bool,
    /// Workspace whose webhook/mocks.yaml answers requests (default: search
    /// from the current directory upwards; none means the default reply)
    #[arg(long, value_name = "DIR")]
    workspace: Option<PathBuf>,
    /// Environment holding the signature verifiers' secrets (default:
    /// RUSTMAN_ENV, then the workspace default)
    #[arg(long, value_name = "NAME")]
    env: Option<String>,
}

pub fn run(args: &WebhookArgs) -> Result<u8, CliError> {
    match &args.command {
        WebhookCommand::Listen(listen) => run_listen(listen),
    }
}

/// What the listener runs with, read before it binds.
struct Setup {
    config: ListenerConfig,
    root: Option<PathBuf>,
    mocks: Mocks,
    verifiers: Verifiers,
}

fn run_listen(args: &ListenArgs) -> Result<u8, CliError> {
    let setup = setup(args)?;
    let output = Output {
        jsonl: args.jsonl,
        redactor: setup.verifiers.redactor(),
    };
    runtime()?.block_on(listen(setup, output, args.max_captures))
}

fn setup(args: &ListenArgs) -> Result<Setup, CliError> {
    let invalid = |e| ConfigError::Invalid(vec![e]);
    let root = workspace_root(args.workspace.as_deref())?;
    let settings = match &root {
        Some(root) => WebhookSettings::load(root).map_err(invalid)?,
        None => WebhookSettings::default(),
    };
    let config = listener_config(args, &settings);
    // Refuse a network-reachable address before reading secrets.
    rustman_webhook::listener::bind_addr(&config)?;
    let (mocks, verifiers) = match &root {
        Some(root) => (
            Mocks::from_file(&root.join(MOCKS_FILE)).map_err(invalid)?,
            load_verifiers(root, &settings, args.env.as_deref())?,
        ),
        None => (Mocks::empty(), Verifiers::default()),
    };
    Ok(Setup {
        config,
        root,
        mocks,
        verifiers,
    })
}

/// Flags first, then `[webhook]`, then the defaults.
fn listener_config(args: &ListenArgs, settings: &WebhookSettings) -> ListenerConfig {
    let config = settings.listener_config();
    ListenerConfig {
        host: args.host.clone().unwrap_or(config.host),
        port: args.port.unwrap_or(config.port),
        allow_remote: args.allow_remote,
        limits: Limits {
            max_body: args.max_body,
            ..config.limits
        },
        ..config
    }
}

/// The verifiers of `[webhook]`, their secrets resolved from the active
/// environment.
fn load_verifiers(
    root: &Path,
    settings: &WebhookSettings,
    env: Option<&str>,
) -> Result<Verifiers, ConfigError> {
    let env_var = std::env::var(ENV_VAR).ok();
    Verifiers::load(root, settings, env, env_var.as_deref(), || {
        secret_store(root)
    })
}

/// The workspace given with `--workspace`, or found from the current
/// directory; listening works without one.
fn workspace_root(explicit: Option<&Path>) -> Result<Option<PathBuf>, ConfigError> {
    match rustman_core::discovery::find_root(&current_dir()?, explicit) {
        Ok(root) => Ok(Some(root)),
        Err(ConfigError::NoWorkspace) if explicit.is_none() => Ok(None),
        Err(err) => Err(err),
    }
}

/// How events are printed.
struct Output {
    jsonl: bool,
    redactor: Redactor,
}

impl Output {
    /// The line for `event`, redacted.
    fn line(&self, event: &Event) -> String {
        if self.jsonl {
            event_json(event, &self.redactor).to_string()
        } else {
            self.redactor.text(&event.to_string()).into_owned()
        }
    }

    /// Print `event` and flush, so a pipe sees it at once.
    fn print(&self, event: &Event) -> std::io::Result<()> {
        let mut out = std::io::stdout().lock();
        writeln!(out, "{}", self.line(event))?;
        out.flush()
    }

    /// The banner on stdout, or on stderr with `--jsonl` so stdout holds
    /// only events.
    fn banner(&self, banner: &str) {
        if self.jsonl {
            eprintln!("{banner}");
        } else {
            println!("{banner}");
        }
    }
}

/// Why the event loop ended.
#[derive(Debug, PartialEq, Eq)]
enum End {
    /// `--max-captures` was reached.
    Done,
    Interrupted,
    /// Nobody reads the output any more.
    Closed,
}

async fn listen(setup: Setup, output: Output, max: Option<u64>) -> Result<u8, CliError> {
    let Setup {
        config,
        root,
        mocks,
        verifiers,
    } = setup;
    let store = CaptureStore::new(BufferLimits::events_only());
    let mut events = store.subscribe();
    let listener = Listener::bind(&config, store.clone(), mocks.clone())
        .await?
        .with_verifiers(verifiers);
    let addr = listener.local_addr();
    output.banner(&listener.banner());
    if is_remote(addr) {
        eprintln!("warning: {}", remote_warning(addr));
    }
    let _watcher = match &root {
        Some(root) => watch::watch(root, mocks, store.clone())
            .inspect_err(|e| eprintln!("warning: mocks will not reload on change: {e}"))
            .ok(),
        None => None,
    };
    // Ctrl+C is watched on its own thread and events are written on
    // another, so a reader that stops reading never stalls the listener.
    let cancel = CancellationToken::new();
    cancel_on_ctrl_c(&cancel);
    let writer = Writer::spawn(output);
    let handle = listener.spawn();
    let end = tokio::select! {
        end = pump(&mut events, &writer, max) => end,
        () = cancel.cancelled() => End::Interrupted,
    };
    match end {
        End::Interrupted => {
            handle.stop().await;
            return Ok(INTERRUPTED);
        }
        // Nothing more can be shown: stop at once instead of waiting for
        // replies in flight.
        End::Closed => {
            handle.stop().await;
            return Ok(SUCCESS);
        }
        End::Done => {}
    }
    let finish = async {
        handle.finish(FINISH_GRACE).await;
        writer.close().await;
    };
    tokio::select! {
        () = finish => Ok(SUCCESS),
        () = cancel.cancelled() => Ok(INTERRUPTED),
    }
}

/// Events waiting for the writer thread; more are dropped and counted.
const WRITE_QUEUE: usize = 256;
/// Body bytes of the captures waiting for the writer thread; a capture
/// that would pass it is dropped unless the queue is empty.
const WRITE_QUEUE_BYTES: usize = 32 * 1024 * 1024;

/// Prints events on a thread of its own, so a blocked stdout holds up
/// only that thread. Events that do not fit the queue are dropped and
/// reported once the output moves again.
struct Writer {
    queue: SyncSender<Event>,
    counters: Arc<Counters>,
    done: oneshot::Receiver<()>,
}

/// Shared between the event pump and the writer thread.
#[derive(Default)]
struct Counters {
    /// Events not shown since the last warning.
    dropped: AtomicU64,
    /// Body bytes of the queued captures.
    queued_bytes: AtomicUsize,
}

/// The body bytes `event` keeps alive while queued.
fn body_bytes(event: &Event) -> usize {
    match event {
        Event::Captured(capture) => capture.body_size(),
        _ => 0,
    }
}

impl Writer {
    fn spawn(output: Output) -> Self {
        let (queue, events) = sync_channel::<Event>(WRITE_QUEUE);
        let counters = Arc::new(Counters::default());
        let (finished, done) = oneshot::channel();
        let shared = Arc::clone(&counters);
        std::thread::Builder::new()
            .name("webhook-output".into())
            .spawn(move || {
                write_all(&events, &output, &shared);
                let _ = finished.send(());
            })
            .expect("cannot start the output thread");
        Self {
            queue,
            counters,
            done,
        }
    }

    /// Queue `event`, or drop it when the queue is full; false once the
    /// output is closed.
    fn send(&self, event: Event) -> bool {
        let bytes = body_bytes(&event);
        let queued = self.counters.queued_bytes.load(Ordering::Relaxed);
        if queued > 0 && queued + bytes > WRITE_QUEUE_BYTES {
            self.drop_events(1);
            return true;
        }
        self.counters
            .queued_bytes
            .fetch_add(bytes, Ordering::Relaxed);
        match self.queue.try_send(event) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                self.counters
                    .queued_bytes
                    .fetch_sub(bytes, Ordering::Relaxed);
                self.drop_events(1);
                true
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }

    fn drop_events(&self, count: u64) {
        self.counters.dropped.fetch_add(count, Ordering::Relaxed);
    }

    /// Wait until the queued events were written (or the output closed).
    async fn close(self) {
        drop(self.queue);
        let _ = self.done.await;
    }
}

/// The writer thread: print each event until the queue or stdout closes.
fn write_all(events: &Receiver<Event>, output: &Output, counters: &Counters) {
    for event in events {
        let missed = counters.dropped.swap(0, Ordering::Relaxed);
        if missed > 0 {
            eprintln!("warning: {missed} events not shown (output too slow)");
        }
        let printed = output.print(&event);
        let bytes = body_bytes(&event);
        drop(event);
        counters.queued_bytes.fetch_sub(bytes, Ordering::Relaxed);
        if printed.is_err() {
            return;
        }
    }
}

/// Queue events until `max` captures were queued or output fails.
async fn pump(events: &mut broadcast::Receiver<Event>, writer: &Writer, max: Option<u64>) -> End {
    let mut captured = 0;
    loop {
        let event = match events.recv().await {
            Ok(event) => event,
            Err(RecvError::Lagged(missed)) => {
                writer.drop_events(missed);
                continue;
            }
            Err(RecvError::Closed) => return End::Closed,
        };
        let is_capture = matches!(event, Event::Captured(_));
        if !writer.send(event) {
            return End::Closed;
        }
        captured += u64::from(is_capture);
        if max == Some(captured) {
            queue_pending(events, writer);
            return End::Done;
        }
    }
}

/// After the last capture: the events already queued (such as its mock
/// match), without further captures.
fn queue_pending(events: &mut broadcast::Receiver<Event>, writer: &Writer) {
    loop {
        match events.try_recv() {
            Ok(Event::Captured(_)) | Err(TryRecvError::Lagged(_)) => {}
            Ok(event) => {
                if !writer.send(event) {
                    return;
                }
            }
            Err(TryRecvError::Empty | TryRecvError::Closed) => return,
        }
    }
}
