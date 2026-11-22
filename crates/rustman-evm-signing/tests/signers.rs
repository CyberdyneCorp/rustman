//! Every signer type (task 5.1): keystores decrypted with a configured or
//! prompted password, keys imported into a stand-in keychain, hardware
//! wallets refused without their drivers. The keystores hold Anvil's
//! well-known dev accounts, encrypted with light scrypt parameters.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use rustman_core::Secret;
use rustman_core::secrets::{Keychain, KeychainError};
use rustman_evm::EvmSettings;
use rustman_evm_signing::import::{import_key, imported_message};
use rustman_evm_signing::signer::{keychain_account, keystore_path};
use rustman_evm_signing::{RAW_KEY_REFUSED, Signers, UnlockContext, UnlockedSigner, unlock};
use zeroize::Zeroizing;

const ANVIL0: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";
const ANVIL1: &str = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8";
const ANVIL1_KEY: &str = "0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d";
const PASSWORD: &str = "rustman-test-password";

static FIXTURES: LazyLock<PathBuf> =
    LazyLock::new(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures"));

/// An in-memory keychain.
#[derive(Default)]
struct MemoryKeychain(Mutex<BTreeMap<String, String>>);

impl Keychain for MemoryKeychain {
    fn get(&self, account: &str) -> Result<Option<String>, KeychainError> {
        Ok(self.0.lock().unwrap().get(account).cloned())
    }
    fn set(&self, account: &str, value: &str) -> Result<(), KeychainError> {
        self.0.lock().unwrap().insert(account.into(), value.into());
        Ok(())
    }
    fn delete(&self, account: &str) -> Result<bool, KeychainError> {
        Ok(self.0.lock().unwrap().remove(account).is_some())
    }
}

fn signers(toml_text: &str) -> Signers {
    let table: toml::Table = toml::from_str(toml_text).unwrap();
    Signers::from_settings(&EvmSettings::from_table(&table).unwrap()).unwrap()
}

type Prompted = Result<Option<Zeroizing<String>>, String>;

fn no_prompt(_: &str) -> Prompted {
    Ok(None)
}

fn context<'a>(
    password: Option<&str>,
    prompt: &'a dyn Fn(&str) -> Prompted,
    keychain: Option<&'a dyn Keychain>,
) -> UnlockContext<'a> {
    UnlockContext {
        root: &FIXTURES,
        password: password.map(|p| Secret::new("deployer_pw", Zeroizing::new(p.to_owned()))),
        prompt,
        keychain,
    }
}

fn unlock_id(
    signers: &Signers,
    id: &str,
    ctx: &UnlockContext<'_>,
) -> Result<UnlockedSigner, rustman_evm::EvmError> {
    unlock(signers.get(id).unwrap(), ctx)
}

const KEYSTORES: &str = "[signers.deployer]\nkeystore = \"keystores/anvil0.json\"\npassword = \"{{secret:deployer_pw}}\"\n\
                         [signers.prompted]\nkeystore = \"keystores/anvil1.json\"\n";

#[test]
fn a_keystore_decrypts_with_the_configured_password() {
    let signers = signers(KEYSTORES);
    let signer = unlock_id(
        &signers,
        "deployer",
        &context(Some(PASSWORD), &no_prompt, None),
    )
    .unwrap();
    assert_eq!(signer.address().to_checksum(None), ANVIL0);
    assert_eq!(signer.id(), "deployer");
    let shown = format!("{signer:?}");
    assert!(!shown.contains(PASSWORD), "{shown}");
}

#[test]
fn a_keystore_password_is_prompted_without_one_configured() {
    let signers = signers(KEYSTORES);
    let asked = Mutex::new(Vec::new());
    let prompt = |question: &str| {
        asked.lock().unwrap().push(question.to_owned());
        Ok(Some(Zeroizing::new(PASSWORD.to_owned())))
    };
    let signer = unlock_id(&signers, "prompted", &context(None, &prompt, None)).unwrap();
    assert_eq!(signer.address().to_checksum(None), ANVIL1);
    assert_eq!(
        *asked.lock().unwrap(),
        ["password for keystore signer 'prompted': "]
    );
}

