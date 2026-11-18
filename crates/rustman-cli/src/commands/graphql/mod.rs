//! `rustman graphql introspect` and `rustman graphql query`
//! (add-graphql-client "GraphQL CLI commands"). The work is done by the
//! `rustman-graphql` crate; this module parses flags, finds the request and
//! prints the result.

mod introspect;
mod print;
mod query;

use clap::{Args, Subcommand};

use crate::exit::CliError;

#[derive(Debug, Args)]
pub struct GraphqlArgs {
    #[command(subcommand)]
    command: GraphqlCommand,
}

#[derive(Debug, Subcommand)]
enum GraphqlCommand {
    /// Fetch an endpoint's schema by introspection and cache it
    /// (optionally writing it as SDL)
    Introspect(introspect::IntrospectArgs),
    /// Validate, send and print a GraphQL operation: a request file, or
    /// --endpoint with --query
    Query(query::QueryArgs),
}

pub fn run(args: &GraphqlArgs) -> Result<u8, CliError> {
    match &args.command {
        GraphqlCommand::Introspect(args) => introspect::run(args),
        GraphqlCommand::Query(args) => query::run(args),
    }
}

/// Whether `target` is an endpoint URL rather than a request.
fn is_url(target: &str) -> bool {
    ["http://", "https://", "ws://", "wss://"]
        .iter()
        .any(|scheme| target.starts_with(scheme))
}
