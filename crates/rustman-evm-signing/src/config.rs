//! `[evm.signers.<id>]` in `rustman.toml` (evm-signing "Approved signers
//! only"): each signer is exactly one of
//!
//! ```toml
//! [evm.signers.deployer]
//! keystore = "~/.foundry/keystores/deployer"   # Web3 Secret Storage JSON
//! password = "{{secret:deployer_pw}}"          # optional; else prompted
//!
//! [evm.signers.ci]
//! keychain = true                              # `rustman evm signer import ci`
//!
//! [evm.signers.cold]
//! ledger = "m/44'/60'/0'/0/0"                  # or trezor = "..."; not available yet
//! ```
//!
//! A raw private key anywhere in the table is refused (exit code 4).

use std::collections::BTreeMap;

use rustman_evm::{EvmError, EvmSettings};
use serde::Deserialize;

use crate::rawkey::{RAW_KEY_REFUSED, is_raw_key};

/// Keys that would hold key material; never accepted, whatever their value.
const KEY_FIELDS: [&str; 5] = ["private_key", "key", "secret_key", "mnemonic", "seed"];

/// Where a signer's key lives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignerSource {
    /// An encrypted JSON keystore; `password` is the configured value as
    /// written (usually a `{{secret:...}}` reference), resolved by the caller.
    Keystore {
        path: String,
        password: Option<String>,
    },
    /// A key `rustman evm signer import` put in the OS keychain.
    Keychain,
    /// A hardware wallet with this derivation path (`hw-wallet` feature).
    Ledger {
        derivation_path: String,
    },
    Trezor {
        derivation_path: String,
    },
}

impl SignerSource {
    /// `keystore`, `keychain`, `ledger` or `trezor`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Keystore { .. } => "keystore",
            Self::Keychain => "keychain",
            Self::Ledger { .. } => "ledger",
            Self::Trezor { .. } => "trezor",
        }
    }
}

/// One configured signer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignerConfig {
    pub id: String,
    pub source: SignerSource,
}

/// `[evm.signers]`, read and checked.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Signers {
    by_id: BTreeMap<String, SignerConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSigner {
    keystore: Option<String>,
    password: Option<String>,
    #[serde(default)]
    keychain: bool,
    ledger: Option<String>,
    trezor: Option<String>,
}

impl Signers {
    /// Read `[evm.signers]` of `settings`.
    pub fn from_settings(settings: &EvmSettings) -> Result<Self, EvmError> {
        let mut by_id = BTreeMap::new();
        for (id, value) in &settings.signers {
            let config = parse_signer(id, value)?;
            by_id.insert(id.clone(), config);
        }
        Ok(Self { by_id })
    }

    /// The signer `id`, or a configuration error listing the known ones.
    /// The id asked for is never echoed: it may come from a flag, an
    /// environment variable or a variable, and hold key material.
    pub fn get(&self, id: &str) -> Result<&SignerConfig, EvmError> {
        self.by_id.get(id).ok_or_else(|| {
            let known: Vec<&str> = self.by_id.keys().map(String::as_str).collect();
            let known = match known.is_empty() {
                true => "none is configured in [evm.signers]".to_owned(),
                false => format!("configured: {}", known.join(", ")),
            };
            EvmError::config(format!("unknown signer ({known})"))
        })
    }

    /// Whether `id` is a configured signer.
    pub fn contains(&self, id: &str) -> bool {
        self.by_id.contains_key(id)
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.by_id.keys().map(String::as_str)
    }
}

fn parse_signer(id: &str, value: &toml::Value) -> Result<SignerConfig, EvmError> {
    let section = format!("[evm.signers.{id}]");
    check_id(id)?;
    let table = value
        .as_table()
        .ok_or_else(|| EvmError::config(format!("{section}: expected a table")))?;
    refuse_key_material(&section, table)?;
    let raw: RawSigner = toml::Value::Table(table.clone())
        .try_into()
        .map_err(|e: toml::de::Error| EvmError::config(format!("{section}: {}", e.message())))?;
    let source = source(&section, raw)?;
    Ok(SignerConfig {
        id: id.to_owned(),
        source,
    })
}

/// The refusal of an invalid signer id. The id is never echoed.
pub const INVALID_SIGNER_ID: &str =
    "invalid signer id: use 1 to 64 letters, digits, '-' or '_', not a hex string";

/// A signer id: 1 to 64 of `[A-Za-z0-9_-]`, and not hex-like (a `0x`
/// prefix, or 32 or more hex digits only), so an id can never be key
/// material. Key material is refused as such (exit code 4); any other
/// invalid id is a configuration error (exit code 2). The id is never
/// part of the error.
pub fn check_id(id: &str) -> Result<(), EvmError> {
    crate::rawkey::refuse_raw_key(id)?;
    let charset = id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'));
    let all_hex = id.bytes().all(|b| b.is_ascii_hexdigit());
    let hex_like = id.starts_with("0x") || id.starts_with("0X") || (all_hex && id.len() >= 32);
    match (1..=64).contains(&id.len()) && charset && !hex_like {
        true => Ok(()),
        false => Err(EvmError::config(INVALID_SIGNER_ID)),
    }
}

/// Refuse a key field, or any string value that is a raw key.
fn refuse_key_material(section: &str, table: &toml::Table) -> Result<(), EvmError> {
    let refused = || EvmError::Refused(format!("{section}: {RAW_KEY_REFUSED}"));
    if table.keys().any(|k| KEY_FIELDS.contains(&k.as_str())) {
        return Err(refused());
    }
    let raw = table
        .values()
        .filter_map(toml::Value::as_str)
        .any(is_raw_key);
    match raw {
        true => Err(refused()),
        false => Ok(()),
    }
}

