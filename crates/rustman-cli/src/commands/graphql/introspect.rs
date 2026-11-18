//! `rustman graphql introspect <endpoint|request> [--out schema.graphql]`:
//! fetch the schema, cache it in the workspace and optionally write SDL.

use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::Args;
use rustman_core::model::Request;
use rustman_core::run::now_rfc3339;
use rustman_core::settings::Settings;
use rustman_core::vars::{
    Layer, Resolved, ResolvedBody, ResolvedGraphql, ResolvedRequest, Resolver, VariablesSource,
    parse_overrides, resolve_graphql,
};
use rustman_core::{ConfigError, Workspace};
use rustman_graphql::{GraphqlError, SchemaCache, introspect, normalize_endpoint, refresh_schema};
use rustman_http::{HttpEngine, SendOptions};

use super::is_url;
use crate::commands::exec::{parse_duration, runtime, secret_store};
use crate::commands::{EnvSelection, current_dir, print_warnings};
use crate::exit::{CliError, SUCCESS};

#[derive(Debug, Args)]
pub struct IntrospectArgs {
    /// An endpoint URL, or a graphql request file, its id or its name
    #[arg(value_name = "ENDPOINT|REQUEST")]
    target: String,
    /// Also write the schema as SDL to FILE
    #[arg(long, value_name = "FILE")]
    out: Option<PathBuf>,
    #[command(flatten)]
    selection: EnvSelection,
    /// Set a variable, above every other source (repeatable): --var k=v
    #[arg(long = "var", value_name = "K=V")]
    vars: Vec<String>,
    /// Total timeout: 500ms, 5s, 2m, or a number of milliseconds
    #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
    timeout: Option<Duration>,
    /// Do not verify TLS certificates
    #[arg(long, short = 'k')]
    insecure: bool,
}

/// What the schema is fetched for: the resolved endpoint, and where the
/// workspace (and so the cache) is.
struct Target {
    resolved: ResolvedGraphql,
    root: Option<PathBuf>,
    settings: Settings,
}

pub fn run(args: &IntrospectArgs) -> Result<u8, CliError> {
    let target = target(args)?;
    let root = target.root.as_deref().unwrap_or(Path::new("."));
    let mut options = SendOptions::for_request(&target.settings, root, &target.resolved.http)?;
    if let Some(timeout) = args.timeout {
        options.timeout = timeout;
    }
    if args.insecure {
        options.tls.verify = false;
    }
    let engine = HttpEngine::new();
    let resolved = &target.resolved;
    let fetched = runtime()?.block_on(async {
        match &target.root {
            Some(root) => {
                let cache = SchemaCache::for_workspace(root);
                let cached = refresh_schema(&engine, &options, resolved, &cache).await?;
                Ok::<_, GraphqlError>(Fetched {
                    endpoint: cached.endpoint,
                    fetched_at: cached.fetched_at,
                    cached: Some(cached.path),
                    schema: cached.schema,
                })
            }
            None => {
                let introspected = introspect(&engine, &options, resolved).await?;
                let endpoint = normalize_endpoint(&resolved.http.url.text)?;
                Ok(Fetched {
                    endpoint: resolved.redactor().text(&endpoint).into_owned(),
                    fetched_at: now_rfc3339(),
                    cached: None,
                    schema: introspected.schema,
                })
            }
        }
    })?;
    report(&fetched, target.root.as_deref(), args.out.as_deref())
}

struct Fetched {
    endpoint: String,
    fetched_at: String,
    cached: Option<PathBuf>,
    schema: rustman_graphql::GraphqlSchema,
}

/// Print what was fetched and write `--out`.
fn report(fetched: &Fetched, root: Option<&Path>, out: Option<&Path>) -> Result<u8, CliError> {
    print_warnings(fetched.schema.warnings());
    println!(
        "introspected {}: {} types, fetched at {}",
        fetched.endpoint,
        fetched.schema.type_count(),
        fetched.fetched_at
    );
    match (&fetched.cached, root) {
        (Some(path), Some(root)) => {
            let shown = path.strip_prefix(root).unwrap_or(path);
            println!("cached in {}", shown.to_string_lossy().replace('\\', "/"));
        }
        _ => println!("not cached: no workspace"),
    }
    if let Some(out) = out {
        std::fs::write(out, fetched.schema.sdl()).map_err(|source| ConfigError::Io {
            path: out.to_path_buf(),
            source,
        })?;
        println!("wrote {}", out.display());
    }
    Ok(SUCCESS)
}

/// The endpoint to introspect: a URL as given (cached in the workspace
/// when there is one), or the endpoint of a graphql request resolved in
/// the active environment.
fn target(args: &IntrospectArgs) -> Result<Target, CliError> {
    let ws = workspace(args)?;
    if is_url(&args.target) {
        return Ok(Target {
            resolved: ad_hoc(&args.target),
            root: ws.as_ref().map(|w| w.root.clone()),
            settings: ws.map_or_else(Settings::ad_hoc, |w| w.settings),
        });
    }
    let ws = ws.ok_or(ConfigError::NoWorkspace)?;
    let resolved = resolve_request(args, &ws)?;
    Ok(Target {
        resolved,
        root: Some(ws.root.clone()),
        settings: ws.settings,
    })
}

/// The workspace, if any; outside one, only a URL can be introspected.
fn workspace(args: &IntrospectArgs) -> Result<Option<Workspace>, ConfigError> {
    let explicit = args.selection.workspace.is_some() || args.selection.env.is_some();
    match args.selection.workspace() {
        Ok(ws) => Ok(Some(ws)),
        Err(ConfigError::NoWorkspace) if !explicit && is_url(&args.target) => Ok(None),
        Err(e) => Err(e),
    }
}

/// The graphql request `args.target`, resolved with `--var` on top.
fn resolve_request(args: &IntrospectArgs, ws: &Workspace) -> Result<ResolvedGraphql, CliError> {
    let request = ws.find_request(&args.target, &current_dir()?)?;
    if !matches!(request.request(), Request::Graphql(_)) {
        let message = format!("request '{}' is not a graphql request", request.entry.id());
        return Err(ConfigError::Usage(message).into());
    }
    let env = args.selection.environment(ws)?;
    let store = secret_store(&ws.root)?;
    let overrides = parse_overrides(&args.vars).map_err(ConfigError::Usage)?;
    let mut resolver = Resolver::for_request(&ws.settings, &request, env, &store);
    resolver.push(VariablesSource::new(Layer::Override, &overrides));
    let resolved = resolve_graphql(&request, &resolver)?;
    print_warnings(&store.take_warnings());
    Ok(resolved)
}

/// An endpoint given on the command line, sent as is.
fn ad_hoc(url: &str) -> ResolvedGraphql {
    ResolvedGraphql {
        http: ResolvedRequest {
            method: "POST".to_owned(),
            url: Resolved::literal(url),
            query: Vec::new(),
            headers: Vec::new(),
            body: ResolvedBody::None,
            auth: None,
            settings: Default::default(),
            tls: Default::default(),
            secrets: Vec::new(),
        },
        operation_name: None,
        variables: Default::default(),
        connection_params: None,
    }
}
