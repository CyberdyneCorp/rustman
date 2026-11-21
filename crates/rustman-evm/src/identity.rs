//! Which chain and which node an endpoint is (evm-toolkit "Chain pinning
//! per environment", "Node client detection").
//!
//! [`Identities`] asks each endpoint once per session: `eth_chainId` first,
//! checked against the environment's pin before anything else is sent,
//! then `web3_clientVersion` (or `net_version` when that is missing).

use std::collections::HashMap;
use std::fmt;
use std::sync::{Mutex, PoisonError};

use serde::Serialize;
use serde_json::{Value, json};

use crate::chain::{ChainInfo, ChainRegistry};
use crate::error::EvmError;
use crate::quantity::parse_hex_u64;
use crate::rpc::{Rpc, call};

/// The node implementations told apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ClientKind {
    Geth,
    Reth,
    Erigon,
    Nethermind,
    Besu,
    Anvil,
    Hardhat,
    Unknown,
}

impl ClientKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Geth => "geth",
            Self::Reth => "reth",
            Self::Erigon => "erigon",
            Self::Nethermind => "nethermind",
            Self::Besu => "besu",
            Self::Anvil => "anvil",
            Self::Hardhat => "hardhat",
            Self::Unknown => "unknown",
        }
    }

    fn from_name(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "geth" => Self::Geth,
            "reth" => Self::Reth,
            "erigon" => Self::Erigon,
            "nethermind" => Self::Nethermind,
            "besu" => Self::Besu,
            "anvil" => Self::Anvil,
            "hardhatnetwork" | "hardhat" => Self::Hardhat,
            _ => Self::Unknown,
        }
    }
}

/// The node behind an endpoint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NodeClient {
    pub kind: ClientKind,
    /// `1.14.11` for `Geth/v1.14.11-stable/...`.
    pub version: Option<String>,
    /// `web3_clientVersion` as received.
    pub raw: Option<String>,
}

impl NodeClient {
    pub fn unknown() -> Self {
        Self {
            kind: ClientKind::Unknown,
            version: None,
            raw: None,
        }
    }

    /// Classify a `web3_clientVersion` answer.
    pub fn parse(client_version: &str) -> Self {
        let mut parts = client_version.split('/');
        let kind = ClientKind::from_name(parts.next().unwrap_or_default().trim());
        let version = (kind != ClientKind::Unknown)
            .then(|| parts.next().map(clean_version))
            .flatten()
            .filter(|v| !v.is_empty());
        Self {
            kind,
            version,
            raw: Some(client_version.to_owned()),
        }
    }
}

/// `v1.14.11-stable` and `v1.25.4+20b10b35` to `1.14.11` and `1.25.4`.
fn clean_version(raw: &str) -> String {
    let raw = raw.trim().trim_start_matches('v');
    let end = raw.find(['-', '+']).unwrap_or(raw.len());
    raw[..end].to_owned()
}

impl fmt::Display for NodeClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.version {
            Some(version) => write!(f, "{} {version}", self.kind.as_str()),
            None => f.write_str(self.kind.as_str()),
        }
    }
}

/// An environment's `chain_id`.
#[derive(Clone, Copy, Debug)]
pub struct Pin<'a> {
    pub environment: &'a str,
    pub chain_id: u64,
}

impl Pin<'_> {
    /// The chain-mismatch refusal when `actual` is not the pinned chain.
    pub fn check(&self, actual: u64, registry: &ChainRegistry) -> Result<(), EvmError> {
        if actual == self.chain_id {
            return Ok(());
        }
        let expected = registry.get(self.chain_id);
        let reported = registry.get(actual);
        Err(EvmError::Refused(format!(
            "chain mismatch: environment '{}' expects {} ({}), endpoint reports {} ({})",
            self.environment, expected.id, expected.name, reported.id, reported.name
        )))
    }
}

/// What an endpoint is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndpointIdentity {
    pub chain: ChainInfo,
    pub client: NodeClient,
    /// `net_version`, asked only when `web3_clientVersion` fails.
    pub network: Option<String>,
}

impl EndpointIdentity {
    /// `Base (8453) · geth 1.14.11`, for status lines.
    pub fn status(&self) -> String {
        format!("{} · {}", self.chain.label(), self.client)
    }

