//! `rustman evm probe <rpc-or-env>...` (add-evm-toolkit "chain-exploration"):
//! resolve each provider (a URL or `{{variable}}` under the active
//! environment, or an environment name whose `{{rpc}}` is used), build its
//! safety policy and governor, run the probe of `rustman-evm`, print the
//! summary, write `--report` renderings and stream `--viz` events.
//!
//! Exit codes: 0, 1 (a finding at or above `--fail-on`), 2 (providers on
//! different chains, bad flags), 3 (a provider cannot be reached), 4 (a
//! refusal: a write method in `--methods`, a chain pin, a production
//! host), 130 on Ctrl+C.

use std::num::NonZeroU32;
use std::time::Duration;

use clap::Args;
use rustman_core::environment::Environment;
use rustman_core::safety::{DEFAULT_MAX_DURATION, EnvironmentInfo, Limits, Origin};
use rustman_core::url_secrets::url_secrets;
use rustman_core::{ConfigError, Redactor};
use rustman_evm::probe::catalog::{MethodSelection, ZERO_ADDRESS};
use rustman_evm::probe::provider::{http_form, policy};
use rustman_evm::probe::report::render_text;
use rustman_evm::probe::{
    DEFAULT_BUDGET, DEFAULT_MAX_DRIFT, DEFAULT_MAX_RATE, ProbeCall, ProbeOptions, Provider,
    ProviderSpec, Timing, governed, probe,
};
use rustman_evm::{ChainRegistry, EvmSettings};
use rustman_http::runner::{RunContext, RunOptions, Runner};
use rustman_report::exploration::Severity;
use rustman_report::{Report, ReportTarget};
use serde_json::{Map, json};

use super::{finish, shown};
use crate::commands::EnvSelection;
use crate::commands::exec::{
    cancel_on_ctrl_c, overrides, parse_duration, runtime, secret_providers, secret_store,
};
use crate::commands::session::Session;
use crate::commands::viz::ChainProbeViz;
use crate::exit::{self, CliError};

#[derive(Debug, Args)]
#[command(after_help = "\
Examples:
  rustman evm probe https://rpc-a.example https://rpc-b.example
  rustman evm probe mainnet-alchemy mainnet-infura --methods all --report html=probe.html
  rustman evm probe wss://rpc-a.example --rate-discovery --max-rate 20
  rustman evm probe https://rpc-a.example --call 0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48:0x18160ddd --viz")]
pub struct ProbeArgs {
    /// Providers: URLs (http, https, ws, wss), {{variables}}, or names of
    /// environments whose {{rpc}} is probed
    #[arg(value_name = "RPC_OR_ENV", required = true)]
    providers: Vec<String>,
    #[command(flatten)]
    selection: EnvSelection,
    /// Methods to probe: standard, all, or a comma-separated list
    #[arg(long, value_name = "LIST", default_value = "standard")]
    methods: String,
    /// Block of the archive check (default 1000000, at most head - 10000)
    #[arg(long, value_name = "N")]
    block: Option<u64>,
    /// Requests per provider at most
    #[arg(long, value_name = "N", default_value_t = DEFAULT_BUDGET,
          value_parser = clap::value_parser!(u64).range(1..))]
    budget: u64,
    /// Render the report as json, junit, markdown, html or sarif, written
    /// to a file or stdout, as in html=probe.html (repeatable)
    #[arg(long = "report", value_name = "FORMAT[=PATH]")]
    report: Vec<ReportTarget>,
    /// Show the results in the graphical viz companion (rustman-viz)
    #[arg(long)]
    viz: bool,
    /// Search for each provider's rate limit with eth_blockNumber at 5,
    /// 10, 20 and 40 req/s
    #[arg(long)]
    rate_discovery: bool,
    /// Highest rate tried by --rate-discovery, in req/s
    #[arg(long, value_name = "N", default_value_t = NonZeroU32::new(DEFAULT_MAX_RATE).unwrap())]
    max_rate: NonZeroU32,
    /// Head drift (in blocks) above which a provider is a medium finding
    #[arg(long, value_name = "BLOCKS", default_value_t = DEFAULT_MAX_DRIFT)]
    max_drift: u64,
    /// Time between the 5 head samples of the drift check (default 2.5s:
    /// 10 s in all)
    #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
    drift_interval: Option<Duration>,
    /// An eth_call compared across providers and traced:
    /// <address>:<calldata>
    #[arg(long, value_name = "ADDRESS:CALLDATA")]
    call: Option<String>,
    /// Address of the balance and state checks (default: the zero address)
    #[arg(long, value_name = "ADDRESS")]
    address: Option<String>,
    /// Exit 1 when a finding is at or above this severity: critical, high,
    /// medium, low or info
    #[arg(long, value_name = "SEVERITY", default_value = "high")]
    fail_on: Severity,
    /// Requests per second per provider (default: [safety].rate, 10)
    #[arg(long, value_name = "N")]
    rate: Option<NonZeroU32>,
    /// Requests in flight per provider (default: [safety].concurrency, 4)
    #[arg(long, value_name = "N")]
    concurrency: Option<NonZeroU32>,
    /// Confirm a production host (repeatable)
    #[arg(long = "production", value_name = "HOST")]
    production: Vec<String>,
    /// Set a variable, above every other source (repeatable): --var k=v
    #[arg(long = "var", value_name = "K=V")]
    vars: Vec<String>,
    /// Total timeout per node call: 500ms, 5s, 2m, or milliseconds
    #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
    timeout: Option<Duration>,
    /// Do not verify TLS certificates
    #[arg(long, short = 'k')]
    insecure: bool,
}

