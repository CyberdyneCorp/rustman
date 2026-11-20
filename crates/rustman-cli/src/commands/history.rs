//! `rustman history list|show|replay|clear`: past exchanges, redacted, from
//! the workspace's `.rustman/history/` (or, outside any workspace, the
//! platform data directory).

use std::io::Write;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use rustman_core::history::{Entry, HistoryStore, Query, StatusFilter, parse_age};
use rustman_core::settings::{HistorySettings, Settings};
use rustman_core::{ConfigError, Workspace};
use rustman_http::runner::{RunContext, RunOptions, Runner};
use serde_json::{Value, json};

use super::exec::{cancel_on_ctrl_c, login_mode, runtime, secret_providers, secret_store};
use super::send::{OutputFlags, finish};
use super::{current_dir, print_warnings};
use crate::exit::{CliError, SUCCESS};

#[derive(Debug, Args)]
pub struct HistoryArgs {
    #[command(subcommand)]
    command: HistoryCommand,
    /// Workspace directory (default: search from the current directory
    /// upwards; outside any workspace, the ad-hoc history)
    #[arg(long, value_name = "DIR", global = true)]
    workspace: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum HistoryCommand {
    /// List exchanges, newest first: id, time, method, URL, status and total ms
    List(ListArgs),
    /// Print one exchange in full (redacted)
    Show(ShowArgs),
    /// Send an exchange's request again, with secrets read from the store now
    Replay(ReplayArgs),
    /// Delete every exchange (and its spilled body)
    Clear,
}

#[derive(Debug, Args)]
struct ListArgs {
    /// Only exchanges with this status code (404) or class (5xx)
    #[arg(long, value_name = "CODE|CLASS")]
    status: Option<StatusFilter>,
    /// Only exchanges with this method
    #[arg(long, value_name = "METHOD")]
    method: Option<String>,
    /// Only exchanges whose URL, request body or response body contains
    /// TEXT (ignoring case)
    #[arg(long, value_name = "TEXT")]
    grep: Option<String>,
    /// Only exchanges from the last DURATION: 30s, 15m, 1h, 2d or 1w
    #[arg(long, value_name = "DURATION", value_parser = parse_age)]
    since: Option<std::time::Duration>,
    /// Show at most N exchanges (at least 1)
    #[arg(long, value_name = "N",
          value_parser = clap::builder::RangedU64ValueParser::<usize>::new().range(1..))]
    limit: Option<usize>,
    /// Print a JSON array instead of one line per exchange
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct ShowArgs {
    /// The exchange id (or a unique prefix of it)
    id: String,
    /// Print the stored JSON entry
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct ReplayArgs {
    /// The exchange id (or a unique prefix of it)
    id: String,
    /// Do not record the replayed exchange
    #[arg(long)]
    no_history: bool,
    #[command(flatten)]
    output: OutputFlags,
    #[command(flatten)]
    plugins: super::plugins::PluginFlags,
}

pub fn run(args: &HistoryArgs) -> Result<u8, CliError> {
    let history = History::open(args.workspace.as_deref())?;
    match &args.command {
        HistoryCommand::List(list) => history.list(list),
        HistoryCommand::Show(show) => history.show(show),
        HistoryCommand::Replay(replay) => history.replay(replay),
        HistoryCommand::Clear => history.clear(),
    }
}

/// The store, and the workspace it belongs to (`None`: ad-hoc history).
pub(super) struct History {
    pub ws: Option<Workspace>,
    pub store: HistoryStore,
}

impl History {
    pub(super) fn open(workspace: Option<&Path>) -> Result<Self, CliError> {
        let ws = match super::plugins::discover(&current_dir()?, workspace) {
            Ok(ws) => ws,
            Err(ConfigError::NoWorkspace) if workspace.is_none() => {
                let store = HistoryStore::global(&HistorySettings::default())?;
                return Ok(Self { ws: None, store });
            }
            Err(e) => return Err(e.into()),
        };
        print_warnings(&ws.warnings);
        let store = HistoryStore::for_workspace(&ws.root, &ws.settings.history);
        Ok(Self {
            ws: Some(ws),
            store,
        })
    }

    fn list(&self, args: &ListArgs) -> Result<u8, CliError> {
        let listing = self.store.list()?;
        print_warnings(&listing.warnings);
        let query = Query {
            status: args.status,
            method: args.method.clone(),
            grep: args.grep.clone(),
            since: args.since,
            limit: args.limit,
        };
        let entries = query.apply(listing.entries);
        let text = if args.json {
            let rows: Vec<Value> = entries.iter().map(summary).collect();
            format!("{:#}\n", Value::Array(rows))
        } else {
            entries.iter().map(|e| format!("{}\n", line(e))).collect()
        };
        write_stdout(&text);
        Ok(SUCCESS)
    }

    fn show(&self, args: &ShowArgs) -> Result<u8, CliError> {
        let entry = self.store.get(&args.id)?;
        let text = if args.json {
            let value = serde_json::to_value(&entry).unwrap_or(Value::Null);
            format!("{value:#}\n")
        } else {
            details(&entry)
        };
        write_stdout(&text);
        Ok(SUCCESS)
    }

    fn clear(&self) -> Result<u8, CliError> {
        let deleted = self.store.clear()?;
        let plural = if deleted == 1 { "" } else { "s" };
        println!("deleted {deleted} exchange{plural} from history");
        Ok(SUCCESS)
    }

    fn replay(self, args: &ReplayArgs) -> Result<u8, CliError> {
        let entry = self.store.get(&args.id)?;
        let Some(replay) = &entry.replay else {
            let message = format!("history entry {} holds no request to replay", entry.id);
            return Err(ConfigError::Usage(message).into());
        };
        let cwd = current_dir()?;
        let ad_hoc = Settings::ad_hoc();
        let (root, settings) = match &self.ws {
            Some(ws) => (ws.root.as_path(), &ws.settings),
            None => (cwd.as_path(), &ad_hoc),
        };
        let secrets = secret_store(root)?;
        let env_name = entry.environment.as_deref();
        let environment = self.ws.as_ref().and_then(|ws| {
            ws.environments
                .iter()
                .find(|e| Some(e.name.as_str()) == env_name)
        });
        let providers = secret_providers(root, settings)?;
        let runtime = runtime()?;
        // Resolving fails, before anything is sent, on a deleted secret or
        // a provider secret that cannot be fetched.
        let resolve = replay.resolve_for_send(environment, env_name, &secrets, Some(&providers));
        let resolved = runtime.block_on(resolve)?;
        // A request signed by a plugin is signed again: its workspace's
        // plugins load under the same trust rule as `run` and `send`.
        let plugins = match &self.ws {
            Some(ws) => super::plugins::headless(&ws.root, environment, &secrets, &args.plugins)?,
            None => None,
        };
        let record = settings.history.enabled && !args.no_history;
        let options = RunOptions {
            login: login_mode(),
            history: record.then_some(self.store),
            plugins,
            ..RunOptions::default()
        };
        let cancel = options.cancel.clone();
        let ctx = RunContext {
            root,
            settings,
            environment,
            secrets: &secrets,
            in_workspace: self.ws.is_some(),
        };
        let runner = Runner::new(ctx, options)?.with_secret_providers(providers);
        let sent = runtime.block_on(async {
            cancel_on_ctrl_c(&cancel);
            runner.replay(&entry, resolved).await
        });
        crate::commands::exec::finish_spans(&runner);
        if let Err(e) = runner.finish() {
            print_warnings(&[e.to_string()]);
        }
        print_warnings(&secrets.take_warnings());
        print_warnings(&sent.record.warnings);
        finish(&args.output, &sent)
    }
}

fn write_stdout(text: &str) {
    let mut out = std::io::stdout().lock();
    // A closed stdout (`| head`) is not an error worth reporting.
    let _ = out.write_all(text.as_bytes()).and_then(|()| out.flush());
}

/// The response status, or for an exchange without an HTTP status (a
/// plugin request kind) the recorded outcome; `error` when it errored.
fn status_text(entry: &Entry) -> String {
    if let Some(status) = entry.status {
        return status.to_string();
    }
    match entry.record.get("status").and_then(Value::as_str) {
        Some(outcome @ ("passed" | "failed" | "skipped")) => outcome.to_owned(),
        _ => "error".to_owned(),
    }
}

fn millis_text(entry: &Entry) -> String {
    entry
        .total_ms
        .map_or_else(|| "-".to_owned(), |ms| format!("{ms:.0} ms"))
}

/// One `history list` line: id, time, method, URL, status and total ms.
fn line(entry: &Entry) -> String {
    format!(
        "{}  {}  {} {}  {}  {}",
        entry.id,
        entry.timestamp,
        entry.sent.method,
        entry.sent.url,
        status_text(entry),
        millis_text(entry)
    )
}

/// One `history list --json` row.
fn summary(entry: &Entry) -> Value {
    json!({
        "id": entry.id,
        "timestamp": entry.timestamp,
        "request": entry.request,
        "environment": entry.environment,
        "method": entry.sent.method,
        "url": entry.sent.url,
        "status": entry.status,
        "total_ms": entry.total_ms,
        "error": entry.error,
    })
}

/// `history show`: the whole exchange as text.
fn details(entry: &Entry) -> String {
    let mut out = format!(
        "id: {}\ntime: {}\nrequest: {}\nenvironment: {}\n\n",
        entry.id,
        entry.timestamp,
        entry.request,
        entry.environment.as_deref().unwrap_or("-")
    );
    out.push_str(&format!("> {} {}\n", entry.sent.method, entry.sent.url));
    for header in &entry.sent.headers {
        out.push_str(&format!("> {}: {}\n", header.name, header.value));
    }
    if let Some(body) = &entry.sent.body {
        out.push_str(&format!(">\n{body}\n"));
    }
    out.push('\n');
    out.push_str(&response_text(entry));
    out.push_str(&outcome_text(entry));
    for line in entry.trace().iter().flat_map(super::trace::details) {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

fn response_text(entry: &Entry) -> String {
    let Some(response) = entry.record.pointer("/exchange/response") else {
        return String::new();
    };
    let field = |name: &str| response.get(name).map(display).unwrap_or_default();
    let mut out = format!(
        "< {} {} ({})\n",
        field("http_version"),
        status_text(entry),
        millis_text(entry)
    );
    for header in response["headers"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "< {}: {}\n",
            display(&header["name"]),
            display(&header["value"])
        ));
    }
    if let Some(body) = entry.response_body() {
        out.push_str(&format!("<\n{body}\n"));
    }
    if let Some(name) = &entry.spill {
        out.push_str(&format!(
            "(body truncated; the full body is in .rustman/bodies/{name})\n"
        ));
    }
    out
}

fn outcome_text(entry: &Entry) -> String {
    let mut out = String::new();
    for assertion in entry.record["assertions"].as_array().into_iter().flatten() {
        let message = display(&assertion["message"]);
        let detail = if message.is_empty() {
            String::new()
        } else {
            format!(" ({message})")
        };
        out.push_str(&format!(
            "{}: {}{detail}\n",
            display(&assertion["outcome"]),
            display(&assertion["label"])
        ));
    }
    if let Some(error) = &entry.error {
        out.push_str(&format!("error: {error}\n"));
    }
    out
}

fn display(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(status: Option<u16>, record: Value) -> Entry {
        serde_json::from_value(json!({
            "schema": rustman_core::history::ENTRY_SCHEMA,
            "id": "01JDQ3XG000000000000000000",
            "timestamp": "2026-11-17T09:30:00.123Z",
            "request": "order.rman.yaml",
            "environment": null,
            "sent": {"method": "SOAP", "url": "http://x/soap", "headers": [], "body": null},
            "status": status,
            "total_ms": 4.0,
            "error": record.get("error").cloned().unwrap_or(Value::Null),
            "spill": null,
            "record": record,
            "replay": null,
        }))
        .unwrap()
    }

    /// A plugin-kind exchange has no HTTP status: its line shows the
    /// recorded outcome, not `error`.
    #[test]
    fn exchanges_without_a_status_show_their_outcome() {
        let passed = entry(None, json!({"status": "passed"}));
        assert_eq!(status_text(&passed), "passed");
        assert!(line(&passed).contains("  passed  "), "{}", line(&passed));
        assert_eq!(
            status_text(&entry(None, json!({"status": "failed"}))),
            "failed"
        );
        let errored = entry(None, json!({"status": "errored", "error": "refused"}));
        assert_eq!(status_text(&errored), "error");
        assert_eq!(
            status_text(&entry(Some(201), json!({"status": "passed"}))),
            "201"
        );
    }
}
