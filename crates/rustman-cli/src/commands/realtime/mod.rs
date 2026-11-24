//! `rustman ws connect` and `rustman sse listen` (add-realtime-client "CLI
//! commands"). The session runs in the `rustman-realtime` crate through the
//! runner, so it is resolved, asserted, recorded in history and redacted
//! like any request; this module parses flags, builds ad-hoc requests,
//! turns stdin lines into text frames and prints the received messages as
//! they arrive.
//!
//! Sessions are bounded as under `rustman run`: `--max-messages` and
//! `--duration`, else the request's `limits`, else 50 messages / 30 s.

mod print;
mod sse;
mod ws;

use std::io::{BufRead, IsTerminal};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use clap::Args;
use rustman_core::collection::RequestEntry;
use rustman_core::run::{RequestRecord, RequestStatus, SkipReason};
use rustman_core::settings::Settings;
use rustman_core::vars::ResolvedFrame;
use rustman_core::workspace::RequestRef;
use rustman_core::{ConfigError, Workspace};
use rustman_http::runner::{RunContext, Runner, Sent};
use rustman_realtime::{Command, RealtimeRunner};
use serde_json::{Map, Value};
use tokio::sync::mpsc;

use crate::commands::exec::{
    SendFlags, cancel_on_ctrl_c, history_store, parse_duration, runtime, secret_store,
};
use crate::commands::{EnvSelection, current_dir, print_warnings};
use crate::exit::{ASSERTION, CliError, INTERRUPTED, REQUEST, SUCCESS};
use print::Printer;

pub use sse::{SseArgs, run as run_sse};
pub use ws::{WsArgs, run as run_ws};

/// The flags both commands share.
#[derive(Debug, Args)]
struct Listen {
    /// A URL (or a {{variable}} holding one), or a request file, its id or
    /// its name
    #[arg(value_name = "URL|REQUEST")]
    target: String,
    /// Add a header (repeatable): --header "Authorization: Bearer {{token}}"
    #[arg(long = "header", short = 'H', value_name = "NAME: VALUE")]
    headers: Vec<String>,
    /// Stop after N received messages (default: the request's
    /// limits.max_messages, else 50)
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    max_messages: Option<u32>,
    /// Stop after this long (default: the request's limits.duration_ms,
    /// else 30s)
    #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
    duration: Option<Duration>,
    /// Print one JSON object per received message (type, id, data, t_ms,
    /// dir, size)
    #[arg(long)]
    json: bool,
    #[command(flatten)]
    selection: EnvSelection,
    #[command(flatten)]
    flags: SendFlags,
}

/// Whether `target` is a URL (or a `{{variable}}`) rather than a request.
fn is_url(target: &str) -> bool {
    target.contains("://") || target.starts_with("{{")
}

/// Run the session `listen` names: an ad-hoc request of `kind` with
/// `fields`, or a saved request of that kind. `stdin_frames`: send the
/// lines of a piped stdin as text frames.
fn run(
    kind: &'static str,
    listen: &Listen,
    mut fields: Map<String, Value>,
    stdin_frames: bool,
) -> Result<u8, CliError> {
    let ad_hoc = is_url(&listen.target);
    let session = Session::open(&listen.selection, ad_hoc)?;
    let entry;
    let target = if ad_hoc {
        if !listen.headers.is_empty() {
            let headers =
                rustman_realtime::header_fields(&listen.headers).map_err(ConfigError::Usage)?;
            fields.insert("headers".into(), headers);
        }
        entry = rustman_realtime::ad_hoc_entry(kind, &listen.target, fields)
            .map_err(|e| ConfigError::Invalid(vec![e]))?;
        session.ad_hoc(&entry)
    } else {
        if !listen.headers.is_empty() || !fields.is_empty() {
            let message =
                "a request file already defines its session: drop --header and the request flags";
            return Err(ConfigError::Usage(message.to_owned()).into());
        }
        session.find(kind, &listen.target)?
    };
    let commands = (stdin_frames && !std::io::stdin().is_terminal()).then(read_stdin);
    let printer = Printer::new(listen.json);
    let record = session.send(listen, ad_hoc, &target, commands, &printer)?;
    Ok(exit_code(&record))
}

/// Send each line of stdin as a text frame, from a thread of its own.
fn read_stdin() -> mpsc::Receiver<Command> {
    let (tx, rx) = rustman_realtime::command_channel();
    let spawned = std::thread::Builder::new()
        .name("rustman-stdin-frames".into())
        .spawn(move || forward_lines(std::io::stdin().lock(), &tx));
    if let Err(e) = spawned {
        print_warnings(&[format!("cannot read stdin: {e}")]);
    }
    rx
}

/// Send each line of `input` to `tx` as a text frame, waiting while the
/// (bounded) channel is full, until the input or the session ends.
fn forward_lines(input: impl BufRead, tx: &mpsc::Sender<Command>) {
    for line in input.lines() {
        let Ok(line) = line else { break };
        let line = line.strip_suffix('\r').unwrap_or(&line).to_owned();
        if tx
            .blocking_send(Command::Send(ResolvedFrame::Text(line)))
            .is_err()
        {
            break;
        }
    }
}

/// The exit code of a finished session, after printing why it failed:
/// 0 passed, 1 a failed assertion, 3 an error, 130 interrupted.
fn exit_code(record: &RequestRecord) -> u8 {
    if record.skip_reason == Some(SkipReason::Interrupted) {
        eprintln!("interrupted");
        return INTERRUPTED;
    }
    if let Some(reason) = record.skip_reason {
        eprintln!("error: request skipped ({reason})");
        return REQUEST;
    }
    for failed in record.assertions.iter().filter(|a| !a.passed()) {
        eprintln!("fail: {failed}");
    }
    if let Some(error) = &record.error {
        eprintln!("error: {error}");
    }
    match record.status {
        RequestStatus::Passed => SUCCESS,
        RequestStatus::Errored => REQUEST,
        _ => ASSERTION,
    }
}