    /// The identity in `evm` command JSON output.
    pub fn to_json(&self) -> Value {
        json!({
            "chain_id": self.chain.id,
            "chain": self.chain.name,
            "client": self.client.kind,
            "client_version": self.client.version,
            "network": self.network,
        })
    }
}

/// The identities of the endpoints used this session.
#[derive(Debug, Default)]
pub struct Identities {
    known: Mutex<HashMap<String, EndpointIdentity>>,
}

impl Identities {
    pub fn new() -> Self {
        Self::default()
    }

    /// Identify `rpc` (once per session) and check `pin`. On a mismatch
    /// nothing but `eth_chainId` has been sent.
    pub async fn identify(
        &self,
        rpc: &dyn Rpc,
        registry: &ChainRegistry,
        pin: Option<Pin<'_>>,
    ) -> Result<EndpointIdentity, EvmError> {
        let key = rpc.endpoint();
        if let Some(known) = self.cached(&key) {
            pin.map_or(Ok(()), |p| p.check(known.chain.id, registry))?;
            return Ok(known);
        }
        let chain_id = chain_id(rpc).await?;
        pin.map_or(Ok(()), |p| p.check(chain_id, registry))?;
        let (client, network) = detect_client(rpc).await;
        let identity = EndpointIdentity {
            chain: registry.get(chain_id),
            client,
            network,
        };
        self.known
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, identity.clone());
        Ok(identity)
    }

    fn cached(&self, key: &str) -> Option<EndpointIdentity> {
        let known = self.known.lock().unwrap_or_else(PoisonError::into_inner);
        known.get(key).cloned()
    }
}

/// `eth_chainId` as an integer.
pub async fn chain_id(rpc: &dyn Rpc) -> Result<u64, EvmError> {
    let value = call(rpc, "eth_chainId", json!([])).await?;
    value
        .as_str()
        .and_then(|s| parse_hex_u64(s).ok())
        .ok_or_else(|| EvmError::Rpc(format!("eth_chainId: not a quantity: {value}")))
}

/// The node client, never an error: a node without `web3_clientVersion`
/// is `unknown`, and `net_version` then names its network.
async fn detect_client(rpc: &dyn Rpc) -> (NodeClient, Option<String>) {
    if let Ok(Value::String(version)) = rpc.call("web3_clientVersion", json!([])).await {
        return (NodeClient::parse(&version), None);
    }
    let network = match rpc.call("net_version", json!([])).await {
        Ok(Value::String(network)) => Some(network),
        Ok(Value::Number(network)) => Some(network.to_string()),
        _ => None,
    };
    (NodeClient::unknown(), network)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_versions_are_classified() {
        let cases = [
            ("Geth/v1.14.11-stable/linux-amd64/go1.23.2", "geth 1.14.11"),
            ("reth/v1.1.0-1ba631b/x86_64-unknown-linux-gnu", "reth 1.1.0"),
            ("erigon/2.60.10/linux-amd64/go1.22.8", "erigon 2.60.10"),
            (
                "Nethermind/v1.29.1+dfea5240/linux-x64/dotnet8.0.11",
                "nethermind 1.29.1",
            ),
            ("besu/v24.10.0/linux-x86_64/openjdk-java-21", "besu 24.10.0"),
            ("anvil/v0.2.0", "anvil 0.2.0"),
            (
                "HardhatNetwork/2.22.15/@ethereumjs/vm/7.0.0",
                "hardhat 2.22.15",
            ),
            ("MyNode/v9", "unknown"),
            ("", "unknown"),
        ];
        for (raw, shown) in cases {
            assert_eq!(NodeClient::parse(raw).to_string(), shown, "{raw}");
        }
    }

    #[test]
    fn the_mismatch_message_names_both_chains() {
        let pin = Pin {
            environment: "sepolia",
            chain_id: 11_155_111,
        };
        let err = pin.check(1, &ChainRegistry::built_in()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "chain mismatch: environment 'sepolia' expects 11155111 (Sepolia), endpoint reports 1 (Ethereum Mainnet)"
        );
        assert_eq!(err.exit_code(), 4);
        assert!(pin.check(11_155_111, &ChainRegistry::built_in()).is_ok());
    }
}
