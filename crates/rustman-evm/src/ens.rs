//! ENS names (evm-toolkit "ENS resolution"): forward resolution through
//! Ethereum Mainnet's registry, reverse resolution verified by a forward
//! lookup, and the choice of the mainnet endpoint (`[evm] ens_rpc`, else
//! the current endpoint when it is chain 1).
//!
//! Names are normalized by ASCII lowercasing; full ENSIP-15 normalization
//! of non-ASCII names is not done.

use alloy_dyn_abi::{DynSolType, DynSolValue};
use alloy_primitives::{Address, B256, address, hex, keccak256};
use serde_json::{Value, json};

use crate::codec::parse_bytes;
use crate::error::EvmError;
use crate::rpc::{Rpc, call};

/// The ENS registry, at the same address on mainnet and its testnets.
pub const ENS_REGISTRY: Address = address!("0x00000000000C2E074eC69A0dFb2997BA6C7d2e1e");

/// The error when a name needs resolving and no mainnet endpoint is known.
pub const ENS_NEEDS_MAINNET: &str = "ENS needs a mainnet RPC; set [evm] ens_rpc";

/// The chain id of Ethereum Mainnet, where the registry is read.
pub const MAINNET: u64 = 1;

/// `resolver(bytes32)`, `addr(bytes32)` and `name(bytes32)`.
const RESOLVER: [u8; 4] = [0x01, 0x78, 0xb8, 0xbf];
const ADDR: [u8; 4] = [0x3b, 0x3b, 0x57, 0xde];
const NAME: [u8; 4] = [0x69, 0x1f, 0x34, 0x31];

/// Whether `input` is an ENS name rather than an address: it has a dot,
/// no spaces, and no `0x` prefix.
pub fn is_ens_name(input: &str) -> bool {
    let input = input.trim();
    input.contains('.')
        && !input.starts_with("0x")
        && !input.contains(char::is_whitespace)
        && !input.starts_with('.')
        && !input.ends_with('.')
}

/// The EIP-137 namehash of `name` (lowercased).
pub fn namehash(name: &str) -> B256 {
    let name = name.trim().to_lowercase();
    let mut node = B256::ZERO;
    if name.is_empty() {
        return node;
    }
    for label in name.rsplit('.') {
        let mut joined = [0u8; 64];
        joined[..32].copy_from_slice(node.as_slice());
        joined[32..].copy_from_slice(keccak256(label.as_bytes()).as_slice());
        node = keccak256(joined);
    }
    node
}

/// The endpoint to read ENS from: the configured `ens_rpc`, else the
/// current endpoint when it is mainnet.
pub fn ens_endpoint<'a>(
    current: &'a dyn Rpc,
    current_chain: u64,
    configured: Option<&'a dyn Rpc>,
) -> Result<&'a dyn Rpc, EvmError> {
    match configured {
        Some(rpc) => Ok(rpc),
        None if current_chain == MAINNET => Ok(current),
        None => Err(EvmError::config(ENS_NEEDS_MAINNET)),
    }
}

/// The ENS reader for a call to `current` (on `current_chain`): the
/// configured `ens_rpc`, whose chain id is checked before its first read,
/// else the current endpoint when it is mainnet.
pub fn ens_for<'a>(
    current: &'a dyn Rpc,
    current_chain: u64,
    configured: Option<&'a dyn Rpc>,
) -> Result<Ens<'a>, EvmError> {
    let rpc = ens_endpoint(current, current_chain, configured)?;
    Ok(match configured {
        Some(_) => Ens::checked(rpc),
        None => Ens::new(rpc),
    })
}

/// The refusal of an `ens_rpc` endpoint on another chain (exit code 4).
pub fn ens_rpc_not_mainnet(chain_id: u64) -> String {
    format!(
        "[evm] ens_rpc reports chain {chain_id}, not Ethereum Mainnet (1); ENS is read on mainnet only"
    )
}

/// What a reverse lookup found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReverseName {
    /// The primary name, confirmed by resolving it back to the address.
    Verified(String),
    /// A name the reverse record claims but whose forward lookup gives
    /// another address (or none): not shown as the address's name.
    Unverified(String),
    /// No reverse record.
    None,
}

/// ENS reads against a mainnet endpoint.
pub struct Ens<'a> {
    rpc: &'a dyn Rpc,
    /// Set for an endpoint not known to be mainnet: its chain id, checked
    /// once before the first read.
    mainnet_check: Option<tokio::sync::OnceCell<Result<(), EvmError>>>,
}

impl<'a> Ens<'a> {
    /// Reads against `rpc`, already known to be mainnet.
    pub fn new(rpc: &'a dyn Rpc) -> Self {
        Self {
            rpc,
            mainnet_check: None,
        }
    }

    /// Reads against `rpc` (a configured `ens_rpc`), refused unless its
    /// `eth_chainId` is 1: the registry has the same address on every
    /// chain, so another chain would silently give other addresses.
    pub fn checked(rpc: &'a dyn Rpc) -> Self {
        Self {
            rpc,
            mainnet_check: Some(tokio::sync::OnceCell::new()),
        }
    }