#[test]
fn keystore_failures_name_the_signer_and_never_the_password() {
    let signers = signers(KEYSTORES);
    let wrong = unlock_id(
        &signers,
        "deployer",
        &context(Some("nope"), &no_prompt, None),
    )
    .unwrap_err();
    assert_eq!(
        wrong.to_string(),
        "signer 'deployer': wrong keystore password"
    );
    assert_eq!(wrong.exit_code(), 4);
    let headless = unlock_id(&signers, "prompted", &context(None, &no_prompt, None)).unwrap_err();
    assert_eq!(
        headless.to_string(),
        "signer 'prompted': the keystore password is needed; set password = \"{{secret:...}}\" in [evm.signers.prompted] or run in a terminal"
    );
    assert_eq!(headless.exit_code(), 4);
    // Regression: a terminal that could not be read was reported as
    // "no terminal", hiding the I/O error.
    let broken = |_: &str| -> Prompted { Err("Input/output error (os error 5)".to_owned()) };
    let err = unlock_id(&signers, "prompted", &context(None, &broken, None)).unwrap_err();
    assert_eq!(
        err.to_string(),
        "signer 'prompted': cannot read the keystore password: Input/output error (os error 5)"
    );
    assert_eq!(err.exit_code(), 2);
    let missing = signers_with_path("keystores/missing.json");
    let err = unlock_id(&missing, "x", &context(Some(PASSWORD), &no_prompt, None)).unwrap_err();
    assert!(
        err.to_string().starts_with("signer 'x': keystore "),
        "{err}"
    );
    assert!(err.to_string().ends_with("missing.json not found"), "{err}");
    assert_eq!(err.exit_code(), 2);
}

fn signers_with_path(path: &str) -> Signers {
    signers(&format!("[signers.x]\nkeystore = \"{path}\"\n"))
}

#[test]
fn keystore_paths_expand_home_and_start_at_the_root() {
    let root = Path::new("/workspace");
    assert_eq!(keystore_path(root, "keys/a.json"), root.join("keys/a.json"));
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    if let Some(home) = home {
        assert_eq!(
            keystore_path(root, "~/.foundry/keystores/deployer"),
            Path::new(&home).join(".foundry/keystores/deployer")
        );
    }
}

#[test]
fn an_imported_key_unlocks_from_the_keychain() {
    let keychain = MemoryKeychain::default();
    let key = Zeroizing::new(format!("  {ANVIL1_KEY}\n"));
    let address = import_key(&keychain, "ci", &key).unwrap();
    assert_eq!(address.to_checksum(None), ANVIL1);
    let stored = keychain.get(&keychain_account("ci")).unwrap().unwrap();
    assert_eq!(stored, ANVIL1_KEY, "stored normalized, under evm-signer/ci");
    let message = imported_message("ci", address);
    assert!(!message.contains(&ANVIL1_KEY[2..]), "{message}");
    assert!(
        message.contains("[evm.signers.ci]\n  keychain = true"),
        "{message}"
    );

    let signers = signers("[signers.ci]\nkeychain = true\n");
    let ctx = context(None, &no_prompt, Some(&keychain));
    let signer = unlock_id(&signers, "ci", &ctx).unwrap();
    assert_eq!(signer.address().to_checksum(None), ANVIL1);
}

#[test]
fn keychain_signers_need_an_entry_and_a_keychain() {
    let signers = signers("[signers.ci]\nkeychain = true\n");
    let empty = MemoryKeychain::default();
    let err = unlock_id(&signers, "ci", &context(None, &no_prompt, Some(&empty))).unwrap_err();
    assert_eq!(
        err.to_string(),
        "signer 'ci' is not in the OS keychain; run 'rustman evm signer import ci'"
    );
    let err = unlock_id(&signers, "ci", &context(None, &no_prompt, None)).unwrap_err();
    assert_eq!(err.to_string(), "signer 'ci': no OS keychain is available");
}

