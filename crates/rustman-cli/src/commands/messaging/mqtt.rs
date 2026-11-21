//! `rustman mqtt produce|consume`: publish messages to a topic, or
//! subscribe to a topic filter (`+` and `#` wildcards) with a clean
//! session.

use clap::{Args, Subcommand};
use serde_json::{Map, Value, json};

use super::{Bounds, Common, Messages, Operation, messages, set};
use crate::exit::CliError;

#[derive(Debug, Args)]
#[command(after_help = "\
Examples:
  printf '21.5\\n22.0\\n' | rustman mqtt produce --broker mqtt://localhost --topic sensors/room1/temp
  rustman mqtt consume --broker mqtts://broker.example --topic 'sensors/+/temp' --json
  rustman mqtt consume sensors/temps.rman.yaml --max-messages 3")]
pub struct MqttArgs {
    #[command(subcommand)]
    command: MqttCommand,
}

#[derive(Debug, Subcommand)]
enum MqttCommand {
    /// Publish messages (a kind: mqtt request, or --value flags, or the
    /// lines of a piped stdin)
    Produce(ProduceArgs),
    /// Subscribe to a topic filter and print messages as they arrive
    Consume(ConsumeArgs),
}

/// Where the messages go or come from.
#[derive(Debug, Args)]
struct Target {
    /// The broker URL: mqtt://, mqtts://, ws:// or wss://
    #[arg(long, value_name = "URL")]
    broker: Option<String>,
    /// The topic (a filter with + and # to consume)
    #[arg(long, value_name = "TOPIC")]
    topic: Option<String>,
    /// The quality of service: 0, 1 (default) or 2
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u8).range(0..=2))]
    qos: Option<u8>,
}

#[derive(Debug, Args)]
struct ProduceArgs {
    #[command(flatten)]
    target: Target,
    /// Ask the broker to retain the messages
    #[arg(long)]
    retain: bool,
    #[command(flatten)]
    messages: Messages,
    #[command(flatten)]
    common: Common,
}

#[derive(Debug, Args)]
struct ConsumeArgs {
    #[command(flatten)]
    target: Target,
    #[command(flatten)]
    bounds: Bounds,
    #[command(flatten)]
    common: Common,
}

pub fn run(args: &MqttArgs) -> Result<u8, CliError> {
    match &args.command {
        MqttCommand::Produce(args) => produce(args),
        MqttCommand::Consume(args) => consume(args),
    }
}

fn produce(args: &ProduceArgs) -> Result<u8, CliError> {
    let mut fields = target_fields(&args.target);
    set(&mut fields, "retain", args.retain.then_some(true));
    if args.common.request.is_none() {
        fields.insert("operation".into(), json!("produce"));
        fields.insert("messages".into(), messages(&args.messages, None)?);
    }
    let operation = Operation {
        kind: "mqtt",
        common: &args.common,
        fields,
        bounds: None,
    };
    operation.run()
}

fn consume(args: &ConsumeArgs) -> Result<u8, CliError> {
    let mut fields = target_fields(&args.target);
    if args.common.request.is_none() {
        fields.insert("operation".into(), json!("consume"));
    }
    let operation = Operation {
        kind: "mqtt",
        common: &args.common,
        fields,
        bounds: Some(&args.bounds),
    };
    operation.run()
}

fn target_fields(target: &Target) -> Map<String, Value> {
    let mut fields = Map::new();
    set(&mut fields, "broker", target.broker.as_deref());
    set(&mut fields, "topic", target.topic.as_deref());
    set(&mut fields, "qos", target.qos);
    fields
}
