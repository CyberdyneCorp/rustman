//! `rustman graphql query <request | --endpoint URL --query TEXT>`:
//! validate, send and print one operation through the runner (so it is
//! recorded in history and its assertions are checked). Blockchain presets
//! add `--paginate`, the display forms (`--raw`, `--units`) and a
//! subgraph's lag. Exit codes: 0 for
//! no errors, 1 when `errors` is not empty (or an assertion failed), and
//! the `cli-runner` codes when nothing came back.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use clap::Args;
use rustman_core::collection::RequestEntry;
use rustman_core::model::{GraphqlRequest, REQUEST_FILE_SUFFIX, Request};
use rustman_core::run::{RequestStatus, SkipReason};
use rustman_core::settings::Settings;
use rustman_core::workspace::RequestRef;
use rustman_core::{ConfigError, Workspace};
use rustman_evm::gql::{Display, RpcHead};
use rustman_graphql::{DEFAULT_MAX_PAGES, GraphqlRunner, MAX_PAGES_LIMIT, PageMode, Paginate};
use rustman_http::HttpEngine;
use rustman_http::runner::{RunContext, Runner, Sent};

use super::{is_url, print};
use crate::commands::exec::{
    SendFlags, cancel_on_ctrl_c, history_store, parse_duration, print_snapshot_notes, runtime,
    secret_store,
};
use crate::commands::{EnvSelection, current_dir, print_warnings};
use crate::exit::{ASSERTION, CliError, INTERRUPTED, REQUEST, SUCCESS};

/// The name ad-hoc operations are reported under.
const AD_HOC: &str = "ad-hoc";

#[derive(Debug, Args)]
pub struct QueryArgs {
    /// A graphql request file, its id or its name
    #[arg(
        value_name = "REQUEST",
        required_unless_present = "endpoint",
        conflicts_with = "endpoint"
    )]
    request: Option<String>,
    /// The endpoint of an ad-hoc operation (with --query; no workspace needed)
    #[arg(long, value_name = "URL", requires = "query")]
    endpoint: Option<String>,
    /// The document of an ad-hoc operation (with --endpoint)
    #[arg(long, value_name = "TEXT", requires = "endpoint")]
    query: Option<String>,
    /// Send even when the document fails validation
    #[arg(long)]
    no_validate: bool,
    /// Print the result as one JSON object
    #[arg(long)]
    json: bool,
    /// Subscriptions stop after N events (default: the request's
    /// max_events, else 10)
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    max_events: Option<u32>,
    /// Subscriptions stop after this long (default: the request's
    /// duration, else 30s)
    #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
    duration: Option<Duration>,
    /// Follow subgraph pagination and concatenate the pages: id_gt
    /// (default, `where: {id_gt: $var}`) or skip (`skip: $var`)
    #[arg(
        long,
        value_name = "MODE",
        num_args = 0..=1,
        default_missing_value = "id_gt",
        value_parser = ["id_gt", "skip"]
    )]
    paginate: Option<String>,
    /// Stop paginating after N pages (default 20, at most 1000)
    #[arg(
        long,
        value_name = "N",
        requires = "paginate",
        value_parser = clap::value_parser!(u32).range(1..=i64::from(MAX_PAGES_LIMIT))
    )]
    max_pages: Option<u32>,
    /// Show blockchain scalars as received, not formatted by the preset
    #[arg(long, conflicts_with = "units")]
    raw: bool,
    /// Also show BigInt values next to a `decimals` field in token units
    #[arg(long)]
    units: bool,
    #[command(flatten)]
    selection: EnvSelection,
    #[command(flatten)]
    flags: SendFlags,
}

