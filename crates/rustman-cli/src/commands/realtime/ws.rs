//! `rustman ws connect <url|request>`: open a WebSocket session, send the
//! lines of a piped stdin as text frames and print what the server sends.

use clap::{Args, Subcommand};
use serde_json::{Map, json};

use super::Listen;
use crate::exit::CliError;

#[derive(Debug, Args)]
#[command(after_help = "\
Examples:
  rustman ws connect wss://echo.example.com --json --max-messages 2 < frames.txt
  rustman ws connect {{feed_url}} --header \"Authorization: Bearer {{token}}\" --subprotocol v2.feed
  rustman ws connect feeds/prices.rman.yaml --duration 10s")]
pub struct WsArgs {
    #[command(subcommand)]
    command: WsCommand,
}

#[derive(Debug, Subcommand)]
enum WsCommand {
    /// Open a WebSocket session (a ws:// or wss:// URL, or a kind: ws
    /// request), send stdin lines as text frames when stdin is not a
    /// terminal, and print the received messages
    Connect(ConnectArgs),
}

#[derive(Debug, Args)]
struct ConnectArgs {
    /// Offer a subprotocol (repeatable, in order of preference)
    #[arg(long = "subprotocol", value_name = "NAME")]
    subprotocols: Vec<String>,
    #[command(flatten)]
    listen: Listen,
}

pub fn run(args: &WsArgs) -> Result<u8, CliError> {
    match &args.command {
        WsCommand::Connect(args) => {
            let mut fields = Map::new();
            if !args.subprotocols.is_empty() {
                fields.insert("subprotocols".into(), json!(args.subprotocols));
            }
            super::run("ws", &args.listen, fields, true)
        }
    }
}
