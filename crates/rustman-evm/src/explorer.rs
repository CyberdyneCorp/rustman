//! Verified ABIs from an Etherscan-compatible explorer (evm-toolkit
//! "Verified ABI fetch is opt-in", design D4).
//!
//! Off unless `[evm] allow_explorer_fetch = true`: the refusal comes before
//! any network access. EIP-1967 proxies resolve to their implementation
//! through `eth_getStorageAt`. Fetched ABIs are cached under
//! `.rustman/abi-cache/<chain>/<address>.json`. The API key is a secret and
//! is redacted from every message.

use std::path::{Path, PathBuf};

use alloy_primitives::{Address, B256};
use rustman_core::Secret;
use rustman_core::model::RequestSettings;
use rustman_core::redact::Redactor;
use rustman_core::vars::{Resolved, ResolvedBody, ResolvedPair, ResolvedRequest, TlsFiles};
use rustman_http::SendOptions;
use rustman_jsonrpc::HttpSend;
use serde_json::{Value, json};

use crate::abi::{AbiCache, AbiOrigin, LoadedAbi};
use crate::error::EvmError;
use crate::rpc::{Rpc, call};
use crate::settings::EvmSettings;

/// The refusal while fetching is not enabled.
pub const FETCH_DISABLED: &str =
    "explorer ABI fetch is disabled; set [evm] allow_explorer_fetch = true";

/// Where fetched ABIs are kept, under the workspace root (git-ignored with
/// the rest of `.rustman/`).
pub const ABI_CACHE_DIR: &str = ".rustman/abi-cache";

/// The EIP-1967 implementation slot: `keccak256("eip1967.proxy.implementation") - 1`.
pub const EIP1967_IMPLEMENTATION_SLOT: &str =
    "0x360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc";

/// The cache file of the ABI of `address` on `chain_id`.
pub fn cache_path(root: &Path, chain_id: u64, address: Address) -> PathBuf {
    root.join(ABI_CACHE_DIR)
        .join(chain_id.to_string())
        .join(format!("{address:#x}.json"))
}

/// The implementation an EIP-1967 proxy at `address` points to, or `None`
/// when the slot is empty (not a proxy).
pub async fn proxy_implementation(
    rpc: &dyn Rpc,
    address: Address,
) -> Result<Option<Address>, EvmError> {
    let params = json!([
        address.to_checksum(None),
        EIP1967_IMPLEMENTATION_SLOT,
        "latest"
    ]);
    let value = call(rpc, "eth_getStorageAt", params).await?;
    let word: B256 = value
        .as_str()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| EvmError::Rpc(format!("eth_getStorageAt: not a 32-byte word: {value}")))?;
    let implementation = Address::from_word(word);
    Ok((implementation != Address::ZERO).then_some(implementation))
}

/// A verified ABI and how it was found.
#[derive(Clone, Debug)]
pub struct FetchedAbi {
    pub loaded: LoadedAbi,
    /// Read from `.rustman/abi-cache/` rather than the explorer.
    pub from_cache: bool,
}

impl FetchedAbi {
    /// `proxy → implementation 0x…` when the address was a proxy.
    pub fn note(&self) -> Option<String> {
        match &self.loaded.origin {
            AbiOrigin::Explorer {
                implementation: Some(implementation),
                ..
            } => Some(format!(
                "proxy → implementation {}",
                implementation.to_checksum(None)
            )),
            _ => None,
        }
    }
}

/// What a fetch needs: the settings, the workspace root, an HTTP sender
/// (the front end's governed sender, so the safety policy applies) and the
/// resolved API key.
pub struct Explorer<'a> {
    pub settings: &'a EvmSettings,
    pub root: &'a Path,
    pub http: &'a dyn HttpSend,
    pub options: &'a SendOptions,
    pub api_key: Option<Secret<String>>,
    pub cache: &'a AbiCache,
}

