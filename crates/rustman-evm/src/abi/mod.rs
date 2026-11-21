//! ABI sources (evm-toolkit "ABI sources", design D4): the built-in token
//! standards, JSON ABI files and Foundry or Hardhat artifacts, resolved
//! relative to the request file and kept inside the workspace. Parsed ABIs
//! are cached by the SHA-256 of their bytes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use alloy_json_abi::{Function, JsonAbi};
use alloy_primitives::Address;
use serde_json::Value;

use crate::error::EvmError;

/// The names `abi:` accepts for the built-in standards.
pub const BUILT_IN: [&str; 3] = ["erc20", "erc721", "erc1155"];

fn parse_built_in(text: &str) -> Arc<JsonAbi> {
    Arc::new(serde_json::from_str(text).expect("built-in ABIs parse"))
}

static ERC20: LazyLock<Arc<JsonAbi>> =
    LazyLock::new(|| parse_built_in(include_str!("builtin/erc20.json")));
static ERC721: LazyLock<Arc<JsonAbi>> =
    LazyLock::new(|| parse_built_in(include_str!("builtin/erc721.json")));
static ERC1155: LazyLock<Arc<JsonAbi>> =
    LazyLock::new(|| parse_built_in(include_str!("builtin/erc1155.json")));

/// The built-in ABI `name` (`erc20`, `erc721` or `erc1155`).
pub fn built_in(name: &str) -> Option<Arc<JsonAbi>> {
    match name {
        "erc20" => Some(ERC20.clone()),
        "erc721" => Some(ERC721.clone()),
        "erc1155" => Some(ERC1155.clone()),
        _ => None,
    }
}

/// Where an ABI came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AbiOrigin {
    BuiltIn(&'static str),
    /// A file, relative to the workspace root.
    File(PathBuf),
    /// Fetched from (or cached for) a block explorer. A proxy's ABI is its
    /// implementation's.
    Explorer {
        chain_id: u64,
        address: Address,
        implementation: Option<Address>,
    },
}

/// An ABI and where it came from.
#[derive(Clone, Debug)]
pub struct LoadedAbi {
    pub abi: Arc<JsonAbi>,
    pub origin: AbiOrigin,
}

/// The SHA-256 of `bytes`.
pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    let mut out = [0; 32];
    out.copy_from_slice(digest.as_ref());
    out
}

/// Parsed ABIs by content hash, for the session.
#[derive(Debug, Default)]
pub struct AbiCache {
    parsed: Mutex<HashMap<[u8; 32], Arc<JsonAbi>>>,
}

impl AbiCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// How many distinct ABI texts have been parsed.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<[u8; 32], Arc<JsonAbi>>> {
        self.parsed.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The ABI `source` names for the request file `file` (relative to the
    /// workspace `root`): a built-in name, or a path relative to the file.
    pub fn load(&self, root: &Path, file: &Path, source: &str) -> Result<LoadedAbi, EvmError> {
        if let Some(name) = BUILT_IN.into_iter().find(|n| *n == source) {
            let abi = built_in(name).expect("listed built-in");
            return Ok(LoadedAbi {
                abi,
                origin: AbiOrigin::BuiltIn(name),
            });
        }
        let base = root.join(file);
        let base = base.parent().unwrap_or(root);
        let path = rustman_core::paths::contain(root, base, source).map_err(|_| {
            EvmError::config(format!(
                "abi path '{source}' resolves outside the workspace"
            ))
        })?;
        let bytes = std::fs::read(&path)
            .map_err(|e| EvmError::config(format!("abi '{source}': cannot read: {e}")))?;
        let abi = self
            .parse(&bytes)
            .map_err(|e| EvmError::config(format!("abi '{source}': {e}")))?;
        let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        Ok(LoadedAbi {
            abi,
            origin: AbiOrigin::File(relative),
        })
    }

    /// Parse a JSON ABI (an array) or an artifact (an object with an `abi`
    /// array, as Foundry and Hardhat write), from the cache when these
    /// exact bytes were parsed before.
    pub fn parse(&self, bytes: &[u8]) -> Result<Arc<JsonAbi>, String> {
        let key = sha256(bytes);
        if let Some(abi) = self.lock().get(&key) {
            return Ok(abi.clone());
        }
        let value: Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        let items = match value {
            Value::Array(_) => value,
            Value::Object(mut object) => match object.remove("abi") {
                Some(abi @ Value::Array(_)) => abi,
                _ => return Err(NOT_AN_ABI.to_owned()),
            },
            _ => return Err(NOT_AN_ABI.to_owned()),
        };
        let abi: JsonAbi = serde_json::from_value(items).map_err(|e| e.to_string())?;
        let abi = Arc::new(abi);
        self.lock().insert(key, abi.clone());
        Ok(abi)
    }
}

