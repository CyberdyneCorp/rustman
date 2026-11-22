//! Nonces (evm-signing "Nonce management", design D9): the request's
//! `nonce`, else `eth_getTransactionCount(signer, "pending")` once per
//! signer and chain, then an in-run counter, so consecutive transactions
//! of one run never race for the same nonce. A failed send is never
//! retried: the node's `nonce too low` or `replacement underpriced` is
//! reported as it came.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use alloy_primitives::Address;
use rustman_evm::EvmError;
use rustman_evm::quantity::parse_hex_u64;
use rustman_evm::rpc::{Rpc, call};
use serde_json::json;

/// The next nonce of each signer on each chain, for one run.
#[derive(Debug, Default)]
pub struct Nonces {
    next: Mutex<HashMap<(u64, Address), u64>>,
}

impl Nonces {
    pub fn new() -> Self {
        Self::default()
    }

    /// The nonce for the next transaction of `signer` on `chain_id`: the
    /// request's `explicit` one, the in-run counter, or the node's pending
    /// count (asked only the first time).
    pub async fn next(
        &self,
        rpc: &dyn Rpc,
        chain_id: u64,
        signer: Address,
        explicit: Option<&str>,
    ) -> Result<u64, EvmError> {
        if let Some(text) = explicit {
            return parse_nonce(text);
        }
        if let Some(next) = self.lock().get(&(chain_id, signer)) {
            return Ok(*next);
        }
        let params = json!([signer.to_checksum(None), "pending"]);
        let answer = call(rpc, "eth_getTransactionCount", params).await?;
        answer
            .as_str()
            .and_then(|text| parse_hex_u64(text).ok())
            .ok_or_else(|| EvmError::Rpc("eth_getTransactionCount: not a quantity".to_owned()))
    }

    /// `nonce` was sent for `signer` on `chain_id`: the next one is
    /// `nonce + 1`.
    pub fn sent(&self, chain_id: u64, signer: Address, nonce: u64) {
        let next = nonce.saturating_add(1);
        let mut map = self.lock();
        let entry = map.entry((chain_id, signer)).or_insert(next);
        *entry = (*entry).max(next);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(u64, Address), u64>> {
        self.next.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A `nonce` as written: decimal or `0x` hex.
fn parse_nonce(text: &str) -> Result<u64, EvmError> {
    let text = text.trim();
    let parsed = match text.starts_with("0x") {
        true => parse_hex_u64(text).ok(),
        false => text.parse().ok(),
    };
    parsed.ok_or_else(|| EvmError::config(format!("nonce: '{text}' is not a number")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonces_parse_as_decimal_or_hex() {
        assert_eq!(parse_nonce("7").unwrap(), 7);
        assert_eq!(parse_nonce("0x10").unwrap(), 16);
        assert_eq!(parse_nonce("seven").unwrap_err().exit_code(), 2);
    }
}