/// Printed once when a provider URL is written with a literal API key.
const LITERAL_KEY_WARNING: &str = "a provider URL holds what looks like an API key: it is \
     hidden in the output, but keep it out of shell history and scripts by storing it as a \
     secret (`rustman secret set NAME`) and writing {{NAME}} or {{secret:<provider>/<path>}} \
     in the URL";

/// Warn (once) when a provider URL carries a literal key.
fn warn_of_literal_keys(providers: &[String]) {
    let literal = providers
        .iter()
        .flat_map(|p| url_secrets(p))
        .any(|key| !key.contains("{{"));
    if literal {
        crate::commands::print_warnings(&[LITERAL_KEY_WARNING.to_owned()]);
    }
}

/// Hide the credentials typed into `url` with `redactor`.
fn add_url_keys(redactor: &mut Redactor, url: &str) {
    for key in url_secrets(url) {
        redactor.add_value("url-key", key);
    }
}

/// Hides the credentials typed into `url`.
fn url_redactor(url: &str) -> Redactor {
    let mut redactor = Redactor::new();
    add_url_keys(&mut redactor, url);
    redactor
}

/// The endpoint of environment-named providers.
const ENV_RPC: &str = "{{rpc}}";

/// A provider argument resolved to an endpoint.
struct Resolved {
    endpoint: rustman_core::vars::ResolvedRequest,
    options: rustman_http::SendOptions,
    environment: Option<Environment>,
}

pub fn run(args: &ProbeArgs) -> Result<u8, CliError> {
    rustman_report::render::one_stdout(&args.report).map_err(usage)?;
    let methods = MethodSelection::parse(&args.methods);
    let call = args.call.as_deref().map(ProbeCall::parse).transpose();
    let (methods, call) = match (methods, call) {
        (Ok(methods), Ok(call)) => (methods, call),
        (Err(e), _) | (_, Err(e)) => return Ok(finish(Err(e), &Redactor::new())),
    };
    let ad_hoc = args.providers.iter().all(|p| p.contains("://"));
    let session = Session::open(&args.selection, ad_hoc)?;
    let active = match &session.ws {
        Some(ws) => args.selection.environment(ws)?.cloned(),
        None => None,
    };
    let store = secret_store(session.root())?;
    let settings = &session.settings;
    let limits = Limits {
        rate: args.rate.unwrap_or(settings.safety.rate),
        concurrency: args.concurrency.unwrap_or(settings.safety.concurrency),
        max_probes: args.budget,
        max_duration: DEFAULT_MAX_DURATION,
    };
    let evm =
        EvmSettings::from_table(&settings.evm).and_then(|s| ChainRegistry::with_overrides(&s));
    let registry = match evm {
        Ok(registry) => registry,
        Err(e) => return Ok(finish(Err(e), &Redactor::new())),
    };
    let options = ProbeOptions {
        methods,
        block: args.block,
        budget: args.budget,
        address: args
            .address
            .clone()
            .unwrap_or_else(|| ZERO_ADDRESS.to_owned()),
        call,
        rate_discovery: args.rate_discovery,
        max_rate: args.max_rate.get(),
        max_drift: args.max_drift,
        fail_on: args.fail_on,
        limits,
        timing: Timing {
            drift_interval: args
                .drift_interval
                .unwrap_or(Timing::default().drift_interval),
            ..Timing::default()
        },
        registry,
        environment: active.as_ref().map(|e| e.name.clone()),
        ..ProbeOptions::default()
    };
    let runtime = runtime()?;
    warn_of_literal_keys(&args.providers);
    let resolved = runtime.block_on(resolve_all(args, &session, active.as_ref(), &store))?;
    let mut redactor = resolved
        .iter()
        .flat_map(|r| &r.endpoint.secrets)
        .fold(Redactor::new(), |r, s| r.with(s));
    for provider in &args.providers {
        add_url_keys(&mut redactor, provider);
    }
    let providers = providers(args, &session, resolved, limits)?;
    let outcome = runtime.block_on(async {
        cancel_on_ctrl_c(&options.cancel);
        probe(&providers, &options).await
    });
    crate::commands::print_warnings(&store.take_warnings());
    let report = match outcome {
        Ok(report) => report,
        Err(_) if options.cancel.is_cancelled() => return Ok(exit::INTERRUPTED),
        Err(e) => return Ok(finish(Err(e), &redactor)),
    };
    finished(args, &session, report, &redactor)
}

