//! `rustman kafka|mqtt|amqp produce|consume` (add-messaging-client "CLI
//! commands"). The operation runs in the `rustman-messaging` crate through
//! the runner, so it is resolved, asserted, recorded in history and
//! redacted like any request; this module parses flags, builds ad-hoc
//! requests, turns stdin lines into messages and prints consumed messages
//! as they arrive.
//!
//! A consume is bounded as under `rustman run`: `--max-messages` and
//! `--duration`, else the request's `limits`, else 10 messages / 30 s.

mod amqp;
mod kafka;
mod mqtt;
mod print;

use std::io::{BufRead, IsTerminal};
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::Args;
use rustman_core::collection::RequestEntry;
use rustman_core::run::{RequestRecord, RequestStatus, SkipReason};
use rustman_core::settings::Settings;
use rustman_core::workspace::RequestRef;
use rustman_core::{ConfigError, Workspace};
use rustman_http::runner::{RunContext, RunOptions, Runner};
use rustman_messaging::MessagingRunner;
use rustman_messaging::adhoc::{ad_hoc_entry, header_fields};
use serde_json::{Map, Value, json};
use tokio::sync::mpsc;

use crate::commands::exec::{
    cancel_on_ctrl_c, history_store, overrides, parse_duration, runtime, secret_store,
};
use crate::commands::{EnvSelection, current_dir, print_warnings};
use crate::exit::{ASSERTION, CliError, INTERRUPTED, REQUEST, SAFETY, SUCCESS};
use print::Printer;

pub use amqp::{AmqpArgs, run as run_amqp};
pub use kafka::{KafkaArgs, run as run_kafka};
pub use mqtt::{MqttArgs, run as run_mqtt};

/// The flags every command shares.
#[derive(Debug, Args)]
struct Common {
    /// A request file, its id or its name; without one, the flags describe
    /// the operation
    #[arg(value_name = "REQUEST")]
    request: Option<String>,
    /// Print one JSON object per message
    #[arg(long)]
    json: bool,
    #[command(flatten)]
    selection: EnvSelection,
    /// Set a variable, above every other source (repeatable): --var k=v
    #[arg(long = "var", value_name = "K=V")]
    vars: Vec<String>,
    /// Do not record the operation in history
    #[arg(long)]
    no_history: bool,
}

/// What a produce sends, without a request file.
#[derive(Debug, Args)]
struct Messages {
    /// A message value (repeatable); without one, each line of a piped
    /// stdin is a message
    #[arg(long = "value", value_name = "TEXT")]
    values: Vec<String>,
    /// Add a header to every message (repeatable): --header "source: cli"
    #[arg(long = "header", short = 'H', value_name = "NAME: VALUE")]
    headers: Vec<String>,
}

/// How long a consume runs.
#[derive(Debug, Args)]
struct Bounds {
    /// Stop after N messages (default: the request's limits.max_messages,
    /// else 10)
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    max_messages: Option<u32>,
    /// Stop after this long (default: the request's limits.duration_ms,
    /// else 30s)
    #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
    duration: Option<Duration>,
}

/// One command, ready to run: a request file, or the request-file
/// `fields` its flags make.
struct Operation<'a> {
    kind: &'static str,
    common: &'a Common,
    fields: Map<String, Value>,
    bounds: Option<&'a Bounds>,
}

impl Operation<'_> {
    /// Run it and print what it produced or consumed.
    fn run(self) -> Result<u8, CliError> {
        let ad_hoc = self.common.request.is_none();
        let session = Session::open(&self.common.selection, ad_hoc)?;
        let entry;
        let target = match &self.common.request {
            Some(request) => {
                if !self.fields.is_empty() {
                    let message = "a request file already defines its operation: drop the broker, destination and message flags";
                    return Err(ConfigError::Usage(message.to_owned()).into());
                }
                session.find(self.kind, request)?
            }
            None => {
                entry = ad_hoc_entry(self.kind, self.fields)
                    .map_err(|e| ConfigError::Usage(e.message))?;
                session.ad_hoc(&entry)
            }
        };
        let printer = Printer::new(self.common.json);
        let record = session.send(self.common, self.bounds, ad_hoc, &target, printer)?;
        printer.produced(&record);
        Ok(exit_code(&record))
    }
}

