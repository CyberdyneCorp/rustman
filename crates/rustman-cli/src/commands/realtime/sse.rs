//! `rustman sse listen <url|request>`: open a Server-Sent Events stream and
//! print its events.

use clap::{Args, Subcommand};
use serde_json::Map;

use super::Listen;
use crate::exit::CliError;

#[derive(Debug, Args)]
#[command(after_help = "\
Examples:
  rustman sse listen https://api.example.com/events --json --max-messages 3 | jq .data
  rustman sse listen {{events_url}} --header \"Authorization: Bearer {{token}}\"
  rustman sse listen streams/completions.rman.yaml --duration 1m")]
pub struct SseArgs {
    #[command(subcommand)]
    command: SseCommand,
}

#[derive(Debug, Subcommand)]
enum SseCommand {
    /// Open a Server-Sent Events stream (an http:// or https:// URL, or a
    /// kind: sse request) and print its events
    Listen(ListenArgs),
}

#[derive(Debug, Args)]
struct ListenArgs {
    #[command(flatten)]
    listen: Listen,
}

pub fn run(args: &SseArgs) -> Result<u8, CliError> {
    match &args.command {
        SseCommand::Listen(args) => super::run("sse", &args.listen, Map::new(), false),
    }
}