pub fn run(args: &QueryArgs) -> Result<u8, CliError> {
    let cwd = current_dir()?;
    let ws = workspace(args)?;
    let Some(endpoint) = &args.endpoint else {
        let ws = ws.ok_or(ConfigError::NoWorkspace)?;
        let target = ws.find_request(args.request.as_deref().unwrap_or_default(), &cwd)?;
        if !matches!(target.request(), Request::Graphql(_)) {
            let message = format!("request '{}' is not a graphql request", target.entry.id());
            return Err(ConfigError::Usage(message).into());
        }
        return send(args, Some(&ws), &ws.settings, &target);
    };
    let entry = ad_hoc_entry(endpoint, args.query.as_deref().unwrap_or_default());
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

/// The workspace, if any; outside one, only ad-hoc operations can be sent.
fn workspace(args: &QueryArgs) -> Result<Option<Workspace>, ConfigError> {
    let explicit = args.selection.workspace.is_some() || args.selection.env.is_some();
    match args.selection.workspace() {
        Ok(ws) => Ok(Some(ws)),
        Err(ConfigError::NoWorkspace) if !explicit && args.endpoint.is_some() => Ok(None),
        Err(e) => Err(e),
    }
}

/// An ad-hoc graphql request: `document` POSTed to `endpoint`.
fn ad_hoc_entry(endpoint: &str, document: &str) -> RequestEntry {
    let transport = if is_url(endpoint) && endpoint.starts_with("ws") {
        "transport: ws\n"
    } else {
        ""
    };
    let text = format!(
        "version: 1\nkind: graphql\nname: {AD_HOC}\nendpoint: {}\ndocument: {}\n{transport}",
        json_string(endpoint),
        json_string(document)
    );
    let path = PathBuf::from(format!("{AD_HOC}{REQUEST_FILE_SUFFIX}"));
    let request = GraphqlRequest::parse(&path, &text).expect("an ad-hoc request always loads");
    RequestEntry {
        path,
        request: Request::Graphql(Box::new(request)),
    }
}

/// `text` as a double-quoted YAML scalar (JSON strings are valid YAML).
fn json_string(text: &str) -> String {
    serde_json::Value::String(text.to_owned()).to_string()
}

fn send(
    args: &QueryArgs,
    ws: Option<&Workspace>,
    settings: &Settings,
    target: &RequestRef<'_>,
) -> Result<u8, CliError> {
    let env = match ws {
        Some(ws) => args.selection.environment(ws)?,
        None => None,
    };
    let store = secret_store(target.root)?;
    let graphql = GraphqlRunner {
        max_events: args.max_events,
        duration: args.duration,
        validate: !args.no_validate,
        paginate: paginate(args),
        chain_head: Some(Arc::new(RpcHead::new(Arc::new(HttpEngine::new())))),
        freshness: true,
        ..GraphqlRunner::default()
    };
    let mut options = args.flags.options()?;
    options.history = history_store(args.flags.no_history, ws, settings);
    options.ad_hoc = args.endpoint.is_some();
    options.graphql = Some(&graphql);
    let cancel = options.cancel.clone();
    let ctx = RunContext {
        root: target.root,
        settings,
        environment: env,
        secrets: &store,
        in_workspace: ws.is_some(),
    };
    let providers = crate::commands::exec::secret_providers(target.root, settings)?;
    let runner = Runner::new(ctx, options)?.with_secret_providers(providers);
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
    finish(args, &sent, cancel.is_cancelled())
}

/// `--paginate [MODE]` with `--max-pages`.
fn paginate(args: &QueryArgs) -> Option<Paginate> {
    let mode = PageMode::parse(args.paginate.as_deref()?)?;
    Some(Paginate {
        mode,
        max_pages: args.max_pages.unwrap_or(DEFAULT_MAX_PAGES),
    })
}

/// How `data` is shown: `--raw`, `--units`, else formatted.
fn display(args: &QueryArgs) -> Display {
    match (args.raw, args.units) {
        (true, _) => Display::Raw,
        (false, true) => Display::Units,
        (false, false) => Display::Formatted,
    }
}

/// Print the result (or why there is none) and pick the exit code. A
/// subscription stopped by Ctrl+C prints its events and exits 130.
fn finish(args: &QueryArgs, sent: &Sent, interrupted: bool) -> Result<u8, CliError> {
    let record = &sent.record;
    if record.skip_reason == Some(SkipReason::Interrupted) {
        eprintln!("interrupted");
        return Ok(INTERRUPTED);
    }
    if let Some(error) = &record.error {
        eprintln!("error: {error}");
    }
    let Some(exchange) = &sent.exchange else {
        return Ok(REQUEST);
    };
    let shown = print::Shown::new(record, exchange, &sent.redactor, display(args));
    shown.write(args.json)?;
    for failed in record.assertions.iter().filter(|a| !a.passed()) {
        eprintln!("fail: {failed}");
    }
    if interrupted {
        eprintln!("interrupted");
        return Ok(INTERRUPTED);
    }
    let clean = shown.no_errors() && record.status == RequestStatus::Passed;
    Ok(if clean { SUCCESS } else { ASSERTION })
}
