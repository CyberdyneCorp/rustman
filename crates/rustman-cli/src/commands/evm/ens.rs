//! `rustman evm ens <name|address>`: forward resolution to a checksummed
//! address, or reverse resolution verified by a forward lookup.

use clap::Args;
use rustman_evm::EvmError;
use rustman_evm::call::parse_address;
use rustman_evm::ens::{Ens, ReverseName, ens_for, is_ens_name};
use rustman_evm::rpc::Rpc;
use serde_json::json;

use super::{DONE, Endpoint, Node, with_node};
use crate::exit::{CliError, SUCCESS};

#[derive(Debug, Args)]
pub struct EnsArgs {
    /// An ENS name (vitalik.eth) or an address
    #[arg(value_name = "NAME|ADDRESS")]
    target: String,
    #[command(flatten)]
    endpoint: Endpoint,
}

pub fn run(args: &EnsArgs) -> Result<u8, CliError> {
    with_node(&args.endpoint, async |node| {
        let configured = match &node.ens_rpc {
            Some(Ok(rpc)) => Some(rpc as &dyn Rpc),
            Some(Err(message)) => return Err(EvmError::config(message.clone())),
            None => None,
        };
        let ens = ens_for(&node.rpc, node.connected.identity.chain.id, configured)?;
        if is_ens_name(&args.target) {
            let address = ens.resolve(&args.target).await?.to_checksum(None);
            node.print(&address, json!({"name": args.target, "address": address}));
            return DONE;
        }
        reverse(node, &ens, &args.target).await
    })
}

async fn reverse(node: &Node<'_>, ens: &Ens<'_>, target: &str) -> Result<u8, EvmError> {
    let address = parse_address(target).map_err(|e| EvmError::config(format!("address: {e}")))?;
    let shown = address.to_checksum(None);
    let (text, name, verified) = match ens.reverse(address).await? {
        ReverseName::Verified(name) => (name.clone(), Some(name), true),
        ReverseName::Unverified(_) => (
            format!("{shown} has no verified primary ENS name"),
            None,
            false,
        ),
        ReverseName::None => (format!("{shown} has no primary ENS name"), None, false),
    };
    node.print(
        &text,
        json!({"address": shown, "name": name, "verified": verified}),
    );
    Ok(SUCCESS)
}
