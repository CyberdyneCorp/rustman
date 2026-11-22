//! Unlocking a configured signer (evm-signing "Approved signers only").
//!
//! Key material only exists inside [`UnlockedSigner`]: a
//! [`Secret`]-wrapped `k256` signing key, zeroized on drop. Keystore passwords, decrypted key bytes and keychain entries are
//! held in [`Zeroizing`] buffers for as long as they are needed, and no
//! error message carries any of them.

use std::path::{Path, PathBuf};

use alloy_primitives::{Address, B256, Signature};
use k256::ecdsa::SigningKey;
use rustman_core::Secret;
use rustman_core::secrets::{Keychain, KeychainError};
use rustman_evm::EvmError;
use zeroize::Zeroizing;

use crate::config::{SignerConfig, SignerSource};

/// The keychain account (under the `rustman` service) of signer `id`.
pub fn keychain_account(id: &str) -> String {
    format!("evm-signer/{id}")
}

/// A signer ready to sign. It has no `Display` and its `Debug` shows only
/// the id and address.
pub struct UnlockedSigner {
    id: String,
    address: Address,
    key: Secret<SigningKey>,
}

impl std::fmt::Debug for UnlockedSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnlockedSigner")
            .field("id", &self.id)
            .field("address", &self.address.to_checksum(None))
            .finish_non_exhaustive()
    }
}

impl UnlockedSigner {
    fn new(id: &str, key: SigningKey) -> Self {
        let point = key.verifying_key().to_sec1_point(false);
        let address = Address::from_raw_public_key(&point.as_bytes()[1..]);
        Self {
            id: id.to_owned(),
            address,
            key: Secret::new(format!("signer:{id}"), key),
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn address(&self) -> Address {
        self.address
    }

    /// Sign the 32-byte `hash` with a low-`s` recoverable signature. Only
    /// the gate's send step calls it, after every precondition passed.
    pub(crate) fn sign_hash(&self, hash: &B256) -> Signature {
        let (signature, recovery) = self.key.expose().sign_prehash_recoverable(hash.as_slice());
        let (r, s) = signature.split_bytes();
        let r = B256::from_slice(&r);
        let s = B256::from_slice(&s);
        Signature::from_scalars_and_parity(r, s, recovery.is_y_odd())
    }
}

/// Asks the user for a keystore password: `Ok(None)` when there is no
/// terminal to ask on, the reason when the terminal could not be read.
pub type PasswordPrompt<'a> = &'a dyn Fn(&str) -> Result<Option<Zeroizing<String>>, String>;

/// What unlocking needs besides the configuration.
pub struct UnlockContext<'a> {
    /// The workspace root: relative keystore paths start there.
    pub root: &'a Path,
    /// The configured keystore password, already resolved (a secret).
    pub password: Option<Secret<Zeroizing<String>>>,
    pub prompt: PasswordPrompt<'a>,
    /// The OS keychain (a stand-in in tests), for keychain signers.
    pub keychain: Option<&'a dyn Keychain>,
}

/// Unlock `config`: decrypt its keystore, read its keychain entry, or
/// reach its hardware wallet.
pub fn unlock(config: &SignerConfig, ctx: &UnlockContext<'_>) -> Result<UnlockedSigner, EvmError> {
    let id = config.id.as_str();
    match &config.source {
        SignerSource::Keystore { path, .. } => {
            unlock_keystore(id, &keystore_path(ctx.root, path), ctx)
        }
        SignerSource::Keychain => unlock_keychain(id, ctx.keychain),
        SignerSource::Ledger { .. } => hardware_wallet(id, "Ledger"),
        SignerSource::Trezor { .. } => hardware_wallet(id, "Trezor"),
    }
}

/// `~/` is the home directory; a relative path starts at the workspace root.
pub fn keystore_path(root: &Path, path: &str) -> PathBuf {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    match (
        path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")),
        home,
    ) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => root.join(path),
    }
}

