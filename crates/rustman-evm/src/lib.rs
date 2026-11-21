//! The EVM toolkit for Rustman (add-evm-toolkit): chain identity and the
//! known-chain registry, unit-aware quantities, ABI loading (built-in token
//! standards, JSON ABIs, Foundry and Hardhat artifacts, opt-in explorer
//! fetch), encoding and decoding of calls, outputs, logs and reverts, and
//! transaction and receipt decoding; read-only calls with state overrides
//! and gas estimates, call traces, head and log streams, ENS resolution,
//! local Anvil forks, and the send-method blocklist.
//!
//! Every node call goes through [`rpc::Rpc`]; the real transport is
//! `rustman-jsonrpc` ([`rpc::JsonrpcTransport`]), so the JSON-RPC client's
//! guards, settings and redaction apply. Nothing in this crate signs or
//! sends a transaction.

pub mod abi;
pub mod adhoc;
pub mod assert;
pub mod blocklist;
pub mod call;
pub mod catalog;
pub mod chain;
pub mod codec;
pub mod ens;
pub mod error;
pub mod events;
pub mod exec;
pub mod explorer;
pub mod fork;
pub mod gql;
pub mod identity;
pub mod load;
pub mod probe;
pub mod quantity;
pub mod query;
#[cfg(any(test, feature = "test-fixtures"))]
pub mod recorded;
pub mod rpc;
pub mod settings;
pub mod stream;
pub mod trace;
pub mod tx;

/// Install the toolkit's process-wide hooks, once, before any workspace is
/// loaded or request sent: the send-method guard, the `@evm-read` preset,
/// the typed-param check of catalog methods, and the load-time check of
/// `evm-call`/`evm-tx` files.
pub fn install() {
    blocklist::install();
    catalog::install();
    rustman_core::workspace::register_workspace_check(load::workspace_check);
}

/// The ABI types front ends read (functions, their inputs, mutability).
pub use alloy_json_abi as json_abi;
pub use chain::{ChainInfo, ChainRegistry, LOCAL_CHAIN_ID};
pub use error::EvmError;
pub use identity::{ClientKind, EndpointIdentity, Identities, NodeClient, Pin};
pub use quantity::{Denomination, Form, Quantity, QuantityError};
#[cfg(any(test, feature = "test-fixtures"))]
pub use recorded::Recorded;
pub use rpc::{JsonrpcTransport, Rpc, RpcFailure};
pub use settings::EvmSettings;
