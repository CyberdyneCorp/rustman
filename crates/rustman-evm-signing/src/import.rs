//! `rustman evm signer import <id>`: the only place a private key is
//! accepted. The key moves into the OS keychain and is referenced from
//! then on as `[evm.signers.<id>] keychain = true`.

use alloy_primitives::Address;
use rustman_core::secrets::Keychain;
use rustman_evm::EvmError;
use zeroize::Zeroizing;

use crate::config::check_id;
use crate::signer::{from_key_bytes, keychain_account, keychain_failure, parse_key};

/// Store `key` (64 hex digits, `0x` optional) for signer `id` in
/// `keychain`, replacing any earlier entry, and return its address.
pub fn import_key(
    keychain: &dyn Keychain,
    id: &str,
    key: &Zeroizing<String>,
) -> Result<Address, EvmError> {
    check_id(id)?;
    let bytes = parse_key(key).ok_or_else(|| {
        EvmError::config("not a private key: expected 64 hex digits, with or without 0x")
    })?;
    let address = from_key_bytes(id, &bytes)?.address();
    let normalized = Zeroizing::new(alloy_primitives::hex::encode_prefixed(bytes.as_slice()));
    keychain
        .set(&keychain_account(id), &normalized)
        .map_err(|e| keychain_failure(id, &e))?;
    Ok(address)
}

/// The lines `import` prints after storing signer `id`.
pub fn imported_message(id: &str, address: Address) -> String {
    format!(
        "imported signer '{id}' ({}) into the OS keychain\nuse it from rustman.toml:\n  [evm.signers.{id}]\n  keychain = true",
        address.to_checksum(None)
    )
}
