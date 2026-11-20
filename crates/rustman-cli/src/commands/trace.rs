//! `rustman trace open`: the trace link of a history exchange, opened in
//! the system browser (trace-propagation spec "Showing trace ids").

use std::path::PathBuf;

use clap::{Args, Subcommand};
use rustman_core::browser::open_browser;
use rustman_core::history::Entry;
use rustman_core::model::Variables;
use rustman_core::settings::Settings;
use rustman_core::trace::{TraceLink, TraceRecord, WorkspaceScope};
use rustman_core::{ConfigError, Redactor};

use super::exec::secret_store;
use super::history::History;
use super::{current_dir, print_warnings};
use crate::exit::{CliError, SUCCESS};

/// The message (and exit code 2) when no link template is configured.
const NO_TEMPLATE: &str = "trace: no link template configured ([trace].link)";

#[derive(Debug, Args)]
pub struct TraceArgs {
    #[command(subcommand)]
    command: TraceCommand,
    /// Workspace directory (default: search from the current directory
    /// upwards; outside any workspace, the ad-hoc history)
    #[arg(long, value_name = "DIR", global = true)]
    workspace: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum TraceCommand {
    /// Open the trace link of a history exchange in the browser (and print it)
    Open(OpenArgs),
}

#[derive(Debug, Args)]
struct OpenArgs {
    /// The exchange id (or a unique prefix of it)
    id: String,
    /// Print the link without opening a browser
    #[arg(long)]
    no_browser: bool,
}

pub fn run(args: &TraceArgs) -> Result<u8, CliError> {
    match &args.command {
        TraceCommand::Open(open) => open_link(args.workspace.as_deref(), open),
    }
}

/// Print the link of exchange `args.id`, redacted, and open it.
fn open_link(workspace: Option<&std::path::Path>, args: &OpenArgs) -> Result<u8, CliError> {
    let history = History::open(workspace)?;
    let entry = history.store.get(&args.id)?;
    let (link, redactor) = configured_link(&history, &entry)?;
    let Some(trace) = entry.trace() else {
        let message = format!(
            "trace: history exchange {} carries no trace context",
            entry.id
        );
        return Err(ConfigError::Usage(message).into());
    };
    let url = trace.render(&link);
    println!("{}", redactor.text(&url));
    if !args.no_browser
        && let Err(e) = open_browser(&url)
    {
        print_warnings(&[format!("trace: cannot open a browser: {e}")]);
    }
    Ok(SUCCESS)
}

/// The link template configured for the entry's environment, now, with a
/// redactor of the secrets its variables used.
fn configured_link(history: &History, entry: &Entry) -> Result<(TraceLink, Redactor), CliError> {
    let ad_hoc = Settings::ad_hoc();
    let cwd = current_dir()?;
    let (root, settings) = match &history.ws {
        Some(ws) => (ws.root.clone(), &ws.settings),
        None => (cwd, &ad_hoc),
    };
    let environment = history.ws.as_ref().and_then(|ws| {
        ws.environments
            .iter()
            .find(|e| Some(e.name.as_str()) == entry.environment.as_deref())
    });
    let secrets = secret_store(&root)?;
    let overrides = Variables::default();
    let scope = WorkspaceScope {
        settings,
        environment,
        secrets: &secrets,
        overrides: &overrides,
    };
    let (link, used) = scope
        .link()
        .map_err(ConfigError::Usage)?
        .ok_or_else(|| ConfigError::Usage(NO_TEMPLATE.to_owned()))?;
    let mut redactor = Redactor::new();
    for secret in &used {
        redactor.add(secret);
    }
    Ok((link, redactor))
}

/// The trace id, the backend's own when it differs, and the link, as
/// `rustman send` prints them on stderr. Every field is already redacted.
pub fn lines(trace: &TraceRecord) -> Vec<String> {
    let mut lines = vec![format!("trace: {}", trace.trace_id)];
    if let Some(backend) = &trace.backend_trace_id {
        lines.push(format!("trace: backend {backend}"));
    }
    if let Some(link) = &trace.link {
        lines.push(format!("trace: {link}"));
    }
    lines
}

/// [`lines`] plus the `tracestate` and `baggage` sent, for `history show`.
pub fn details(trace: &TraceRecord) -> Vec<String> {
    let mut out = lines(trace);
    let sent = [
        ("tracestate", &trace.tracestate),
        ("baggage", &trace.baggage),
    ];
    for (name, value) in sent {
        if let Some(value) = value {
            out.push(format!("{name}: {value}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_show_ids_backend_link_and_sent_members() {
        let mut trace = TraceRecord {
            trace_id: "4bf92f3577b34da6a3ce929d0e0e4736".into(),
            span_id: "00f067aa0ba902b7".into(),
            sampled: true,
            backend_trace_id: None,
            tracestate: None,
            baggage: None,
            start_ms: 0,
            end_ms: 0,
            link: None,
        };
        assert_eq!(lines(&trace), ["trace: 4bf92f3577b34da6a3ce929d0e0e4736"]);
        trace.backend_trace_id = Some("0af7651916cd43dd8448eb211c80319c".into());
        trace.link = Some("http://j/trace/0af7651916cd43dd8448eb211c80319c".into());
        trace.baggage = Some("api_key=«redacted:api_key»".into());
        assert_eq!(
            details(&trace),
            [
                "trace: 4bf92f3577b34da6a3ce929d0e0e4736",
                "trace: backend 0af7651916cd43dd8448eb211c80319c",
                "trace: http://j/trace/0af7651916cd43dd8448eb211c80319c",
                "baggage: api_key=«redacted:api_key»",
            ]
        );
    }
}
