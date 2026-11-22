//! The known-chain registry (evm-toolkit "Known-chain registry"): name,
//! native currency and explorer per chain id, with `[evm.chains.<id>]`
//! overrides.

use std::collections::BTreeMap;

use crate::error::EvmError;
use crate::settings::EvmSettings;

/// The decimals of every built-in native currency.
pub const NATIVE_DECIMALS: u8 = 18;

/// The chain id of Anvil and Hardhat's local node.
pub const LOCAL_CHAIN_ID: u64 = 31_337;

/// What the registry knows about one chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainInfo {
    pub id: u64,
    pub name: String,
    /// The native currency symbol, such as `ETH`.
    pub symbol: String,
    pub decimals: u8,
    /// The block explorer's base URL.
    pub explorer: Option<String>,
    /// `false` for an id neither built in nor configured.
    pub known: bool,
}

impl ChainInfo {
    /// `Sepolia (11155111)`, or `chain 5` for an unknown id.
    pub fn label(&self) -> String {
        if self.known {
            format!("{} ({})", self.name, self.id)
        } else {
            self.name.clone()
        }
    }

    fn unknown(id: u64) -> Self {
        Self {
            id,
            name: format!("chain {id}"),
            symbol: "ETH".to_owned(),
            decimals: NATIVE_DECIMALS,
            explorer: None,
            known: false,
        }
    }
}

const BUILT_IN: &[(u64, &str, &str, Option<&str>)] = &[
    (1, "Ethereum Mainnet", "ETH", Some("https://etherscan.io")),
    (
        11_155_111,
        "Sepolia",
        "ETH",
        Some("https://sepolia.etherscan.io"),
    ),
    (
        17_000,
        "Holesky",
        "ETH",
        Some("https://holesky.etherscan.io"),
    ),
    (8453, "Base", "ETH", Some("https://basescan.org")),
    (
        10,
        "OP Mainnet",
        "ETH",
        Some("https://optimistic.etherscan.io"),
    ),
    (42_161, "Arbitrum One", "ETH", Some("https://arbiscan.io")),
    (137, "Polygon PoS", "POL", Some("https://polygonscan.com")),
    (LOCAL_CHAIN_ID, "Anvil/Hardhat local", "ETH", None),
];

/// The built-in chains that are test or local networks.
const TEST_CHAINS: [u64; 3] = [11_155_111, 17_000, LOCAL_CHAIN_ID];

/// Whether `chain_id` is a built-in mainnet, where value is real: every
/// built-in chain but the test and local ones.
pub fn is_built_in_mainnet(chain_id: u64) -> bool {
    BUILT_IN.iter().any(|&(id, ..)| id == chain_id) && !TEST_CHAINS.contains(&chain_id)
}

/// Chain ids to [`ChainInfo`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainRegistry {
    chains: BTreeMap<u64, ChainInfo>,
}

impl Default for ChainRegistry {
    fn default() -> Self {
        Self::built_in()
    }
}

impl ChainRegistry {
    /// The chains every build knows.
    pub fn built_in() -> Self {
        let chains = BUILT_IN
            .iter()
            .map(|&(id, name, symbol, explorer)| {
                let info = ChainInfo {
                    id,
                    name: name.to_owned(),
                    symbol: symbol.to_owned(),
                    decimals: NATIVE_DECIMALS,
                    explorer: explorer.map(str::to_owned),
                    known: true,
                };
                (id, info)
            })
            .collect();
        Self { chains }
    }

    /// The built-in chains with `[evm.chains.<id>]` applied on top.
    pub fn with_overrides(settings: &EvmSettings) -> Result<Self, EvmError> {
        let mut registry = Self::built_in();
        for (key, entry) in &settings.chains {
            let id: u64 = key.parse().map_err(|_| {
                EvmError::config(format!("[evm.chains.{key}]: the key must be a chain id"))
            })?;
            let mut info = registry.get(id);
            info.known = true;
            if let Some(name) = &entry.name {
                info.name.clone_from(name);
            } else if info.name.starts_with("chain ") {
                info.known = false;
            }
            if let Some(symbol) = &entry.symbol {
                info.symbol.clone_from(symbol);
            }
            if let Some(decimals) = entry.decimals {
                info.decimals = decimals;
            }
            if let Some(explorer) = &entry.explorer {
                info.explorer = Some(explorer.trim_end_matches('/').to_owned());
            }
            registry.chains.insert(id, info);
        }
        Ok(registry)
    }