#[test]
fn import_refuses_what_is_not_a_key_and_raw_key_ids() {
    let keychain = MemoryKeychain::default();
    let not_a_key = Zeroizing::new("0x1234".to_owned());
    let err = import_key(&keychain, "ci", &not_a_key).unwrap_err();
    assert_eq!(
        err.to_string(),
        "not a private key: expected 64 hex digits, with or without 0x"
    );
    let zero = Zeroizing::new(format!("0x{}", "0".repeat(64)));
    let err = import_key(&keychain, "ci", &zero).unwrap_err();
    assert_eq!(err.to_string(), "signer 'ci': not a valid secp256k1 key");
    let key = Zeroizing::new(ANVIL1_KEY.to_owned());
    let err = import_key(&keychain, ANVIL1_KEY, &key).unwrap_err();
    assert_eq!(err.to_string(), RAW_KEY_REFUSED);
    assert_eq!(err.exit_code(), 4);
    assert!(keychain.0.lock().unwrap().is_empty());
}

#[test]
fn hardware_wallets_fail_clearly_in_this_build() {
    let signers = signers(
        "[signers.cold]\nledger = \"m/44'/60'/0'/0/0\"\n[signers.vault]\ntrezor = \"m/44'/60'/0'/0/1\"\n",
    );
    let ctx = context(None, &no_prompt, None);
    let ledger = unlock_id(&signers, "cold", &ctx).unwrap_err();
    let trezor = unlock_id(&signers, "vault", &ctx).unwrap_err();
    let expected = |id: &str, device: &str| {
        format!(
            "signer '{id}': {device} signing is not available yet; use a keystore or keychain signer"
        )
    };
    assert_eq!(ledger.to_string(), expected("cold", "Ledger"));
    assert_eq!(trezor.to_string(), expected("vault", "Trezor"));
    assert_eq!(ledger.exit_code(), 2);
}

#[test]
fn a_keystore_with_a_costly_key_derivation_is_refused_before_running_it() {
    // Regression: the scrypt and pbkdf2 parameters were used as written,
    // so a hostile keystore (n = 2^30) hung the process.
    let dir = tempfile::tempdir().unwrap();
    let fixture = std::fs::read_to_string(FIXTURES.join("keystores/anvil0.json")).unwrap();
    let mut json: serde_json::Value = serde_json::from_str(&fixture).unwrap();
    let hostile = [
        (
            "scrypt",
            serde_json::json!({"dklen": 32, "n": 1u64 << 30, "r": 8, "p": 1, "salt": "01"}),
        ),
        (
            "scrypt",
            serde_json::json!({"dklen": 32, "n": 1024, "r": 64, "p": 1, "salt": "01"}),
        ),
        (
            "scrypt",
            serde_json::json!({"dklen": 32, "n": 1u64 << 20, "r": 8, "p": 16, "salt": "01"}),
        ),
        // Above geth's standard n = 2^18 (2^19 is 512 MiB of memory).
        (
            "scrypt",
            serde_json::json!({"dklen": 32, "n": 1u64 << 19, "r": 8, "p": 1, "salt": "01"}),
        ),
        (
            "scrypt",
            serde_json::json!({"dklen": 32, "n": 1u64 << 14, "r": 8, "p": 8, "salt": "01"}),
        ),
        (
            "pbkdf2",
            serde_json::json!({"dklen": 32, "c": 4_000_000_000u64, "prf": "hmac-sha256", "salt": "01"}),
        ),
    ];
    let set = signers("[signers.evil]\nkeystore = \"evil.json\"\n");
    for (kdf, params) in hostile {
        json["crypto"]["kdf"] = serde_json::json!(kdf);
        json["crypto"]["kdfparams"] = params.clone();
        std::fs::write(dir.path().join("evil.json"), json.to_string()).unwrap();
        let mut ctx = context(Some(PASSWORD), &no_prompt, None);
        ctx.root = dir.path();
        let started = std::time::Instant::now();
        let err = unlock(set.get("evil").unwrap(), &ctx).unwrap_err();
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "{params}"
        );
        assert_eq!(err.exit_code(), 2, "{err}");
        assert!(
            err.to_string().contains("above the accepted limits"),
            "{params}: {err}"
        );
    }
}

