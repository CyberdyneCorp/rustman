//! `rustman send`: execute one request (a request file, its id or its name,
//! or an ad-hoc `<METHOD> <url>`) and print the response.

use std::fs::File;
use std::io::{self, Write};
use std::path::PathBuf;

use clap::Args;
use rustman_core::collection::RequestEntry;
use rustman_core::model::{REQUEST_FILE_SUFFIX, Request};
use rustman_core::run::{RequestRecord, RequestStatus, SkipReason, Summary};
use rustman_core::settings::Settings;
use rustman_core::workspace::RequestRef;
use rustman_core::{ConfigError, Redactor, Workspace};
use rustman_http::Exchange;
use rustman_http::import_curl_words;
use rustman_http::runner::{RunContext, Runner, Sent};

use super::exec::{
    SendFlags, cancel_on_ctrl_c, history_store, print_snapshot_notes, runtime, secret_store,
};
use super::{EnvSelection, current_dir, print_warnings};
use crate::exit::{CliError, run_code};

/// The file name ad-hoc requests are reported under.
const AD_HOC: &str = "ad-hoc";

#[derive(Debug, Args)]
pub struct SendArgs {
    /// A request file, its id or its name; or, with URL, the method of an
    /// ad-hoc request. A lone http(s):// URL sends an ad-hoc GET.
    #[arg(value_name = "REQUEST|METHOD")]
    target: String,
    /// The URL of an ad-hoc request (no workspace needed)
    #[arg(value_name = "URL")]
    url: Option<String>,
    /// Ad-hoc request header (repeatable): -H 'Accept: application/json'
    #[arg(short = 'H', long = "header", value_name = "K: V")]
    headers: Vec<String>,
    /// Ad-hoc request body, sent as is
    #[arg(
        short = 'd',
        long = "data",
        value_name = "BODY",
        conflicts_with = "json"
    )]
    data: Option<String>,
    /// Ad-hoc JSON body (sets Content-Type and Accept to application/json)
    #[arg(long, value_name = "JSON")]
    json: Option<String>,
    #[command(flatten)]
    output: OutputFlags,
    #[command(flatten)]
    selection: EnvSelection,
    #[command(flatten)]
    flags: SendFlags,
}

/// Where the response goes, shared by `send` and `history replay`.
#[derive(Debug, Args)]
pub struct OutputFlags {
    /// Print only the response body, redacted (as received with
    /// --show-secrets)
    #[arg(long)]
    body_only: bool,
    /// Write the response body, byte for byte, to FILE instead of stdout
    #[arg(short = 'o', long = "output", value_name = "FILE")]
    output: Option<PathBuf>,
    /// Print secret values and credential headers as they are, on this
    /// terminal only (history and reports stay redacted)
    #[arg(long)]
    show_secrets: bool,
}

impl SendArgs {
    fn ad_hoc_url(&self) -> Option<&str> {
        let lone_url = ["http://", "https://", "{{"]
            .iter()
            .any(|prefix| self.target.starts_with(prefix));
        match &self.url {
            Some(url) => Some(url),
            None if lone_url => Some(&self.target),
            None => None,
        }
    }

    fn has_ad_hoc_parts(&self) -> bool {
        !self.headers.is_empty() || self.data.is_some() || self.json.is_some()
    }
}

pub fn run(args: &SendArgs) -> Result<u8, CliError> {
    let cwd = current_dir()?;
    let ws = workspace(args)?;
    let Some(url) = args.ad_hoc_url() else {
        if args.has_ad_hoc_parts() {
            let message =
                "-H, --data and --json need an ad-hoc request: rustman send <METHOD> <URL>";
            return Err(ConfigError::Usage(message.into()).into());
        }
        let ws = ws.ok_or(ConfigError::NoWorkspace)?;
        let target = ws.find_request(&args.target, &cwd)?;
        return send(args, Some(&ws), &ws.settings, &target);
    };
    let entry = ad_hoc_entry(args, url)?;
    let ad_hoc_settings = Settings::ad_hoc();
    let settings = ws.as_ref().map_or(&ad_hoc_settings, |w| &w.settings);
    let target = RequestRef {
        root: ws.as_ref().map_or(cwd.as_path(), |w| w.root.as_path()),
        http: &settings.http,
        entry: &entry,
        ancestors: Vec::new(),
    };
    send(args, ws.as_ref(), settings, &target)
}

/// The workspace, if any. Outside one, only ad-hoc requests without
/// `--env` or `--workspace` can be sent.
fn workspace(args: &SendArgs) -> Result<Option<Workspace>, ConfigError> {
    let explicit = args.selection.workspace.is_some() || args.selection.env.is_some();
    match args.selection.workspace() {
        Ok(ws) => Ok(Some(ws)),
        Err(ConfigError::NoWorkspace) if !explicit && args.ad_hoc_url().is_some() => Ok(None),
        Err(e) => Err(e),
    }
}

/// An ad-hoc request built the way `rustman import curl` reads the same flags.
fn ad_hoc_entry(args: &SendArgs, url: &str) -> Result<RequestEntry, CliError> {
    let method = if args.url.is_some() {
        args.target.to_ascii_uppercase()
    } else {
        "GET".to_owned()
    };
    let mut words = vec!["-X".to_owned(), method, url.to_owned()];
    for header in &args.headers {
        words.extend(["-H".to_owned(), header.clone()]);
    }
    if let Some(data) = &args.data {
        words.extend(["--data-raw".to_owned(), data.clone()]);
    }
    if let Some(json) = &args.json {
        words.extend(["--json".to_owned(), json.clone()]);
    }
    let imported = import_curl_words(&words)?;
    print_warnings(&imported.warnings);
    let mut request = imported.request;
    request.name = AD_HOC.to_owned();
    Ok(RequestEntry {
        path: PathBuf::from(format!("{AD_HOC}{REQUEST_FILE_SUFFIX}")),
        request: Request::Http(Box::new(request)),
    })
}

