//! Raw private keys are refused wherever they appear (evm-signing
//! "Approved signers only"): request files, environments, variables, CLI
//! flags and environment variables. The one way in for a key is
//! `rustman evm signer import`, which moves it into the OS keychain.
//!
//! Detection is deliberately broad: anything that looks like key material
//! (a long hex run, a seed phrase) is refused, even when it is not quite a
//! valid key, and the refusal never echoes the value.

use rustman_evm::EvmError;

/// The refusal of a raw private key (exit code 4).
pub const RAW_KEY_REFUSED: &str =
    "raw private keys are not accepted; import the key with 'rustman evm signer import'";

/// The shortest hex run treated as key material: a 32-byte key is 64
/// digits; a few digits short or long is a key with a typo or padding.
const MIN_KEY_HEX: usize = 60;

/// The word counts of a BIP-39 seed phrase.
const PHRASE_WORDS: [usize; 5] = [12, 15, 18, 21, 24];

/// Whether `text` looks like key material: a run of at least 60 hex
/// digits (with or without `0x`) anywhere in it, or a seed phrase (12,
/// 15, 18, 21 or 24 lowercase words).
pub fn is_raw_key(text: &str) -> bool {
    has_long_hex_run(text) || is_seed_phrase(text)
}

fn has_long_hex_run(text: &str) -> bool {
    let mut run = 0;
    for b in text.bytes() {
        run = match b.is_ascii_hexdigit() {
            true => run + 1,
            false => 0,
        };
        if run >= MIN_KEY_HEX {
            return true;
        }
    }
    false
}

fn is_seed_phrase(text: &str) -> bool {
    let words: Vec<&str> = text.split_whitespace().collect();
    PHRASE_WORDS.contains(&words.len())
        && words
            .iter()
            .all(|w| (2..=8).contains(&w.len()) && w.bytes().all(|b| b.is_ascii_lowercase()))
}

/// Refuse `value` when it looks like key material. The value itself is
/// never part of the error.
pub fn refuse_raw_key(value: &str) -> Result<(), EvmError> {
    match is_raw_key(value) {
        true => Err(EvmError::Refused(RAW_KEY_REFUSED.to_owned())),
        false => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
    const PHRASE: &str = "test test test test test test test test test test test junk";

    #[test]
    fn sixty_four_hex_digits_are_a_raw_key() {
        for raw in [KEY.to_owned(), format!("0x{KEY}"), format!(" 0X{KEY}\n")] {
            assert!(is_raw_key(&raw));
            let err = refuse_raw_key(&raw).unwrap_err();
            assert_eq!(err.exit_code(), 4);
            assert_eq!(err.to_string(), RAW_KEY_REFUSED);
            assert!(!err.to_string().contains(KEY));
        }
    }

    #[test]
    fn near_keys_and_seed_phrases_are_key_material_too() {
        // Regression: only exactly 64 hex digits were refused, so a key
        // with a digit missing or extra padding, or a mnemonic, went on
        // and was echoed in the preview and in `unknown signer '...'`.
        let near = [
            KEY[..63].to_owned(),
            KEY[..60].to_owned(),
            format!("0x00{KEY}"),
            format!("{KEY}0"),
            format!("key:{KEY}"),
            format!("zz{}", &KEY[2..]),
            PHRASE.to_owned(),
            format!("  {PHRASE}\n"),
            "abandon ability able about above absent absorb abstract absurd abuse access accident acoustic acquire across".to_owned(),
            ["zoo"; 24].join(" "),
        ];
        for text in near {
            assert!(is_raw_key(&text), "{text}");
            let err = refuse_raw_key(&text).unwrap_err();
            assert!(!err.to_string().contains(text.trim()), "{text}");
        }
    }

    #[test]
    fn ids_addresses_and_paths_are_not() {
        let not_keys = [
            "deployer",
            "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266",
            "~/.foundry/keystores/deployer",
            &KEY[..59],
            "{{secret:deployer_pw}}",
            "m/44'/60'/0'/0/0",
            "test test test test test test test test test test test",
            "Test test test test test test test test test test test junk",
        ];
        for text in not_keys {
            assert!(!is_raw_key(text), "{text}");
            assert!(refuse_raw_key(text).is_ok());
        }
    }
}
