//! `rustman evm logs`: stream new heads or logs (eth_subscribe over
//! WebSocket, eth_newFilter polling over HTTP) and print one decoded event
//! per line, reorged logs marked `REORGED`; or, with `--from-block`, query
//! logs over a range of at most 10,000 blocks.

use std::time::Duration;

use clap::Args;
use rustman_core::text::printable;
use rustman_evm::EvmError;
use rustman_evm::call::BlockSel;
use rustman_evm::query::{block_range, get_logs};
use rustman_evm::stream::{
    LogFilter, StreamConfig, StreamItem, StreamKind, StreamMode, head_line, item_channel, stream,
};
use rustman_evm::tx::{AbiMap, DecodeContext, TokenCache, log_line, log_view};
use serde_json::{Value, json};

use super::{Endpoint, Node, with_node};
use crate::commands::exec::parse_duration;
use crate::exit::{CliError, SUCCESS};

#[derive(Debug, Args)]
pub struct LogsArgs {
    /// Logs from this contract (repeatable; any contract when absent)
    #[arg(long = "address", value_name = "ADDRESS")]
    addresses: Vec<String>,
    /// Topic 0 as an event signature: "Transfer(address,address,uint256)"
    #[arg(long, value_name = "SIGNATURE")]
    event: Option<String>,
    /// A further topic, in order (32-byte hex, a signature, or * for any)
    #[arg(long = "topic", value_name = "TOPIC")]
    topics: Vec<String>,
    /// Stream new block headers instead of logs
    #[arg(long, conflicts_with_all = ["addresses", "event", "topics"])]
    heads: bool,
    /// Query logs from this block instead of streaming (with --to-block,
    /// default latest)
    #[arg(long, value_name = "BLOCK", conflicts_with = "heads")]
    from_block: Option<String>,
    /// The last block of a query
    #[arg(long, value_name = "BLOCK", requires = "from_block")]
    to_block: Option<String>,
    /// Stop streaming after N events (reorged logs not counted)
    #[arg(long, value_name = "N")]
    max_events: Option<u64>,
    /// Stop streaming after this long: 30s, 5m
    #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
    duration: Option<Duration>,
    /// How often to poll over HTTP (default 2s)
    #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
    poll_interval: Option<Duration>,
    #[command(flatten)]
    endpoint: Endpoint,
}

pub fn run(args: &LogsArgs) -> Result<u8, CliError> {
    with_node(&args.endpoint, async |node| {
        let filter = LogFilter::parse(&args.addresses, args.event.as_deref(), &args.topics)?;
        match &args.from_block {
            Some(from) => query(node, args, &filter, from).await,
            None => follow(node, args, filter).await,
        }
    })
}

async fn query(
    node: &Node<'_>,
    args: &LogsArgs,
    filter: &LogFilter,
    from: &str,
) -> Result<u8, EvmError> {
    let to = args.to_block.as_deref().unwrap_or("latest");
    let (from, to) = block_range(&node.rpc, &BlockSel::parse(from)?, &BlockSel::parse(to)?).await?;
    let logs = get_logs(&node.rpc, filter, from, to).await?;
    let printer = Printer::new(node);
    let mut lines = Vec::new();
    let mut views = Vec::new();
    for log in &logs {
        let (line, view) = printer.log(log).await?;
        lines.push(line);
        views.push(view);
    }
    let text = match lines.is_empty() {
        true => format!("no logs in blocks {from}..={to}"),
        false => lines.join("\n"),
    };
    node.print(
        &text,
        json!({"from_block": from, "to_block": to, "logs": views}),
    );
    Ok(SUCCESS)
}

async fn follow(node: &Node<'_>, args: &LogsArgs, filter: LogFilter) -> Result<u8, EvmError> {
    let kind = match args.heads {
        true => StreamKind::Heads,
        false => StreamKind::Logs(filter),
    };
    let mut config = StreamConfig {
        max_events: args.max_events,
        max_duration: args.duration,
        cancel: node.cancel.clone(),
        ..StreamConfig::default()
    };
    if let Some(interval) = args.poll_interval {
        config.poll_interval = interval;
    }
    let mode = StreamMode::of(&node.rpc.request().url.text, &config);
    if let Some(note) = mode.note() {
        node.note(&note);
    }
    let (sender, mut items) = item_channel();
    let streaming = async move {
        let summary = stream(&node.rpc, &kind, &config, &sender).await;
        drop(sender);
        summary
    };
    let printer = Printer::new(node);
    let printer = &printer;
    // Owns the receiver: when printing fails it is dropped, which ends the
    // stream instead of leaving it waiting on a full channel.
    let printing = async move {
        while let Some(item) = items.recv().await {
            printer.item(&item).await?;
        }
        Ok::<(), EvmError>(())
    };
    let (summary, printed) = tokio::join!(streaming, printing);
    printed?;
    let summary = summary?;
    node.note(&format!(
        "{} event(s), {} reorged",
        summary.events, summary.reorged
    ));
    Ok(SUCCESS)
}

/// Decodes and prints streamed items, one line each (one JSON object per
/// line with --json).
struct Printer<'n> {
    node: &'n Node<'n>,
    abis: AbiMap,
    tokens: TokenCache,
}

impl<'n> Printer<'n> {
    fn new(node: &'n Node<'n>) -> Self {
        Self {
            node,
            abis: AbiMap::new(),
            tokens: TokenCache::new(),
        }
    }

    async fn log(&self, log: &Value) -> Result<(String, Value), EvmError> {
        let ctx = DecodeContext {
            rpc: &self.node.rpc,
            chain: self.node.connected.identity.chain.clone(),
            abis: &self.abis,
            tokens: &self.tokens,
        };
        let mut warnings = Vec::new();
        let view = log_view(&ctx, log, &mut warnings).await?;
        for warning in warnings {
            self.node.note(&format!("warning: {warning}"));
        }
        Ok((log_line(&view), view.to_json()))
    }

    async fn item(&self, item: &StreamItem) -> Result<(), EvmError> {
        let (line, value) = match item {
            StreamItem::Head(head) => (head_line(head), head.clone()),
            StreamItem::Log(log) => self.log(log).await?,
        };
        let shown = match self.node.json {
            true => value.to_string(),
            false => line,
        };
        println!("{}", printable(&self.node.redactor.text(&shown)));
        Ok(())
    }
}