/// The workspace (if any) and what sending needs from it.
struct Session {
    ws: Option<Workspace>,
    cwd: PathBuf,
    settings: Settings,
}

impl Session {
    /// The workspace, if any; outside one, only URLs can be opened.
    fn open(selection: &EnvSelection, ad_hoc: bool) -> Result<Self, ConfigError> {
        let cwd = current_dir()?;
        let explicit = selection.workspace.is_some() || selection.env.is_some();
        let ws = match selection.workspace() {
            Ok(ws) => Some(ws),
            Err(ConfigError::NoWorkspace) if !explicit && ad_hoc => None,
            Err(e) => return Err(e),
        };
        let settings = ws
            .as_ref()
            .map_or_else(Settings::ad_hoc, |w| w.settings.clone());
        Ok(Self { ws, cwd, settings })
    }

    fn root(&self) -> &Path {
        self.ws.as_ref().map_or(self.cwd.as_path(), |w| &w.root)
    }

    /// The `kind` request `target` (a file, id or name) of the workspace.
    fn find(&self, kind: &str, target: &str) -> Result<RequestRef<'_>, ConfigError> {
        let ws = self.ws.as_ref().ok_or(ConfigError::NoWorkspace)?;
        let found = ws.find_request(target, &self.cwd)?;
        if found.request().kind() != kind {
            let message = format!(
                "request '{}' is not a kind: {kind} request",
                found.entry.id()
            );
            return Err(ConfigError::Usage(message));
        }
        Ok(found)
    }

    /// `entry` as a request of this session.
    fn ad_hoc<'s>(&'s self, entry: &'s RequestEntry) -> RequestRef<'s> {
        RequestRef {
            root: self.root(),
            http: &self.settings.http,
            entry,
            ancestors: Vec::new(),
        }
    }

    /// Run the session of `target`, printing its messages as they arrive.
    fn send(
        &self,
        listen: &Listen,
        ad_hoc: bool,
        target: &RequestRef<'_>,
        commands: Option<mpsc::Receiver<Command>>,
        printer: &Printer,
    ) -> Result<RequestRecord, CliError> {
        let env = match &self.ws {
            Some(ws) => listen.selection.environment(ws)?,
            None => None,
        };
        let store = secret_store(self.root())?;
        let (feed, mut entries) = rustman_realtime::feed_channel();
        let realtime = RealtimeRunner {
            max_messages: listen.max_messages,
            duration: listen.duration,
            feed: Some(feed),
            commands: Mutex::new(commands),
            ..RealtimeRunner::default()
        };
        let mut options = listen.flags.options()?;
        options.history = history_store(listen.flags.no_history, self.ws.as_ref(), &self.settings);
        options.ad_hoc = ad_hoc;
        options.realtime = Some(&realtime);
        let cancel = options.cancel.clone();
        let ctx = RunContext {
            root: self.root(),
            settings: &self.settings,
            environment: env,
            secrets: &store,
            in_workspace: self.ws.is_some(),
        };
        let providers = crate::commands::exec::secret_providers(self.root(), &self.settings)?;
        let runner = Runner::new(ctx, options)?.with_secret_providers(providers);
        let sent: Sent = runtime()?.block_on(async {
            cancel_on_ctrl_c(&cancel);
            let send = runner.send(target, 1);
            tokio::pin!(send);
            loop {
                tokio::select! {
                    sent = &mut send => break sent,
                    Some(item) = entries.recv() => printer.item(&item),
                }
            }
        });
        while let Some(item) = entries.try_recv() {
            printer.item(&item);
        }
        if let Err(e) = runner.finish() {
            print_warnings(&[e.to_string()]);
        }
        print_warnings(&store.take_warnings());
        print_warnings(&sent.record.warnings);
        Ok(sent.record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fast stdin never queues more frames than the channel holds: the
    /// reader waits for the session to take them.
    #[test]
    fn stdin_lines_wait_for_room_in_a_bounded_channel() {
        let lines = "f\r\n".repeat(rustman_realtime::COMMAND_QUEUE * 3);
        let (tx, mut rx) = rustman_realtime::command_channel();
        let reader = std::thread::spawn(move || forward_lines(lines.as_bytes(), &tx));
        let mut received = 0;
        while let Some(command) = rx.blocking_recv() {
            assert!(rx.len() <= rustman_realtime::COMMAND_QUEUE);
            assert_eq!(command, Command::Send(ResolvedFrame::Text("f".into())));
            received += 1;
        }
        reader.join().unwrap();
        assert_eq!(received, rustman_realtime::COMMAND_QUEUE * 3);
    }

    #[test]
    fn urls_and_requests_are_told_apart() {
        assert!(is_url("wss://feed.example/ws"));
        assert!(is_url("https://api.example/events"));
        assert!(is_url("{{feed_url}}"));
        assert!(!is_url("feeds/prices.rman.yaml"));
        assert!(!is_url("Prices"));
    }

    /// The completion scripts offer both commands and their flags.
    #[test]
    fn completions_cover_ws_connect_and_sse_listen() {
        use clap::CommandFactory;
        for shell in [clap_complete::Shell::Zsh, clap_complete::Shell::Bash] {
            let mut script = Vec::new();
            clap_complete::generate(shell, &mut crate::Cli::command(), "rustman", &mut script);
            let script = String::from_utf8(script).unwrap();
            for word in ["connect", "listen", "--max-messages", "--subprotocol"] {
                assert!(script.contains(word), "{shell}: {word}");
            }
        }
    }
}
