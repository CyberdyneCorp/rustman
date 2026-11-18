//! The bearer token of the HTTP transport (spec "HTTP bearer token"): read
//! from `--token-file` or generated at startup, compared in constant time.

use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::digest::{SHA256, digest};
use ring::rand::{SecureRandom, SystemRandom};

/// Random bytes in a generated token.
pub const TOKEN_BYTES: usize = 32;

/// Fill `bytes` from the OS random source.
pub fn fill_random(bytes: &mut [u8]) {
    SystemRandom::new()
        .fill(bytes)
        .expect("the OS random source is available");
}

/// A new token: [`TOKEN_BYTES`] random bytes, base64url without padding.
pub fn generate() -> String {
    let mut bytes = [0u8; TOKEN_BYTES];
    fill_random(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// The token in `path`, without surrounding whitespace. An empty file is an
/// error: it would accept an empty bearer.
pub fn read_file(path: &Path) -> Result<String, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read the token file {}: {e}", path.display()))?;
    let token = text.trim();
    if token.is_empty() {
        return Err(format!("the token file {} is empty", path.display()));
    }
    Ok(token.to_owned())
}

/// Whether `given` equals `expected`, in time that depends on neither: both
/// are hashed first, and the digests compared without an early exit.
pub fn same(given: &str, expected: &str) -> bool {
    let a = digest(&SHA256, given.as_bytes());
    let b = digest(&SHA256, expected.as_bytes());
    let diff = a
        .as_ref()
        .iter()
        .zip(b.as_ref())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y));
    diff == 0
}

/// The token of an `Authorization: Bearer <token>` value.
pub fn bearer(header: &str) -> Option<&str> {
    let (scheme, token) = header.trim().split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_tokens_are_32_random_bytes_in_base64url() {
        let (a, b) = (generate(), generate());
        assert_ne!(a, b);
        assert_eq!(URL_SAFE_NO_PAD.decode(&a).unwrap().len(), 32);
        assert!(
            a.bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        );
    }

    #[test]
    fn tokens_compare_equal_only_when_identical() {
        assert!(same("abc", "abc"));
        assert!(!same("abc", "abd"));
        assert!(!same("", "abc"));
        assert!(!same("abcd", "abc"));
    }

    #[test]
    fn the_bearer_scheme_is_parsed() {
        assert_eq!(bearer("Bearer abc"), Some("abc"));
        assert_eq!(bearer("bearer  abc "), Some("abc"));
        assert_eq!(bearer("Basic abc"), None);
        assert_eq!(bearer("abc"), None);
    }

    #[test]
    fn token_files_are_trimmed_and_must_not_be_empty() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("token");
        std::fs::write(&file, "s3cret\r\n").unwrap();
        assert_eq!(read_file(&file).unwrap(), "s3cret");
        std::fs::write(&file, " \n").unwrap();
        assert!(read_file(&file).unwrap_err().contains("is empty"));
        assert!(read_file(&dir.path().join("missing")).is_err());
    }
}