fn source(section: &str, raw: RawSigner) -> Result<SignerSource, EvmError> {
    let RawSigner {
        keystore,
        password,
        keychain,
        ledger,
        trezor,
    } = raw;
    let chosen = [
        keystore.is_some(),
        keychain,
        ledger.is_some(),
        trezor.is_some(),
    ];
    if chosen.iter().filter(|c| **c).count() != 1 {
        return Err(EvmError::config(format!(
            "{section}: set exactly one of keystore, keychain = true, ledger or trezor"
        )));
    }
    if password.is_some() && keystore.is_none() {
        return Err(EvmError::config(format!(
            "{section}: password applies to keystore signers only"
        )));
    }
    Ok(match (keystore, ledger, trezor) {
        (Some(path), _, _) => SignerSource::Keystore { path, password },
        (_, Some(derivation_path), _) => SignerSource::Ledger { derivation_path },
        (_, _, Some(derivation_path)) => SignerSource::Trezor { derivation_path },
        _ => SignerSource::Keychain,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

    fn signers(text: &str) -> Result<Signers, EvmError> {
        let table: toml::Table = toml::from_str(text).unwrap();
        Signers::from_settings(&EvmSettings::from_table(&table)?)
    }

    #[test]
    fn every_signer_type_is_read() {
        let signers = signers(
            "[signers.deployer]\nkeystore = \"~/.foundry/keystores/deployer\"\npassword = \"{{secret:deployer_pw}}\"\n\
             [signers.ci]\nkeychain = true\n\
             [signers.cold]\nledger = \"m/44'/60'/0'/0/0\"\n\
             [signers.vault]\ntrezor = \"m/44'/60'/0'/0/1\"\n",
        )
        .unwrap();
        let kinds: Vec<_> = ["deployer", "ci", "cold", "vault"]
            .iter()
            .map(|id| signers.get(id).unwrap().source.kind())
            .collect();
        assert_eq!(kinds, ["keystore", "keychain", "ledger", "trezor"]);
        assert_eq!(
            signers.get("deployer").unwrap().source,
            SignerSource::Keystore {
                path: "~/.foundry/keystores/deployer".into(),
                password: Some("{{secret:deployer_pw}}".into()),
            }
        );
    }

    #[test]
    fn raw_keys_in_the_table_are_refused() {
        for text in [
            format!("[signers.a]\nprivate_key = \"{KEY}\"\n"),
            "[signers.a]\nkey = \"{{pk}}\"\n".to_owned(),
            format!("[signers.a]\nkeystore = \"{KEY}\"\n"),
            format!("[signers.a]\nkeystore = \"k.json\"\npassword = \"{KEY}\"\n"),
        ] {
            let err = signers(&text).unwrap_err();
            assert_eq!(err.exit_code(), 4, "{text}");
            assert_eq!(
                err.to_string(),
                format!("[evm.signers.a]: {RAW_KEY_REFUSED}")
            );
            assert!(!err.to_string().contains(&KEY[2..]));
        }
        let err = signers(&format!("[signers.{}]\nkeychain = true\n", &KEY[2..])).unwrap_err();
        assert_eq!(err.to_string(), RAW_KEY_REFUSED);
    }

    #[test]
    fn invalid_signers_are_configuration_errors() {
        let cases = [
            ("[signers.a]\n", "set exactly one of"),
            (
                "[signers.a]\nkeychain = true\nledger = \"m\"\n",
                "set exactly one of",
            ),
            (
                "[signers.a]\nkeychain = true\npassword = \"x\"\n",
                "keystore signers only",
            ),
            ("[signers.a]\nkeychain = true\nfoo = 1\n", "foo"),
            ("[signers.\"a b\"]\nkeychain = true\n", INVALID_SIGNER_ID),
            ("[signers.\"a.b\"]\nkeychain = true\n", INVALID_SIGNER_ID),
        ];
        for (text, expected) in cases {
            let err = signers(text).unwrap_err();
            assert_eq!(err.exit_code(), 2, "{text}");
            assert!(err.to_string().contains(expected), "{err}");
        }
    }

    #[test]
    fn signer_ids_are_plain_identifiers_never_echoed() {
        // Regression: a near-key, a mnemonic or a hex string passed as a
        // signer id was echoed in the error.
        for id in [
            "ci",
            "deploy-er_2",
            "A",
            &"x".repeat(64),
            "deadbeef",
            "0xyz",
        ] {
            let ok = check_id(id).is_ok();
            assert_eq!(ok, id != "0xyz", "{id}");
        }
        let hexish = KEY[2..40].to_owned();
        let refused = [
            "",
            "a b",
            "a.b",
            "x/y",
            &"x".repeat(65),
            &hexish,
            &KEY[..40],
            &KEY[2..63],
        ];
        for id in refused {
            let err = check_id(id).unwrap_err();
            assert!(matches!(err.exit_code(), 2 | 4), "{id}");
            if !id.is_empty() {
                assert!(!err.to_string().contains(id), "{id}: {err}");
            }
        }
    }

    #[test]
    fn unknown_signers_list_the_configured_ones() {
        let none = signers("").unwrap();
        assert_eq!(
            none.get("ci").unwrap_err().to_string(),
            "unknown signer (none is configured in [evm.signers])"
        );
        let some = signers("[signers.ci]\nkeychain = true\n").unwrap();
        assert_eq!(
            some.get("x").unwrap_err().to_string(),
            "unknown signer (configured: ci)"
        );
        assert!(some.contains("ci") && !some.contains("x"));
        assert_eq!(some.ids().collect::<Vec<_>>(), ["ci"]);
    }
}