const NOT_AN_ABI: &str = "not a JSON ABI or a Foundry/Hardhat artifact (no 'abi' array)";

/// The function `spec` names: a name, or a full signature such as
/// `transfer(address,uint256)` (spaces ignored). A bare name of an
/// overloaded function is an error suggesting the first signature.
pub fn resolve_function<'a>(abi: &'a JsonAbi, spec: &str) -> Result<&'a Function, EvmError> {
    let spec: String = spec.chars().filter(|c| !c.is_whitespace()).collect();
    if spec.contains('(') {
        return abi
            .functions()
            .find(|f| f.signature() == spec)
            .ok_or_else(|| EvmError::config(format!("function '{spec}' not found in the ABI")));
    }
    match abi.function(&spec).map(Vec::as_slice) {
        Some([only]) => Ok(only),
        Some([first, ..]) => Err(EvmError::config(format!(
            "function '{spec}' is overloaded; use a full signature such as '{}'",
            first.signature()
        ))),
        _ => Err(EvmError::config(format!(
            "function '{spec}' not found in the ABI"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_ins_have_the_token_standards() {
        let erc20 = built_in("erc20").unwrap();
        let transfer = resolve_function(&erc20, "transfer").unwrap();
        assert_eq!(transfer.signature(), "transfer(address,uint256)");
        assert!(erc20.event("Transfer").is_some());
        let erc721 = built_in("erc721").unwrap();
        assert!(erc721.function("ownerOf").is_some());
        let erc1155 = built_in("erc1155").unwrap();
        assert!(erc1155.event("TransferSingle").is_some());
        assert!(built_in("erc4626").is_none());
    }

    #[test]
    fn overloaded_functions_need_a_signature() {
        let erc721 = built_in("erc721").unwrap();
        let err = resolve_function(&erc721, "safeTransferFrom").unwrap_err();
        assert_eq!(
            err.to_string(),
            "function 'safeTransferFrom' is overloaded; use a full signature such as 'safeTransferFrom(address,address,uint256)'"
        );
        let full = resolve_function(
            &erc721,
            "safeTransferFrom(address, address, uint256, bytes)",
        )
        .unwrap();
        assert_eq!(full.inputs.len(), 4);
        let err = resolve_function(&erc721, "mint").unwrap_err();
        assert_eq!(err.to_string(), "function 'mint' not found in the ABI");
        let err = resolve_function(&erc721, "mint(uint256)").unwrap_err();
        assert_eq!(
            err.to_string(),
            "function 'mint(uint256)' not found in the ABI"
        );
    }

    #[test]
    fn parsing_is_cached_by_content() {
        let cache = AbiCache::new();
        let text = include_bytes!("builtin/erc20.json");
        let a = cache.parse(text).unwrap();
        let b = cache.parse(text).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(cache.len(), 1);
        let artifact = format!("{{\"abi\": {}}}", String::from_utf8_lossy(text));
        cache.parse(artifact.as_bytes()).unwrap();
        assert_eq!(cache.len(), 2);
        assert_eq!(
            cache.parse(b"{\"bytecode\": \"0x\"}").unwrap_err(),
            NOT_AN_ABI
        );
        assert_eq!(cache.parse(b"5").unwrap_err(), NOT_AN_ABI);
        assert!(cache.parse(b"[{\"type\": \"function\"}]").is_err());
    }
}
