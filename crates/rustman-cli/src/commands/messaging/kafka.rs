//! `rustman kafka produce|consume`: produce messages to a topic, or read
//! it from chosen partitions and offsets without joining a consumer group.

use clap::{Args, Subcommand};
use serde_json::{Map, Value, json};

use super::{Bounds, Common, Messages, Operation, messages, set};
use crate::exit::CliError;

#[derive(Debug, Args)]
#[command(after_help = "\
Examples:
  rustman kafka produce --brokers localhost:9092 --topic orders --key order-1 --value '{\"id\":1}'
  rustman kafka consume --brokers localhost:9092 --topic orders --offset earliest --max-messages 5 --json
  rustman kafka consume events/orders.rman.yaml --duration 10s")]
pub struct KafkaArgs {
    #[command(subcommand)]
    command: KafkaCommand,
}

#[derive(Debug, Subcommand)]
enum KafkaCommand {
    /// Produce messages (a kind: kafka request, or --value flags, or the
    /// lines of a piped stdin) and print their partitions and offsets
    Produce(ProduceArgs),
    /// Read messages from a topic and print them as they arrive
    Consume(ConsumeArgs),
}

/// Where the messages go or come from.
#[derive(Debug, Args)]
struct Target {
    /// Bootstrap brokers: host:port, comma-separated
    #[arg(long, value_name = "HOST:PORT")]
    brokers: Option<String>,
    /// The topic
    #[arg(long, value_name = "NAME")]
    topic: Option<String>,
}

#[derive(Debug, Args)]
struct ProduceArgs {
    #[command(flatten)]
    target: Target,
    /// Send every message to this partition (default: by key hash)
    #[arg(long, value_name = "N")]
    partition: Option<i32>,
    /// The key of every message
    #[arg(long, value_name = "TEXT")]
    key: Option<String>,
    #[command(flatten)]
    messages: Messages,
    #[command(flatten)]
    common: Common,
}

#[derive(Debug, Args)]
struct ConsumeArgs {
    #[command(flatten)]
    target: Target,
    /// Read this partition (repeatable; default: all)
    #[arg(long = "partition", value_name = "N")]
    partitions: Vec<i32>,
    /// Where to start: earliest, latest (default), an offset, or an RFC
    /// 3339 timestamp
    #[arg(long, value_name = "OFFSET")]
    offset: Option<String>,
    #[command(flatten)]
    bounds: Bounds,
    #[command(flatten)]
    common: Common,
}

pub fn run(args: &KafkaArgs) -> Result<u8, CliError> {
    match &args.command {
        KafkaCommand::Produce(args) => produce(args),
        KafkaCommand::Consume(args) => consume(args),
    }
}

fn produce(args: &ProduceArgs) -> Result<u8, CliError> {
    let mut fields = target_fields(&args.target);
    set(&mut fields, "partition", args.partition);
    if args.common.request.is_none() {
        fields.insert("operation".into(), json!("produce"));
        fields.insert(
            "messages".into(),
            messages(&args.messages, args.key.as_deref())?,
        );
    }
    let operation = Operation {
        kind: "kafka",
        common: &args.common,
        fields,
        bounds: None,
    };
    operation.run()
}

fn consume(args: &ConsumeArgs) -> Result<u8, CliError> {
    let mut fields = target_fields(&args.target);
    if !args.partitions.is_empty() {
        fields.insert("partitions".into(), json!(args.partitions));
    }
    set(&mut fields, "offset", args.offset.as_deref().map(offset));
    if args.common.request.is_none() {
        fields.insert("operation".into(), json!("consume"));
    }
    let operation = Operation {
        kind: "kafka",
        common: &args.common,
        fields,
        bounds: Some(&args.bounds),
    };
    operation.run()
}

fn target_fields(target: &Target) -> Map<String, Value> {
    let mut fields = Map::new();
    set(&mut fields, "brokers", target.brokers.as_deref());
    set(&mut fields, "topic", target.topic.as_deref());
    fields
}

/// `--offset` as the request-file field: a keyword, a number, or a
/// timestamp.
fn offset(text: &str) -> Value {
    match text {
        "earliest" | "latest" => json!(text),
        _ => text
            .parse::<i64>()
            .map_or_else(|_| json!({"timestamp": text}), |n| json!(n)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_are_keywords_numbers_or_timestamps() {
        assert_eq!(offset("earliest"), json!("earliest"));
        assert_eq!(offset("42"), json!(42));
        assert_eq!(
            offset("2026-11-17T09:00:00Z"),
            json!({"timestamp": "2026-11-17T09:00:00Z"})
        );
    }
}
