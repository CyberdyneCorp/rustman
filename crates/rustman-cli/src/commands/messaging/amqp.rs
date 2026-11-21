//! `rustman amqp produce|consume`: publish to an exchange with a routing
//! key, or consume through a temporary queue bound to an exchange (an
//! existing queue only with --destructive or --requeue).

use clap::{Args, Subcommand};
use serde_json::{Map, Value, json};

use super::{Bounds, Common, Messages, Operation, messages, set};
use crate::exit::CliError;

#[derive(Debug, Args)]
#[command(after_help = "\
Examples:
  rustman amqp produce --broker amqp://localhost --exchange orders --routing-key order.created --value '{\"id\":1}'
  rustman amqp consume --broker amqp://localhost --exchange orders --binding-key 'order.*' --json
  rustman amqp consume --broker amqp://localhost --queue payments --requeue --max-messages 5")]
pub struct AmqpArgs {
    #[command(subcommand)]
    command: AmqpCommand,
}

#[derive(Debug, Subcommand)]
enum AmqpCommand {
    /// Publish messages (a kind: amqp request, or --value flags, or the
    /// lines of a piped stdin) with publisher confirms
    Produce(ProduceArgs),
    /// Consume messages and print them as they arrive
    Consume(ConsumeArgs),
}

#[derive(Debug, Args)]
struct ProduceArgs {
    /// The broker URL: amqp:// or amqps://
    #[arg(long, value_name = "URL")]
    broker: Option<String>,
    /// The exchange (default: the default exchange)
    #[arg(long, value_name = "NAME")]
    exchange: Option<String>,
    /// The routing key
    #[arg(long, value_name = "KEY")]
    routing_key: Option<String>,
    /// Fail when no queue is bound for the routing key
    #[arg(long)]
    mandatory: bool,
    #[command(flatten)]
    messages: Messages,
    #[command(flatten)]
    common: Common,
}

#[derive(Debug, Args)]
struct ConsumeArgs {
    /// The broker URL: amqp:// or amqps://
    #[arg(long, value_name = "URL")]
    broker: Option<String>,
    /// Bind a temporary queue to this exchange
    #[arg(long, value_name = "NAME")]
    exchange: Option<String>,
    /// The binding key of the temporary queue
    #[arg(long, value_name = "KEY")]
    binding_key: Option<String>,
    /// Consume from this existing queue instead (needs --destructive or
    /// --requeue)
    #[arg(long, value_name = "NAME")]
    queue: Option<String>,
    /// Accept that consuming from --queue removes its messages
    #[arg(long, requires = "queue")]
    destructive: bool,
    /// Put every message read from --queue back
    #[arg(long, requires = "queue")]
    requeue: bool,
    #[command(flatten)]
    bounds: Bounds,
    #[command(flatten)]
    common: Common,
}

pub fn run(args: &AmqpArgs) -> Result<u8, CliError> {
    match &args.command {
        AmqpCommand::Produce(args) => produce(args),
        AmqpCommand::Consume(args) => consume(args),
    }
}

fn produce(args: &ProduceArgs) -> Result<u8, CliError> {
    let mut fields = Map::new();
    set(&mut fields, "broker", args.broker.as_deref());
    set(&mut fields, "exchange", args.exchange.as_deref());
    set(&mut fields, "routing_key", args.routing_key.as_deref());
    set(&mut fields, "mandatory", args.mandatory.then_some(true));
    if args.common.request.is_none() {
        fields.insert("operation".into(), json!("produce"));
        fields.insert("messages".into(), messages(&args.messages, None)?);
    }
    let operation = Operation {
        kind: "amqp",
        common: &args.common,
        fields,
        bounds: None,
    };
    operation.run()
}

fn consume(args: &ConsumeArgs) -> Result<u8, CliError> {
    let mut fields: Map<String, Value> = Map::new();
    set(&mut fields, "broker", args.broker.as_deref());
    set(&mut fields, "exchange", args.exchange.as_deref());
    set(&mut fields, "binding_key", args.binding_key.as_deref());
    set(&mut fields, "queue", args.queue.as_deref());
    set(&mut fields, "destructive", args.destructive.then_some(true));
    set(&mut fields, "requeue", args.requeue.then_some(true));
    if args.common.request.is_none() {
        fields.insert("operation".into(), json!("consume"));
    }
    let operation = Operation {
        kind: "amqp",
        common: &args.common,
        fields,
        bounds: Some(&args.bounds),
    };
    operation.run()
}