/// Unlock `evil` from a copy of the fixture keystore whose `crypto` is
/// changed by `edit`.
fn unlock_edited(edit: impl FnOnce(&mut serde_json::Value)) -> rustman_evm::EvmError {
    let dir = tempfile::tempdir().unwrap();
    let fixture = std::fs::read_to_string(FIXTURES.join("keystores/anvil0.json")).unwrap();
    let mut json: serde_json::Value = serde_json::from_str(&fixture).unwrap();
    edit(&mut json["crypto"]);
    std::fs::write(dir.path().join("evil.json"), json.to_string()).unwrap();
    let set = signers("[signers.evil]\nkeystore = \"evil.json\"\n");
    let mut ctx = context(Some(PASSWORD), &no_prompt, None);
    ctx.root = dir.path();
    let started = std::time::Instant::now();
    let err = unlock(set.get("evil").unwrap(), &ctx).unwrap_err();
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    err
}

#[test]
fn a_keystore_cannot_hide_a_costly_derivation_behind_another_kdf() {
    // Regression: the limits were picked from `kdf`, but the decryption
    // picks the derivation from the shape of `kdfparams`: `kdf: scrypt`
    // with a pbkdf2 `c` of u32::MAX ran pbkdf2 for hours, and a dklen
    // below 32 made the decryption panic.
    let hostile = [
        serde_json::json!({"kdf": "scrypt", "kdfparams": {"c": 4_294_967_295u64, "dklen": 32, "prf": "hmac-sha256", "salt": "00", "n": 2, "r": 1, "p": 1}}),
        serde_json::json!({"kdf": "pbkdf2", "kdfparams": {"dklen": 32, "n": 1u64 << 30, "r": 8, "p": 1, "salt": "00"}}),
        serde_json::json!({"kdf": "scrypt", "kdfparams": {"dklen": 16, "n": 2, "r": 1, "p": 1, "salt": "00"}}),
        serde_json::json!({"kdf": "scrypt", "kdfparams": {"n": 2, "r": 1, "p": 1, "salt": "00"}}),
        serde_json::json!({"kdf": "pbkdf2", "kdfparams": {"dklen": 32, "c": "4294967295", "prf": "hmac-sha256", "salt": "00"}}),
        serde_json::json!({"kdf": "argon2", "kdfparams": {"dklen": 32, "salt": "00"}}),
    ];
    for crypto in hostile {
        let err = unlock_edited(|c| {
            c["kdf"] = crypto["kdf"].clone();
            c["kdfparams"] = crypto["kdfparams"].clone();
        });
        assert_eq!(err.exit_code(), 2, "{crypto}: {err}");
        assert!(
            err.to_string().contains("above the accepted limits"),
            "{crypto}: {err}"
        );
    }
}

#[test]
fn an_unreadable_keystore_fails_closed() {
    // Regression: a keystore the check could not parse skipped the check.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("evil.json"), "{not json").unwrap();
    std::fs::write(dir.path().join("big.json"), vec![b' '; 70 * 1024]).unwrap();
    for (id, file) in [("evil", "evil.json"), ("big", "big.json")] {
        let set = signers(&format!("[signers.{id}]\nkeystore = \"{file}\"\n"));
        let mut ctx = context(Some(PASSWORD), &no_prompt, None);
        ctx.root = dir.path();
        let err = unlock(set.get(id).unwrap(), &ctx).unwrap_err();
        assert_eq!(err.exit_code(), 2, "{err}");
        assert!(err.to_string().contains("unreadable keystore"), "{err}");
    }
}