fn unlock_keystore(
    id: &str,
    path: &Path,
    ctx: &UnlockContext<'_>,
) -> Result<UnlockedSigner, EvmError> {
    if !path.is_file() {
        return Err(EvmError::config(format!(
            "signer '{id}': keystore {} not found",
            path.display()
        )));
    }
    let prompted;
    let password: &str = match &ctx.password {
        Some(secret) => secret.expose(),
        None => {
            let question = format!("password for keystore signer '{id}': ");
            prompted = (ctx.prompt)(&question)
                .map_err(|reason| {
                    EvmError::config(format!(
                        "signer '{id}': cannot read the keystore password: {reason}"
                    ))
                })?
                .ok_or_else(|| {
                    EvmError::Refused(format!(
                        "signer '{id}': the keystore password is needed; set password = \"{{{{secret:...}}}}\" in [evm.signers.{id}] or run in a terminal"
                    ))
                })?;
            &prompted
        }
    };
    let (key, address) = decrypt_checked(id, path, password)?;
    let signer = from_key_bytes(id, &key)?;
    match address {
        Some(address) if address != signer.address() => Err(EvmError::config(format!(
            "signer '{id}': keystore {} decrypts to {}, not its 'address' {}; refusing a keystore whose key does not match it",
            path.display(),
            signer.address().to_checksum(None),
            address.to_checksum(None)
        ))),
        _ => Ok(signer),
    }
}

/// The largest keystore file read; a Web3 Secret Storage file is under
/// 1 KiB.
const MAX_KEYSTORE_BYTES: u64 = 64 * 1024;

/// The decrypted key and the keystore's `address`, when it has one.
type Decrypted = (Zeroizing<Vec<u8>>, Option<Address>);

/// Read the keystore once, refuse a costly or malformed key derivation
/// and a malformed cipher, then decrypt exactly the bytes that were
/// checked (from a private copy, so the file cannot be swapped between
/// the check and the decryption).
fn decrypt_checked(id: &str, path: &Path, password: &str) -> Result<Decrypted, EvmError> {
    let unreadable = |reason: String| {
        EvmError::config(format!(
            "signer '{id}': unreadable keystore {}: {reason}",
            path.display()
        ))
    };
    let bytes = read_bounded(path).map_err(unreadable)?;
    let json: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| unreadable(e.to_string()))?;
    check_kdf(&json).map_err(|reason| {
        EvmError::config(format!(
            "signer '{id}': keystore {} asks for a key derivation above the accepted limits ({reason}; scrypt n <= {MAX_SCRYPT_N}, r <= {MAX_SCRYPT_R}, p <= {MAX_SCRYPT_P}; pbkdf2 c <= {MAX_PBKDF2_C}; dklen = 32); re-encrypt the key with standard parameters",
            path.display()
        ))
    })?;
    let address = check_cipher(&json).map_err(|reason| {
        EvmError::config(format!(
            "signer '{id}': malformed keystore {}: {reason}",
            path.display()
        ))
    })?;
    let copy = private_copy(&bytes).map_err(|e| unreadable(e.to_string()))?;
    let key = eth_keystore::decrypt_key(copy.path(), password.as_bytes())
        .map(Zeroizing::new)
        .map_err(|e| keystore_error(id, &e))?;
    Ok((key, address))
}

/// The one cipher a keystore may use, and its parameter lengths in hex
/// digits: a 16-byte IV, a 32-byte encrypted key, a 32-byte MAC.
const CIPHER: &str = "aes-128-ctr";
const IV_HEX: usize = 32;
const CIPHERTEXT_HEX: usize = 64;
const MAC_HEX: usize = 64;

/// Check the cipher of a keystore, failing closed: the decryption ignores
/// `cipher`, panics on a short IV and truncates a long one (the IV is not
/// covered by the MAC). Returns its `address`, which the decrypted key
/// must match.
fn check_cipher(json: &serde_json::Value) -> Result<Option<Address>, String> {
    let crypto = json
        .get("crypto")
        .or_else(|| json.get("Crypto"))
        .ok_or("no 'crypto'")?;
    if crypto["cipher"].as_str() != Some(CIPHER) {
        return Err(format!("cipher is not {CIPHER}"));
    }
    hex_field(
        "cipherparams.iv",
        &crypto["cipherparams"]["iv"],
        Some(IV_HEX),
    )?;
    hex_field("ciphertext", &crypto["ciphertext"], Some(CIPHERTEXT_HEX))?;
    hex_field("mac", &crypto["mac"], Some(MAC_HEX))?;
    hex_field("kdfparams.salt", &crypto["kdfparams"]["salt"], None)?;
    match json.get("address") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => {
            let text = value.as_str().ok_or("address is not a string")?;
            let text = text.strip_prefix("0x").unwrap_or(text);
            hex_field("address", &serde_json::json!(text), Some(40))?;
            text.parse::<Address>()
                .map(Some)
                .map_err(|_| "address is not an address".to_owned())
        }
    }
}

