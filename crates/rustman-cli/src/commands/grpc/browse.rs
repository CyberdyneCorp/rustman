//! `rustman grpc list <target|request>` and `rustman grpc describe
//! <target|request> <symbol>`: the services and methods of a target, and
//! one service, method (with a request template) or message. Without a
//! local schema they come from reflection, through the workspace's
//! descriptor cache; a cached answer prints its stale notice on stderr.

use clap::Args;
use rustman_grpc::{Schema, schema_for};

use super::{Reached, TargetFlags};
use crate::commands::exec::runtime;
use crate::commands::print_warnings;
use crate::exit::{CliError, SUCCESS};

#[derive(Debug, Args)]
pub struct ListArgs {
    /// A target (host:port, grpc://host:port, grpcs://host:port or a
    /// {{variable}}), or a grpc request file, its id or its name
    #[arg(value_name = "TARGET|REQUEST")]
    target: String,
    /// Ask the server again instead of using the descriptor cache
    #[arg(long)]
    refresh: bool,
    /// Print one JSON object per service
    #[arg(long)]
    json: bool,
    #[command(flatten)]
    flags: TargetFlags,
}

#[derive(Debug, Args)]
pub struct DescribeArgs {
    /// A target (host:port, grpc://host:port, grpcs://host:port or a
    /// {{variable}}), or a grpc request file, its id or its name
    #[arg(value_name = "TARGET|REQUEST")]
    target: String,
    /// A service, method (package.Service/Method) or message name
    #[arg(value_name = "SYMBOL")]
    symbol: String,
    /// Ask the server again instead of using the descriptor cache
    #[arg(long)]
    refresh: bool,
    /// Print the description as one JSON object
    #[arg(long)]
    json: bool,
    #[command(flatten)]
    flags: TargetFlags,
}

pub fn list(args: &ListArgs) -> Result<u8, CliError> {
    let schema = schema(&args.flags, &args.target, args.refresh)?;
    for service in schema.services() {
        if args.json {
            println!("{}", serde_json::to_string(&service).unwrap_or_default());
        } else {
            print!("{}", rustman_grpc::Description::Service(service));
        }
    }
    Ok(SUCCESS)
}

pub fn describe(args: &DescribeArgs) -> Result<u8, CliError> {
    let schema = schema(&args.flags, &args.target, args.refresh)?;
    let description = schema.describe(&args.symbol)?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string(&description).unwrap_or_default()
        );
    } else {
        print!("{description}");
    }
    Ok(SUCCESS)
}

/// The schema of `target`, with its stale notice printed.
fn schema(flags: &TargetFlags, target: &str, refresh: bool) -> Result<Schema, CliError> {
    let reached = Reached::open(flags, target)?;
    let cache = reached.session.cache();
    let found = runtime()?.block_on(schema_for(
        &reached.resolved,
        &reached.options,
        cache,
        refresh,
    ));
    let (schema, _) = found.map_err(|e| reached.redacted(e))?;
    if let Some(notice) = schema.notice() {
        print_warnings(&[notice]);
    }
    Ok(schema)
}
