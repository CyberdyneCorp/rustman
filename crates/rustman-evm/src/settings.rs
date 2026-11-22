//! `[evm]` in `rustman.toml`: chain overrides, explorers and switches.
//!
//! `rustman-core` keeps the table as written; this module reads and checks
//! it, so a typo is reported with the key that holds it.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::error::EvmError;

/// `[evm]`.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EvmSettings {
    /// `[evm.chains.<id>]`: entries added to or replacing the built-in
    /// chain registry.
    pub chains: BTreeMap<String, ChainOverride>,
    /// `[evm.explorers.<id>]`: Etherscan-compatible APIs for verified ABIs.
    pub explorers: BTreeMap<String, ExplorerConfig>,
    /// Verified ABIs are fetched only when this is `true`.
    pub allow_explorer_fetch: bool,
    /// A mainnet endpoint (URL or `{{variable}}`) for ENS resolution.
    pub ens_rpc: Option<String>,
    /// Lets `jsonrpc` files send raw transactions to chain 31337.
    pub allow_raw_send_local: bool,
    /// Read by the signing gate (evm-signing, the `rustman-evm-signing`
    /// crate); this crate only parses them.
    pub allow_signing: bool,
    pub signers: toml::Table,
    pub protected_chains: Vec<u64>,
    pub headless_signing_chains: Vec<u64>,
    /// The highest max cost (gas limit × max fee + value) `--yes-local`
    /// signs, as a decimal amount of the chain's native unit (`"1"`, the
    /// default, is 1 ETH on Ethereum chains).
    pub max_cost: Option<String>,
}

/// One `[evm.chains.<id>]` entry. Absent fields keep the built-in value.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChainOverride {
    pub name: Option<String>,
    /// The native currency symbol.
    pub symbol: Option<String>,
    /// The native currency decimals (18 unless set).
    pub decimals: Option<u8>,
    /// The block explorer's base URL, such as `https://etherscan.io`.
    pub explorer: Option<String>,
}

/// One `[evm.explorers.<id>]` entry.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExplorerConfig {
    /// The API endpoint, such as `https://api.etherscan.io/v2/api`.
    pub api_url: String,
    /// The API key: a `{{variable}}` naming a secret, resolved by the caller.
    pub api_key: Option<String>,
}

/// Hides the API key.
impl std::fmt::Debug for ExplorerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExplorerConfig")
            .field("api_url", &self.api_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "<hidden>"))
            .finish()
    }
}

impl EvmSettings {
    /// Read `[evm]` from the table `rustman-core` kept.
    pub fn from_table(table: &toml::Table) -> Result<Self, EvmError> {
        let settings: Self = toml::Value::Table(table.clone())
            .try_into()
            .map_err(|e: toml::de::Error| EvmError::config(format!("[evm]: {}", e.message())))?;
        let chains: Vec<&String> = settings.chains.keys().collect();
        let explorers: Vec<&String> = settings.explorers.keys().collect();
        for (section, keys) in [("chains", chains), ("explorers", explorers)] {
            if let Some(bad) = keys.into_iter().find(|k| k.parse::<u64>().is_err()) {
                return Err(EvmError::config(format!(
                    "[evm.{section}.{bad}]: the key must be a chain id (an integer)"
                )));
            }
        }
        Ok(settings)
    }

    /// The explorer configured for `chain_id`.
    pub fn explorer(&self, chain_id: u64) -> Option<&ExplorerConfig> {
        self.explorers.get(&chain_id.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<EvmSettings, EvmError> {
        let table: toml::Table = toml::from_str(text).unwrap();
        EvmSettings::from_table(&table)
    }

    #[test]
    fn defaults_are_off() {
        let settings = parse("").unwrap();
        assert!(!settings.allow_explorer_fetch && !settings.allow_signing);
        assert!(settings.chains.is_empty() && settings.ens_rpc.is_none());
    }

    #[test]
    fn chains_and_explorers_are_read() {
        let settings = parse(
            "allow_explorer_fetch = true\n[chains.999]\nname = \"My L2\"\nsymbol = \"MYL\"\n[explorers.1]\napi_url = \"https://api.etherscan.io/v2/api\"\napi_key = \"{{etherscan_key}}\"\n",
        )
        .unwrap();
        assert_eq!(settings.chains["999"].symbol.as_deref(), Some("MYL"));
        let explorer = settings.explorer(1).unwrap();
        assert_eq!(explorer.api_url, "https://api.etherscan.io/v2/api");
        assert!(!format!("{explorer:?}").contains("etherscan_key"));
    }

    #[test]
    fn typos_and_bad_ids_are_errors() {
        let err = parse("allow_explorer_fetchh = true\n").unwrap_err();
        assert!(err.to_string().contains("allow_explorer_fetchh"), "{err}");
        let err = parse("[chains.mainnet]\nname = \"x\"\n").unwrap_err();
        assert_eq!(
            err.to_string(),
            "[evm.chains.mainnet]: the key must be a chain id (an integer)"
        );
        let err = parse("[chains.1]\nnam = \"x\"\n").unwrap_err();
        assert!(err.to_string().contains("nam"), "{err}");
    }
}