    /// The chain `id`; `chain <id>` with symbol `ETH` when unknown.
    pub fn get(&self, id: u64) -> ChainInfo {
        self.chains
            .get(&id)
            .cloned()
            .unwrap_or_else(|| ChainInfo::unknown(id))
    }

    /// The chain named `name` (any case) or with id `name`.
    pub fn find(&self, name: &str) -> Option<ChainInfo> {
        if let Ok(id) = name.parse::<u64>() {
            return Some(self.get(id));
        }
        let wanted = name.trim().to_ascii_lowercase();
        let alias = match wanted.as_str() {
            "mainnet" | "ethereum" => "ethereum mainnet",
            "optimism" | "op" => "op mainnet",
            "arbitrum" => "arbitrum one",
            "polygon" => "polygon pos",
            "anvil" | "hardhat" | "local" => "anvil/hardhat local",
            other => other,
        };
        self.chains
            .values()
            .find(|c| c.name.to_ascii_lowercase() == alias)
            .cloned()
    }

    /// Every chain, by id.
    pub fn chains(&self) -> impl Iterator<Item = &ChainInfo> {
        self.chains.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(text: &str) -> EvmSettings {
        EvmSettings::from_table(&toml::from_str(text).unwrap()).unwrap()
    }

    #[test]
    fn the_built_in_chains_are_listed() {
        let registry = ChainRegistry::built_in();
        let labels: Vec<String> = registry.chains().map(ChainInfo::label).collect();
        assert_eq!(
            labels,
            [
                "Ethereum Mainnet (1)",
                "OP Mainnet (10)",
                "Polygon PoS (137)",
                "Base (8453)",
                "Holesky (17000)",
                "Anvil/Hardhat local (31337)",
                "Arbitrum One (42161)",
                "Sepolia (11155111)",
            ]
        );
        assert_eq!(registry.get(137).symbol, "POL");
        assert_eq!(
            registry.get(1).explorer.as_deref(),
            Some("https://etherscan.io")
        );
    }

    #[test]
    fn unknown_ids_are_chain_n_in_eth() {
        let info = ChainRegistry::built_in().get(5);
        assert_eq!(
            (info.label(), info.symbol.as_str()),
            ("chain 5".into(), "ETH")
        );
    }

    #[test]
    fn custom_chain_from_settings() {
        let registry = ChainRegistry::with_overrides(&settings(
            "[chains.999]\nname = \"My L2\"\nsymbol = \"MYL\"\n[chains.1]\nexplorer = \"https://eth.example/\"\n",
        ))
        .unwrap();
        let custom = registry.get(999);
        assert_eq!(
            (custom.label(), custom.symbol.as_str()),
            ("My L2 (999)".into(), "MYL")
        );
        let mainnet = registry.get(1);
        assert_eq!(mainnet.name, "Ethereum Mainnet");
        assert_eq!(mainnet.explorer.as_deref(), Some("https://eth.example"));
        let symbol_only =
            ChainRegistry::with_overrides(&settings("[chains.7]\nsymbol = \"SVN\"\n"))
                .unwrap()
                .get(7);
        assert_eq!(
            (symbol_only.label(), symbol_only.symbol.as_str()),
            ("chain 7".into(), "SVN")
        );
    }

    #[test]
    fn chains_are_found_by_name_alias_or_id() {
        let registry = ChainRegistry::built_in();
        assert_eq!(registry.find("mainnet").unwrap().id, 1);
        assert_eq!(registry.find("Sepolia").unwrap().id, 11_155_111);
        assert_eq!(registry.find("8453").unwrap().name, "Base");
        assert!(registry.find("solana").is_none());
    }
}
