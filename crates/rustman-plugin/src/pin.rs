//! sha256 pinning (plugins spec, "Integrity pinning and lockfile").

use sha2::{Digest, Sha256};

use crate::error::LoadError;

/// The lowercase hex sha256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(64);
    for byte in digest {
        hex.push(char::from(HEX[usize::from(byte >> 4)]));
        hex.push(char::from(HEX[usize::from(byte & 0xf)]));
    }
    hex
}

const HEX: &[u8; 16] = b"0123456789abcdef";

/// Refuse `bytes` unless they hash to `expected` (lowercase hex).
pub fn verify(plugin: &str, expected: &str, bytes: &[u8]) -> Result<(), LoadError> {
    let actual = sha256_hex(bytes);
    if actual == expected {
        return Ok(());
    }
    Err(LoadError::Mismatch {
        plugin: plugin.to_owned(),
        expected: expected.to_owned(),
        actual,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_are_lowercase_hex() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn a_different_hash_is_a_mismatch() {
        let pin = sha256_hex(b"plugin");
        assert!(verify("hmac", &pin, b"plugin").is_ok());
        let err = verify("hmac", &pin, b"tampered").unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "plugin hmac: sha256 mismatch (expected {pin}, got {})",
                sha256_hex(b"tampered")
            )
        );
    }
}
