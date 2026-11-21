//! Blockchain GraphQL presets (add-evm-toolkit "blockchain-graphql",
//! design D11) on the EVM side: display formatting of blockchain scalars,
//! links from results into the toolkit, and the chain head a subgraph's
//! freshness is measured against. Detection, scalar paths, pagination and
//! the `_meta` query live in `rustman-graphql`.

pub mod format;
pub mod links;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use rustman_core::vars::ResolvedRequest;
use rustman_graphql::ChainHead;
use rustman_http::SendOptions;
use rustman_jsonrpc::HttpSend;

use crate::query::block_number;
use crate::rpc::JsonrpcTransport;

pub use format::{Display, format_data};
pub use links::{Link, LinkAction, LinkKind, find_links};

/// Reads the head with `eth_blockNumber` through the JSON-RPC client, so
/// its guards and redaction apply.
pub struct RpcHead {
    sender: Arc<dyn HttpSend + Send + Sync>,
}

impl RpcHead {
    pub fn new(sender: Arc<dyn HttpSend + Send + Sync>) -> Self {
        Self { sender }
    }
}

impl std::fmt::Debug for RpcHead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RpcHead")
    }
}

impl ChainHead for RpcHead {
    fn head<'a>(
        &'a self,
        endpoint: &'a ResolvedRequest,
        options: &'a SendOptions,
    ) -> Pin<Box<dyn Future<Output = Result<u64, String>> + 'a>> {
        Box::pin(async move {
            let rpc = JsonrpcTransport::new(endpoint.clone(), self.sender.clone(), options.clone());
            block_number(&rpc).await.map_err(|e| e.to_string())
        })
    }
}
