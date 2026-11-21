//! `rustman evm block <number|tag|hash>`: a block's main fields.

use clap::Args;
use rustman_evm::call::BlockSel;
use rustman_evm::query::{BlockView, fetch_block};

use super::{DONE, Endpoint, with_node};
use crate::exit::CliError;

#[derive(Debug, Args)]
pub struct BlockArgs {
    /// A block number, a tag (latest, finalized, safe, earliest, pending)
    /// or a block hash
    #[arg(value_name = "BLOCK", default_value = "latest")]
    block: String,
    #[command(flatten)]
    endpoint: Endpoint,
}

pub fn run(args: &BlockArgs) -> Result<u8, CliError> {
    with_node(&args.endpoint, async |node| {
        let block = BlockSel::parse(&args.block)?;
        let raw = fetch_block(&node.rpc, &block).await?;
        let view = BlockView::of(&raw);
        node.print(&view.render(), view.to_json());
        DONE
    })
}