/// `value` is a non-empty string of hex digit pairs (`len` digits when
/// given), without `0x`.
fn hex_field(name: &str, value: &serde_json::Value, len: Option<usize>) -> Result<(), String> {
    let text = value.as_str().ok_or(format!("no '{name}'"))?;
    let hex =
        !text.is_empty() && text.len() % 2 == 0 && text.bytes().all(|b| b.is_ascii_hexdigit());
    match (hex, len) {
        (false, _) => Err(format!("{name} is not hex")),
        (true, Some(len)) if text.len() != len => {
            Err(format!("{name} has {} hex digits, not {len}", text.len()))
        }
        _ => Ok(()),
    }
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(MAX_KEYSTORE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    match bytes.len() as u64 > MAX_KEYSTORE_BYTES {
        true => Err(format!("larger than {MAX_KEYSTORE_BYTES} bytes")),
        false => Ok(bytes),
    }
}

/// The checked keystore (encrypted) in a file only this user can read,
/// removed on drop.
fn private_copy(bytes: &[u8]) -> std::io::Result<tempfile::NamedTempFile> {
    use std::io::Write;
    let mut copy = tempfile::NamedTempFile::new()?;
    copy.write_all(bytes)?;
    copy.flush()?;
    Ok(copy)
}

/// The largest key derivation a keystore may ask for. Decryption runs the
/// derivation the file describes, so a hostile file could otherwise hang
/// the process or exhaust its memory. Geth's standard scrypt (n = 2^18,
/// r = 8, p = 1: 256 MiB) is the ceiling; pbkdf2 (c = 262144) is well
/// inside.
pub const MAX_SCRYPT_N: u64 = 1 << 18;
pub const MAX_SCRYPT_R: u64 = 8;
pub const MAX_SCRYPT_P: u64 = 4;
/// The largest n × r × p, the scrypt work (geth's standard).
pub const MAX_SCRYPT_WORK: u64 = 1 << 21;
pub const MAX_PBKDF2_C: u64 = 10_000_000;
/// The only derived key length a keystore may use: 16 bytes of AES key
/// and 16 of MAC key.
const DKLEN: u64 = 32;

/// Check the key derivation of a keystore, failing closed. The decryption
/// picks the derivation from the shape of `kdfparams` (not from `kdf`), so
/// the shape must agree with `kdf` and every cost parameter present is
/// bounded, whichever derivation would run.
fn check_kdf(json: &serde_json::Value) -> Result<(), String> {
    let crypto = json
        .get("crypto")
        .or_else(|| json.get("Crypto"))
        .ok_or("no 'crypto'")?;
    let params = crypto["kdfparams"].as_object().ok_or("no 'kdfparams'")?;
    let has = |name: &str| params.contains_key(name);
    let shape = match crypto["kdf"].as_str() {
        Some("scrypt") if !has("c") && !has("prf") => Ok(()),
        Some("pbkdf2") if !has("n") && !has("r") && !has("p") => Ok(()),
        Some(kdf @ ("scrypt" | "pbkdf2")) => Err(format!("kdfparams do not match kdf {kdf}")),
        _ => Err("kdf is neither scrypt nor pbkdf2".to_owned()),
    };
    shape?;
    if param(params, "dklen")? != Some(DKLEN) {
        return Err("dklen is not 32".to_owned());
    }
    if let Some(c) = param(params, "c")? {
        bounded("c", c, MAX_PBKDF2_C)?;
    }
    if crypto["kdf"] == "scrypt" {
        scrypt_within_limits(params)?;
    }
    Ok(())
}

fn scrypt_within_limits(params: &serde_json::Map<String, serde_json::Value>) -> Result<(), String> {
    let required = |name: &str| param(params, name)?.ok_or(format!("no scrypt '{name}'"));
    let (n, r, p) = (required("n")?, required("r")?, required("p")?);
    bounded("n", n, MAX_SCRYPT_N)?;
    bounded("r", r, MAX_SCRYPT_R)?;
    bounded("p", p, MAX_SCRYPT_P)?;
    let work = n.saturating_mul(r).saturating_mul(p);
    bounded("n × r × p", work, MAX_SCRYPT_WORK)
}

fn bounded(name: &str, value: u64, max: u64) -> Result<(), String> {
    match value <= max {
        true => Ok(()),
        false => Err(format!("{name} = {value}")),
    }
}

/// A numeric KDF parameter: absent is `None`; present but not an
/// unsigned integer is refused.
fn param(
    params: &serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Result<Option<u64>, String> {
    match params.get(name) {
        None => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("{name} is not an unsigned integer")),
    }
}

fn keystore_error(id: &str, error: &eth_keystore::KeystoreError) -> EvmError {
    match error {
        eth_keystore::KeystoreError::MacMismatch => {
            EvmError::Refused(format!("signer '{id}': wrong keystore password"))
        }
        other => EvmError::config(format!("signer '{id}': unreadable keystore: {other}")),
    }
}

fn unlock_keychain(id: &str, keychain: Option<&dyn Keychain>) -> Result<UnlockedSigner, EvmError> {
    let keychain = keychain
        .ok_or_else(|| EvmError::config(format!("signer '{id}': no OS keychain is available")))?;
    let stored = keychain
        .get(&keychain_account(id))
        .map_err(|e| keychain_failure(id, &e))?
        .map(Zeroizing::new)
        .ok_or_else(|| {
            EvmError::config(format!(
                "signer '{id}' is not in the OS keychain; run 'rustman evm signer import {id}'"
            ))
        })?;
    let key = parse_key(&stored).ok_or_else(|| {
        EvmError::config(format!("signer '{id}': the keychain entry is not a key"))
    })?;
    from_key_bytes(id, &key)
}

pub(crate) fn keychain_failure(id: &str, error: &KeychainError) -> EvmError {
    let reason = match error {
        KeychainError::Unavailable(reason) | KeychainError::Failed(reason) => reason,
    };
    EvmError::config(format!("signer '{id}': OS keychain: {reason}"))
}

/// The 32 key bytes of `text` (64 hex digits, `0x` optional).
pub(crate) fn parse_key(text: &str) -> Option<Zeroizing<Vec<u8>>> {
    let text = text.trim();
    let digits = text.strip_prefix("0x").unwrap_or(text);
    if digits.len() != 64 {
        return None;
    }
    alloy_primitives::hex::decode(digits)
        .ok()
        .map(Zeroizing::new)
}

pub(crate) fn from_key_bytes(id: &str, key: &[u8]) -> Result<UnlockedSigner, EvmError> {
    SigningKey::from_slice(key)
        .map(|key| UnlockedSigner::new(id, key))
        .map_err(|_| EvmError::config(format!("signer '{id}': not a valid secp256k1 key")))
}

/// The refusal of a hardware wallet: not available yet in any build
/// (the `hw-wallet` feature links no driver).
pub fn hardware_wallet_unavailable(id: &str, device: &str) -> String {
    format!(
        "signer '{id}': {device} signing is not available yet; use a keystore or keychain signer"
    )
}

fn hardware_wallet(id: &str, device: &str) -> Result<UnlockedSigner, EvmError> {
    Err(EvmError::config(hardware_wallet_unavailable(id, device)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Anvil's first well-known dev account.
    const ANVIL0: &str = "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
    const ANVIL0_ADDRESS: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";

    #[test]
    fn signatures_recover_to_the_signer_with_a_low_s() {
        let key = parse_key(ANVIL0).unwrap();
        let signer = from_key_bytes("dev", &key).unwrap();
        assert_eq!(signer.address().to_checksum(None), ANVIL0_ADDRESS);
        let hash = alloy_primitives::keccak256(b"rustman");
        let signature = signer.sign_hash(&hash);
        let ecdsa = k256::ecdsa::Signature::from_scalars(
            signature.r().to_be_bytes::<32>(),
            signature.s().to_be_bytes::<32>(),
        )
        .unwrap();
        assert_eq!(ecdsa.normalize_s(), ecdsa, "s is already low");
        let recovery = k256::ecdsa::RecoveryId::new(signature.v(), false);
        let recovered =
            k256::ecdsa::VerifyingKey::recover_from_prehash(hash.as_slice(), &ecdsa, recovery)
                .unwrap();
        let point = recovered.to_sec1_point(false);
        assert_eq!(
            Address::from_raw_public_key(&point.as_bytes()[1..]),
            signer.address()
        );
    }

    #[test]
    fn debug_never_shows_the_key() {
        let signer = from_key_bytes("dev", &parse_key(ANVIL0).unwrap()).unwrap();
        let shown = format!("{signer:?}");
        assert!(shown.contains("dev") && shown.contains("0xf39F"), "{shown}");
        assert!(!shown.to_lowercase().contains(&ANVIL0[..16]), "{shown}");
    }

    #[test]
    fn the_signing_key_is_zeroized_on_drop() {
        fn zeroized_on_drop<T: zeroize::ZeroizeOnDrop>() {}
        zeroized_on_drop::<SigningKey>();
        zeroized_on_drop::<Zeroizing<String>>();
    }
}