/// The `messages` field of an ad-hoc produce: one per `--value`, else
/// one per line of a piped stdin, each with `key` (Kafka) when set.
fn messages(flags: &Messages, key: Option<&str>) -> Result<Value, ConfigError> {
    let values = if flags.values.is_empty() && !std::io::stdin().is_terminal() {
        stdin_lines(std::io::stdin().lock())
    } else {
        flags.values.clone()
    };
    if values.is_empty() {
        let message = "nothing to produce: give --value or pipe lines on stdin";
        return Err(ConfigError::Usage(message.to_owned()));
    }
    let headers = header_fields(&flags.headers).map_err(ConfigError::Usage)?;
    let messages = values.into_iter().map(|value| {
        let mut message = json!({"value": value});
        if let Some(key) = key {
            message["key"] = json!(key);
        }
        if !flags.headers.is_empty() {
            message["headers"] = headers.clone();
        }
        message
    });
    Ok(Value::Array(messages.collect()))
}

/// The lines of `input`, without line endings.
fn stdin_lines(input: impl BufRead) -> Vec<String> {
    input
        .lines()
        .map_while(Result::ok)
        .map(|line| line.strip_suffix('\r').unwrap_or(&line).to_owned())
        .collect()
}

/// Insert `value` as `name` when it is set.
fn set<T: serde::Serialize>(fields: &mut Map<String, Value>, name: &str, value: Option<T>) {
    if let Some(value) = value {
        fields.insert(name.to_owned(), json!(value));
    }
}

/// The exit code of a finished operation, after printing why it failed:
/// 0 passed, 1 a failed assertion, 3 an error, 4 refused by the safety
/// policy, 130 interrupted.
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
    let blocked = record.messaging.as_ref().is_some_and(|m| m.blocked);
    match record.status {
        RequestStatus::Passed => SUCCESS,
        RequestStatus::Errored if blocked => SAFETY,
        RequestStatus::Errored => REQUEST,
        _ => ASSERTION,
    }
}

/// The workspace (if any) and what running needs from it.
struct Session {
    ws: Option<Workspace>,
    cwd: PathBuf,
    settings: Settings,
}

impl Session {
    /// The workspace, if any; outside one, only flags can describe the
    /// operation.
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

    /// Run `target`, printing consumed messages as they arrive.
    fn send(
        &self,
        common: &Common,
        bounds: Option<&Bounds>,
        ad_hoc: bool,
        target: &RequestRef<'_>,
        printer: Printer,
    ) -> Result<RequestRecord, CliError> {
        let env = match &self.ws {
            Some(ws) => common.selection.environment(ws)?,
            None => None,
        };
        let store = secret_store(self.root())?;
        let (feed, mut consumed) = mpsc::unbounded_channel::<Value>();
        let messaging = MessagingRunner {
            max_messages: bounds.and_then(|b| b.max_messages),
            duration: bounds.and_then(|b| b.duration),
            feed: Some(feed),
            ..MessagingRunner::default()
        };
        let options = RunOptions {
            overrides: overrides(&common.vars)?,
            history: history_store(common.no_history, self.ws.as_ref(), &self.settings),
            ad_hoc,
            messaging: Some(&messaging),
            ..RunOptions::default()
        };
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
        let sent = runtime()?.block_on(async {
            cancel_on_ctrl_c(&cancel);
            let send = runner.send(target, 1);
            tokio::pin!(send);
            loop {
                tokio::select! {
                    sent = &mut send => break sent,
                    Some(message) = consumed.recv() => printer.consumed(&message),
                }
            }
        });
        while let Ok(message) = consumed.try_recv() {
            printer.consumed(&message);
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

    #[test]
    fn stdin_lines_lose_their_line_endings() {
        let lines = stdin_lines("a\r\n{\"id\":1}\nlast".as_bytes());
        assert_eq!(lines, ["a", "{\"id\":1}", "last"]);
    }

    #[test]
    fn values_become_messages_with_the_key_and_headers() {
        let flags = Messages {
            values: vec!["one".into(), "two".into()],
            headers: vec!["source: cli".into()],
        };
        let messages = messages(&flags, Some("k")).unwrap();
        assert_eq!(
            messages,
            json!([
                {"value": "one", "key": "k", "headers": [{"name": "source", "value": "cli"}]},
                {"value": "two", "key": "k", "headers": [{"name": "source", "value": "cli"}]},
            ])
        );
    }

    /// The completion scripts offer every command and its flags.
    #[test]
    fn completions_cover_the_messaging_commands() {
        use clap::CommandFactory;
        let mut script = Vec::new();
        let shell = clap_complete::Shell::Bash;
        clap_complete::generate(shell, &mut crate::Cli::command(), "rustman", &mut script);
        let script = String::from_utf8(script).unwrap();
        for word in [
            "kafka",
            "mqtt",
            "amqp",
            "produce",
            "consume",
            "--routing-key",
            "--offset",
        ] {
            assert!(script.contains(word), "{word}");
        }
    }
}