/// Print, render and visualize the report; the exit code.
fn finished(
    args: &ProbeArgs,
    session: &Session,
    report: rustman_report::exploration::ExplorationReport,
    redactor: &Redactor,
) -> Result<u8, CliError> {
    let text = shown(redactor, &render_text(&report));
    // A report on stdout keeps it machine-readable.
    if args.report.iter().any(|t| t.path.is_none()) {
        eprint!("{text}");
    } else {
        print!("{text}");
    }
    if args.viz
        && let Some(viz) = ChainProbeViz::start("rustman evm probe", Some(session.root()))
    {
        viz.finish(&report);
    }
    let failing = report.failing(args.fail_on);
    crate::commands::report::write_all(&args.report, &Report::Exploration(Box::new(report)))?;
    Ok(if failing > 0 {
        exit::ASSERTION
    } else {
        exit::SUCCESS
    })
}

/// Resolve every provider argument, in order.
async fn resolve_all(
    args: &ProbeArgs,
    session: &Session,
    active: Option<&Environment>,
    store: &rustman_core::secrets::SecretStore,
) -> Result<Vec<Resolved>, CliError> {
    let mut resolved = Vec::new();
    for provider in &args.providers {
        let (url, environment) = target(provider, session, active)?;
        let ctx = RunContext {
            root: session.root(),
            settings: &session.settings,
            environment: environment.as_ref(),
            secrets: store,
            in_workspace: session.ws.is_some(),
        };
        let options = RunOptions {
            overrides: overrides(&args.vars)?,
            timeout: args.timeout,
            insecure: args.insecure,
            ..RunOptions::default()
        };
        let secret_providers = secret_providers(session.root(), &session.settings)?;
        let runner = Runner::new(ctx, options)?.with_secret_providers(secret_providers);
        let mut fields = Map::new();
        fields.insert("method".into(), json!("eth_chainId"));
        // The entry is synthetic: its file name and line mean nothing here.
        // Errors name the provider without the keys typed into it.
        let hide = url_redactor(provider);
        let entry = rustman_jsonrpc::ad_hoc_entry(&url, fields).map_err(|e| {
            let message = e.message.strip_prefix("jsonrpc: ").unwrap_or(&e.message);
            usage(hide.text(&format!("{provider}: {message}")).into_owned())
        })?;
        let request = session.ad_hoc(&entry);
        let (endpoint, options) = runner
            .resolve_evm_endpoint(&request, &url)
            .await
            .map_err(|e| usage(hide.text(&format!("{provider}: {e}")).into_owned()))?;
        resolved.push(Resolved {
            endpoint,
            options,
            environment,
        });
    }
    Ok(resolved)
}

/// The URL of `provider` and the environment it resolves under.
fn target(
    provider: &str,
    session: &Session,
    active: Option<&Environment>,
) -> Result<(String, Option<Environment>), CliError> {
    if provider.contains("://") || provider.contains("{{") {
        return Ok((provider.to_owned(), active.cloned()));
    }
    let ws = session.ws.as_ref().ok_or_else(|| {
        usage(format!(
            "'{provider}' is not a URL, and there is no workspace to find an environment of that name in"
        ))
    })?;
    let environment = ws.active_environment(Some(provider), None)?.cloned();
    Ok((ENV_RPC.to_owned(), environment))
}

/// The governed providers: each under its own safety policy.
fn providers(
    args: &ProbeArgs,
    session: &Session,
    resolved: Vec<Resolved>,
    limits: Limits,
) -> Result<Vec<Provider>, CliError> {
    let mut providers = Vec::new();
    for r in resolved {
        let url = r.endpoint.url.text.clone();
        let info = r
            .environment
            .as_ref()
            .map(EnvironmentInfo::from)
            .unwrap_or_default();
        let confirmed = production_for(&url, &args.production);
        let policy = policy(&url, &session.settings.safety, confirmed, info)?;
        providers.push(governed(ProviderSpec {
            endpoint: r.endpoint,
            options: r.options,
            environment: r.environment.as_ref().map(|e| e.name.clone()),
            pin: r.environment.as_ref().and_then(|e| e.chain_id),
            policy,
            limits,
            max_rate: args.max_rate,
        }));
    }
    Ok(providers)
}

/// The `--production` host naming `url`'s host, when one does.
fn production_for<'a>(url: &str, hosts: &'a [String]) -> Option<&'a str> {
    let origin = Origin::of(&http_form(url)).ok()?;
    hosts
        .iter()
        .find(|h| h.eq_ignore_ascii_case(origin.host()))
        .map(String::as_str)
}

fn usage(message: String) -> CliError {
    ConfigError::Usage(message).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_hosts_match_by_host() {
        let hosts = vec!["rpc.example".to_owned()];
        assert_eq!(
            production_for("wss://rpc.example/v3/k", &hosts),
            Some("rpc.example")
        );
        assert_eq!(production_for("https://other.example", &hosts), None);
    }
}
