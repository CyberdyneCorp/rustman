//! `rustman evm decode --abi <abi> [--topic T]... <hex>`: decode call
//! data, revert data or (with topics) a log, offline: no node is called.

use clap::Args;
use rustman_core::text::printable;
use rustman_evm::EvmError;
use rustman_evm::abi::AbiCache;
use rustman_evm::codec::parse_bytes;
use rustman_evm::query::decode_offline;
use rustman_evm::stream::topic;

use super::offline_settings;
use crate::commands::EnvSelection;
use crate::exit::{CliError, SUCCESS};

#[derive(Debug, Args)]
pub struct DecodeArgs {
    /// Call data, revert data or log data, as 0x-prefixed hex
    #[arg(value_name = "HEX")]
    data: String,
    /// The ABI: erc20, erc721, erc1155, or a JSON ABI or Foundry/Hardhat
    /// artifact path from the workspace root
    #[arg(long, value_name = "ABI")]
    abi: String,
    /// A log topic (repeatable, topic 0 first): decode DATA as a log
    #[arg(long = "topic", value_name = "TOPIC")]
    topics: Vec<String>,
    /// Print one JSON object instead of text
    #[arg(long)]
    json: bool,
    #[command(flatten)]
    selection: EnvSelection,
}

pub fn run(args: &DecodeArgs) -> Result<u8, CliError> {
    let (_, root) = offline_settings(&args.selection)?;
    match decode(args, &root) {
        Ok(()) => Ok(SUCCESS),
        Err(error) => {
            eprintln!("error: {}", printable(&error.to_string()));
            Ok(error.exit_code())
        }
    }
}

fn decode(args: &DecodeArgs, root: &std::path::Path) -> Result<(), EvmError> {
    let anchor = std::path::Path::new(rustman_evm::adhoc::AD_HOC);
    let abi = AbiCache::new().load(root, anchor, &args.abi)?;
    let data = parse_bytes(&args.data)?;
    let topics = args
        .topics
        .iter()
        .map(|t| topic(t))
        .collect::<Result<Vec<_>, _>>()?;
    let decoded = decode_offline(&abi.abi, &data, &topics)?;
    if args.json {
        let shown = serde_json::to_string_pretty(&decoded.to_json()).unwrap_or_default();
        println!("{shown}");
    } else {
        println!("{}", printable(&decoded.render()));
    }
    Ok(())
}
