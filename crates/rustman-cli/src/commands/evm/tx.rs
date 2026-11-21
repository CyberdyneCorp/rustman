//! `rustman evm tx <hash>`: a transaction and its receipt, with input data
//! and logs decoded (built-in token ABIs), fee and status.

use clap::Args;
use rustman_evm::tx::{AbiMap, DecodeContext, TokenCache, fetch_tx};

use super::{DONE, Endpoint, with_node};
use crate::exit::CliError;

#[derive(Debug, Args)]
pub struct TxArgs {
    /// The transaction hash
    #[arg(value_name = "HASH")]
    hash: String,
    #[command(flatten)]
    endpoint: Endpoint,
}

pub fn run(args: &TxArgs) -> Result<u8, CliError> {
    with_node(&args.endpoint, async |node| {
        let abis = AbiMap::new();
        let tokens = TokenCache::new();
        let ctx = DecodeContext {
            rpc: &node.rpc,
            chain: node.connected.identity.chain.clone(),
            abis: &abis,
            tokens: &tokens,
        };
        let report = fetch_tx(&ctx, &args.hash).await?;
        for warning in &report.warnings {
            node.note(&format!("warning: {warning}"));
        }
        node.print(&report.render(), report.to_json());
        DONE
    })
}