fn send(
    args: &SendArgs,
    ws: Option<&Workspace>,
    settings: &Settings,
    target: &RequestRef<'_>,
) -> Result<u8, CliError> {
    let env = match ws {
        Some(ws) => args.selection.environment(ws)?,
        None => None,
    };
    let store = secret_store(target.root)?;
    let mut options = args.flags.options()?;
    options.history = history_store(args.flags.no_history, ws, settings);
    options.ad_hoc = args.ad_hoc_url().is_some();
    let cancel = options.cancel.clone();
    let ctx = RunContext {
        root: target.root,
        settings,
        environment: env,
        secrets: &store,
        in_workspace: ws.is_some(),
    };
    let runner = Runner::new(ctx, options)?;
    let sent = runtime()?.block_on(async {
        cancel_on_ctrl_c(&cancel);
        runner.send(target, 1).await
    });
    crate::commands::exec::finish_spans(&runner);
    if let Err(e) = runner.finish() {
        print_warnings(&[e.to_string()]);
    }
    print_warnings(&store.take_warnings());
    print_warnings(&sent.record.warnings);
    print_snapshot_notes(&sent.record);
    finish(&args.output, &sent)
}

/// Print the response (or why there is none) and pick the exit code.
pub fn finish(args: &OutputFlags, sent: &Sent) -> Result<u8, CliError> {
    let record = &sent.record;
    if record.skip_reason == Some(SkipReason::UnsupportedKind) {
        let message = format!(
            "request '{}' is not an HTTP request; this build cannot send it",
            record.id
        );
        return Err(ConfigError::Usage(message).into());
    }
    if let Some(exchange) = &sent.exchange {
        print_response(args, record, exchange, &sent.redactor)?;
    }
    for line in record.trace.iter().flat_map(super::trace::lines) {
        eprintln!("{line}");
    }
    for reason in failures(record) {
        eprintln!("{reason}");
    }
    let summary = Summary::of(
        std::slice::from_ref(record),
        record.duration_ms,
        record.skip_reason == Some(SkipReason::Interrupted),
    );
    Ok(run_code(&summary))
}

/// Why the request did not pass, one line each, for stderr.
fn failures(record: &RequestRecord) -> Vec<String> {
    let mut lines: Vec<String> = record.error.iter().map(|e| format!("error: {e}")).collect();
    if record.status == RequestStatus::Failed {
        lines.extend(
            record
                .assertions
                .iter()
                .filter(|a| !a.passed())
                .map(|a| format!("fail: {a}")),
        );
    }
    lines
}

/// Print the response: redacted on stdout (a binary body byte-wise too),
/// unless `--show-secrets`; `-o` writes the body as received.
fn print_response(
    args: &OutputFlags,
    record: &RequestRecord,
    exchange: &Exchange,
    redactor: &Redactor,
) -> Result<(), ConfigError> {
    if let Some(path) = &args.output {
        let io = |source| ConfigError::Io {
            path: path.clone(),
            source,
        };
        let mut file = File::create(path).map_err(io)?;
        write_body(exchange, &mut file).map_err(io)?;
        eprintln!("wrote {} bytes to {}", exchange.size_bytes, path.display());
        return Ok(());
    }
    let mut out = io::stdout().lock();
    let written = match (args.body_only, args.show_secrets) {
        (true, true) => write_body(exchange, &mut out),
        (true, false) => write_redacted_body(exchange, redactor, &mut out),
        (false, true) => write_unredacted(exchange, &mut out),
        (false, false) => write_redacted(record, exchange, redactor, &mut out),
    };
    // A closed stdout (`| head`) is not an error worth reporting.
    let _ = written.and_then(|()| out.flush());
    Ok(())
}

/// The status line, the redacted headers, a blank line and the redacted
/// body (a binary one too: it is only shown, not saved).
fn write_redacted(
    record: &RequestRecord,
    exchange: &Exchange,
    redactor: &Redactor,
    out: &mut dyn Write,
) -> io::Result<()> {
    writeln!(out, "{} {}", exchange.http_version, exchange.status)?;
    let response = record.exchange.as_ref().map(|e| &e.response);
    for header in response.map(|r| r.headers.as_slice()).unwrap_or_default() {
        writeln!(out, "{}: {}", header.name, header.value)?;
    }
    writeln!(out)?;
    write_redacted_body(exchange, redactor, out)
}

/// The response body with every secret replaced, a spilled one streamed
/// from its file.
fn write_redacted_body(
    exchange: &Exchange,
    redactor: &Redactor,
    out: &mut dyn Write,
) -> io::Result<()> {
    match &exchange.spill {
        Some(path) => redactor.copy(File::open(path)?, out),
        None => out.write_all(&redactor.bytes(&exchange.body)),
    }
}

/// The status line, then the headers and body as received.
fn write_unredacted(exchange: &Exchange, out: &mut dyn Write) -> io::Result<()> {
    writeln!(out, "{} {}", exchange.http_version, exchange.status)?;
    for (name, value) in &exchange.headers {
        write!(out, "{name}: ")?;
        out.write_all(value.as_bytes())?;
        writeln!(out)?;
    }
    writeln!(out)?;
    write_body(exchange, out)
}

/// The response body exactly as received (decoded), including a body
/// spilled to disk.
fn write_body(exchange: &Exchange, out: &mut dyn Write) -> io::Result<()> {
    match &exchange.spill {
        Some(path) => io::copy(&mut File::open(path)?, out).map(|_| ()),
        None => out.write_all(&exchange.body),
    }
}