    async fn ensure_mainnet(&self) -> Result<(), EvmError> {
        let Some(check) = &self.mainnet_check else {
            return Ok(());
        };
        let checked = check
            .get_or_init(|| async {
                match crate::identity::chain_id(self.rpc).await? {
                    MAINNET => Ok(()),
                    other => Err(EvmError::Refused(ens_rpc_not_mainnet(other))),
                }
            })
            .await;
        checked.clone()
    }

    /// The address `name` resolves to.
    pub async fn resolve(&self, name: &str) -> Result<Address, EvmError> {
        let node = namehash(name);
        let resolver = self
            .resolver(node)
            .await?
            .ok_or_else(|| EvmError::NotFound(format!("ENS name '{name}' has no resolver")))?;
        let data = self.read(resolver, ADDR, node).await?;
        let resolved = decode_address(&data)?;
        if resolved == Address::ZERO {
            return Err(EvmError::NotFound(format!(
                "ENS name '{name}' does not resolve to an address"
            )));
        }
        Ok(resolved)
    }

    /// The primary name of `address`, verified by a forward lookup.
    pub async fn reverse(&self, address: Address) -> Result<ReverseName, EvmError> {
        let reverse = format!("{}.addr.reverse", hex::encode(address));
        let node = namehash(&reverse);
        let Some(resolver) = self.resolver(node).await? else {
            return Ok(ReverseName::None);
        };
        let data = self.read(resolver, NAME, node).await?;
        let name = match DynSolType::String.abi_decode(&data) {
            Ok(DynSolValue::String(name)) if !name.is_empty() => name,
            _ => return Ok(ReverseName::None),
        };
        match self.resolve(&name).await {
            Ok(forward) if forward == address => Ok(ReverseName::Verified(name)),
            Ok(_) | Err(EvmError::NotFound(_)) => Ok(ReverseName::Unverified(name)),
            Err(other) => Err(other),
        }
    }

    /// `input` as an address: hex as is, an ENS name resolved.
    pub async fn address(&self, input: &str) -> Result<Address, EvmError> {
        if is_ens_name(input) {
            return self.resolve(input).await;
        }
        input
            .trim()
            .parse()
            .map_err(|_| EvmError::config(format!("'{input}' is not an address or ENS name")))
    }

    async fn resolver(&self, node: B256) -> Result<Option<Address>, EvmError> {
        let data = self.read(ENS_REGISTRY, RESOLVER, node).await?;
        let resolver = decode_address(&data)?;
        Ok((resolver != Address::ZERO).then_some(resolver))
    }

    async fn read(&self, to: Address, selector: [u8; 4], node: B256) -> Result<Vec<u8>, EvmError> {
        self.ensure_mainnet().await?;
        let mut data = selector.to_vec();
        data.extend_from_slice(node.as_slice());
        let params = json!([
            {"to": to.to_checksum(None), "data": hex::encode_prefixed(&data)},
            "latest"
        ]);
        let value = call(self.rpc, "eth_call", params).await?;
        parse_bytes(value.as_str().unwrap_or("0x"))
    }
}

/// An `eth_call` result holding one address; empty data reads as zero
/// (no contract there).
fn decode_address(data: &[u8]) -> Result<Address, EvmError> {
    if data.is_empty() {
        return Ok(Address::ZERO);
    }
    match DynSolType::Address.abi_decode(data) {
        Ok(DynSolValue::Address(address)) => Ok(address),
        _ => Err(EvmError::Rpc(format!(
            "ENS: not an address: {}",
            hex::encode_prefixed(data)
        ))),
    }
}

/// Resolve the ENS names among `args` for `address` parameters (top level
/// only), so they can be encoded.
pub async fn resolve_args(
    ens: Option<&Ens<'_>>,
    inputs: &[alloy_json_abi::Param],
    args: &[Value],
) -> Result<Vec<Value>, EvmError> {
    let mut resolved = Vec::with_capacity(args.len());
    for (param, arg) in inputs.iter().zip(args) {
        let name = arg
            .as_str()
            .filter(|s| param.ty == "address" && is_ens_name(s));
        let value = match (name, ens) {
            (Some(name), Some(ens)) => Value::String(ens.resolve(name).await?.to_checksum(None)),
            (Some(_), None) => return Err(EvmError::config(ENS_NEEDS_MAINNET)),
            (None, _) => arg.clone(),
        };
        resolved.push(value);
    }
    resolved.extend(args.iter().skip(inputs.len()).cloned());
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namehashes_match_eip_137() {
        assert_eq!(namehash(""), B256::ZERO);
        assert_eq!(
            namehash("eth").to_string(),
            "0x93cdeb708b7545dc668eb9280176169d1c33cfd8ed6f04690a0bcc88a93fc4ae"
        );
        assert_eq!(
            namehash("vitalik.eth").to_string(),
            "0xee6c4522aab0003e8d14cd40a6af439055fd2577951148c14b6cea9a53475835"
        );
        assert_eq!(namehash("Vitalik.ETH"), namehash("vitalik.eth"));
    }

    #[test]
    fn names_are_told_from_addresses() {
        assert!(is_ens_name("vitalik.eth") && is_ens_name("a.b.c"));
        for not in [
            "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045",
            "eth",
            "a b.eth",
            ".eth",
        ] {
            assert!(!is_ens_name(not), "{not}");
        }
    }
}