impl Explorer<'_> {
    /// The verified ABI of `address` on `chain_id`, or of its
    /// implementation when it is an EIP-1967 proxy.
    pub async fn fetch(
        &self,
        rpc: &dyn Rpc,
        chain_id: u64,
        address: Address,
    ) -> Result<FetchedAbi, EvmError> {
        if !self.settings.allow_explorer_fetch {
            return Err(EvmError::config(FETCH_DISABLED));
        }
        let implementation = proxy_implementation(rpc, address).await?;
        let target = implementation.unwrap_or(address);
        let path = cache_path(self.root, chain_id, target);
        let (bytes, from_cache) = match std::fs::read(&path) {
            Ok(bytes) => (bytes, true),
            Err(_) => {
                let bytes = self.download(chain_id, target).await?;
                store(&path, &bytes)?;
                (bytes, false)
            }
        };
        let abi = self
            .cache
            .parse(&bytes)
            .map_err(|e| EvmError::config(format!("explorer ABI of {target:#x}: {e}")))?;
        let origin = AbiOrigin::Explorer {
            chain_id,
            address,
            implementation,
        };
        Ok(FetchedAbi {
            loaded: LoadedAbi { abi, origin },
            from_cache,
        })
    }

    fn redactor(&self) -> Redactor {
        let mut redactor = Redactor::new();
        if let Some(key) = &self.api_key {
            redactor.add(key);
        }
        redactor
    }

    fn request(&self, api_url: &str, chain_id: u64, address: Address) -> ResolvedRequest {
        let pair = |name: &str, value: String| ResolvedPair {
            name: Resolved::literal(name),
            value: Resolved::literal(value),
        };
        let mut query = vec![
            pair("chainid", chain_id.to_string()),
            pair("module", "contract".to_owned()),
            pair("action", "getabi".to_owned()),
            pair("address", format!("{address:#x}")),
        ];
        if let Some(key) = &self.api_key {
            query.push(pair("apikey", key.expose().clone()));
        }
        ResolvedRequest {
            method: "GET".to_owned(),
            url: Resolved::literal(api_url),
            query,
            headers: Vec::new(),
            body: ResolvedBody::None,
            auth: None,
            settings: RequestSettings::default(),
            tls: TlsFiles::default(),
            secrets: self.api_key.iter().cloned().collect(),
        }
    }

    async fn download(&self, chain_id: u64, address: Address) -> Result<Vec<u8>, EvmError> {
        let explorer = self.settings.explorer(chain_id).ok_or_else(|| {
            EvmError::config(format!(
                "no explorer configured for chain {chain_id}; add [evm.explorers.{chain_id}] api_url"
            ))
        })?;
        let redactor = self.redactor();
        let shown = |text: &str| redactor.text(text).into_owned();
        let request = self.request(&explorer.api_url, chain_id, address);
        let exchange = self
            .http
            .send_http(&request, self.options)
            .await
            .map_err(|e| EvmError::Rpc(shown(&format!("explorer: {e}"))))?;
        if exchange.status != 200 {
            return Err(EvmError::Rpc(format!(
                "explorer: HTTP {} from {}",
                exchange.status,
                shown(&explorer.api_url)
            )));
        }
        let body: Value = serde_json::from_slice(&exchange.body)
            .map_err(|e| EvmError::Rpc(format!("explorer: invalid JSON: {e}")))?;
        match (body["status"].as_str(), body["result"].as_str()) {
            (Some("1"), Some(abi)) => Ok(abi.as_bytes().to_vec()),
            _ => {
                let message = body["result"].as_str().or(body["message"].as_str());
                Err(EvmError::Rpc(shown(&format!(
                    "explorer: no verified ABI for {address:#x}: {}",
                    message.unwrap_or("unexpected answer")
                ))))
            }
        }
    }
}

/// Write the cache file `path` through a uniquely named temporary file in
/// its folder, renamed into place, so a crash or a concurrent run never
/// leaves a truncated ABI to be read back. When another run renamed its
/// copy first (Windows refuses to replace a file in use), that copy stays.
fn store(path: &Path, bytes: &[u8]) -> Result<(), EvmError> {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
    let temp = path.with_file_name(format!(
        ".{}.{}-{n}.tmp",
        name.unwrap_or_default(),
        std::process::id()
    ));
    let write = || -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&temp, bytes)?;
        match std::fs::rename(&temp, path) {
            Err(_) if path.is_file() => Ok(()),
            other => other,
        }
    };
    let result = write();
    let _ = std::fs::remove_file(&temp);
    result.map_err(|e| EvmError::config(format!("{}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use alloy_primitives::address;

    use super::*;

    #[test]
    fn the_cache_is_replaced_whole_through_a_temporary_file() {
        // Regression: a plain write could leave a truncated ABI behind.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("abi-cache/1/0xabc.json");
        store(&path, b"[1]").unwrap();
        store(&path, b"[2, 3]").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"[2, 3]");
        let names: Vec<String> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["0xabc.json"], "no temporary file is left");
    }

    #[test]
    fn the_cache_path_uses_chain_and_lowercase_address() {
        let path = cache_path(
            Path::new("ws"),
            1,
            address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
        );
        assert_eq!(
            path,
            Path::new("ws/.rustman/abi-cache/1/0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48.json")
        );
    }

    #[test]
    fn the_slot_is_eip1967() {
        let hash = alloy_primitives::keccak256("eip1967.proxy.implementation");
        let slot =
            alloy_primitives::U256::from_be_bytes(hash.0) - alloy_primitives::U256::from(1u8);
        assert_eq!(format!("{slot:#066x}"), EIP1967_IMPLEMENTATION_SLOT);
    }
}
