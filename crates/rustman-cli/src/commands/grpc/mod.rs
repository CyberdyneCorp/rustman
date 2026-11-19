//! `rustman grpc list`, `describe`, `call` and `health` (add-grpc-client
//! "CLI commands", "Health check helper"). The work is done by the
//! `rustman-grpc` crate: calls go through the runner, so they are
//! resolved, checked, recorded in history and redacted like any request;
//! this module parses flags, builds ad-hoc requests and prints results.

mod browse;
mod call;
mod health;

use clap::{Args, Subcommand};
use rustman_core::model::Request;
use rustman_core::vars::{Layer, ResolvedGrpc, Resolver, VariablesSource, resolve_grpc};
use rustman_core::{ConfigError, Environment};
use rustman_grpc::{DescriptorCache, GrpcError, GrpcTarget};
use rustman_http::SendOptions;

use crate::commands::exec::{overrides, secret_store};
use crate::commands::session::{Kind, Session};
use crate::commands::{EnvSelection, print_warnings};
use crate::exit::CliError;

#[derive(Debug, Args)]
#[command(after_help = "\
Examples:
  rustman grpc list grpc://localhost:50051
  rustman grpc list {{grpc_host}} --refresh
  rustman grpc describe grpc://localhost:50051 shop.v1.Catalog/ListProducts
  rustman grpc call grpc/get-product.rman.yaml
  rustman grpc call --target grpc://localhost:50051 --method shop.v1.Catalog/ListProducts --data '{}' --json
  rustman grpc health grpcs://api.example.com:443 --service shop.v1.Catalog")]
pub struct GrpcArgs {
    #[command(subcommand)]
    command: GrpcCommand,
}

#[derive(Debug, Subcommand)]
enum GrpcCommand {
    /// List the services and methods of a target or a grpc request file
    List(browse::ListArgs),
    /// Describe a service, method or message, with a request template
    Describe(browse::DescribeArgs),
    /// Call a method (a request file, or --target, --method and --data)
    /// and print the response
    Call(call::CallArgs),
    /// Check a server's health with grpc.health.v1.Health/Check
    Health(health::HealthArgs),
}

pub fn run(args: &GrpcArgs) -> Result<u8, CliError> {
    match &args.command {
        GrpcCommand::List(args) => browse::list(args),
        GrpcCommand::Describe(args) => browse::describe(args),
        GrpcCommand::Call(args) => call::run(args),
        GrpcCommand::Health(args) => health::run(args),
    }
}

/// Flags of the commands that reach a target without a run.
#[derive(Debug, Args)]
struct TargetFlags {
    #[command(flatten)]
    selection: EnvSelection,
    /// Set a variable, above every other source (repeatable): --var k=v
    #[arg(long = "var", value_name = "K=V")]
    vars: Vec<String>,
    /// Do not verify TLS certificates
    #[arg(long, short = 'k')]
    insecure: bool,
}

/// Whether `text` is a target (`host:port`, a `grpc://` or `grpcs://`
/// URL, or a `{{variable}}` holding one) rather than a request.
fn is_target(text: &str) -> bool {
    let url = ["grpc://", "grpcs://", "{{"]
        .iter()
        .any(|start| text.starts_with(start));
    url || (!text.ends_with(".rman.yaml") && GrpcTarget::parse(text).is_ok())
}

/// `kind: grpc` requests.
const GRPC: Kind = Kind {
    name: "grpc",
    matches: |request| matches!(request, Request::Grpc(_)),
};

impl Session {
    fn environment(&self, selection: &EnvSelection) -> Result<Option<&Environment>, ConfigError> {
        match &self.ws {
            Some(ws) => selection.environment(ws),
            None => Ok(None),
        }
    }

    /// The descriptor cache of the workspace; none outside one.
    fn cache(&self) -> Option<DescriptorCache> {
        self.ws
            .as_ref()
            .map(|ws| DescriptorCache::for_workspace(&ws.root))
    }
}

/// A target or request, resolved and ready to reach.
struct Reached {
    session: Session,
    resolved: ResolvedGrpc,
    options: SendOptions,
}

impl Reached {
    /// Resolve `text` (a target or a grpc request) with `flags`.
    fn open(flags: &TargetFlags, text: &str) -> Result<Self, CliError> {
        let session = Session::open(&flags.selection, is_target(text))?;
        let (resolved, options) = resolve(&session, flags, text)?;
        Ok(Self {
            session,
            resolved,
            options,
        })
    }

    /// `err` with every secret of the request replaced.
    fn redacted(&self, err: GrpcError) -> CliError {
        CliError::Grpc(err.redacted(&self.resolved.redactor()))
    }
}

/// Resolve `text` in `session`: the request and its transport options.
fn resolve(
    session: &Session,
    flags: &TargetFlags,
    text: &str,
) -> Result<(ResolvedGrpc, SendOptions), CliError> {
    let entry = is_target(text)
        .then(|| rustman_grpc::target_entry(text).map_err(invalid))
        .transpose()?;
    let request = match &entry {
        Some(entry) => session.ad_hoc(entry),
        None => session.find(text, &GRPC)?,
    };
    let env = session.environment(&flags.selection)?;
    let store = secret_store(session.root())?;
    let vars = overrides(&flags.vars)?;
    let mut resolver = Resolver::for_request(&session.settings, &request, env, &store);
    resolver.push(VariablesSource::new(Layer::Override, &vars));
    let resolved = resolve_grpc(&request, &resolver)?;
    let mut options =
        SendOptions::for_request(&session.settings, session.root(), &resolved.request)?;
    options.tls.verify &= !flags.insecure;
    print_warnings(&store.take_warnings());
    Ok((resolved, options))
}

fn invalid(err: rustman_core::LoadError) -> ConfigError {
    ConfigError::Invalid(vec![err])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_and_requests_are_told_apart() {
        for target in [
            "grpc://127.0.0.1:50051",
            "grpcs://api.example.com:443",
            "localhost:50051",
            "{{grpc_host}}",
        ] {
            assert!(is_target(target), "{target}");
        }
        for request in ["grpc/get.rman.yaml", "Get product", "grpc/get"] {
            assert!(!is_target(request), "{request}");
        }
    }
}
