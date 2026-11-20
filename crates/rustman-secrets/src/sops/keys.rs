//! The data key of a SOPS file: from an `age` stanza with the user's age
//! identities, or from a `pgp` stanza through an installed `gpg`.

use std::io::Read;
use std::path::PathBuf;

use age::armor::ArmoredReader;
use age::{Decryptor, Identity, IdentityFile};
use rustman_core::secrets::provider::ProviderError;
use serde_json::Value;
use zeroize::Zeroizing;

use super::tree::DataKey;
use crate::context::Context;
use crate::process::{self, RunError};

/// Inline age identities.
pub const AGE_KEY_VAR: &str = "SOPS_AGE_KEY";
/// A file of age identities.
pub const AGE_KEY_FILE_VAR: &str = "SOPS_AGE_KEY_FILE";

pub const GPG_NOT_FOUND: &str = "sops: PGP decryption requires gpg, which was not found";

type Identities = Vec<Box<dyn Identity>>;

/// The `enc` values of the `kind` stanzas (`age` or `pgp`) in `sops`.
fn stanzas<'a>(sops: &'a Value, kind: &str) -> Vec<&'a str> {
    sops.get(kind)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|s| s.get("enc").and_then(Value::as_str))
        .collect()
}

/// The data key, from the first stanza the user can decrypt: age first,
/// then PGP (only when a PGP stanza exists and age gave nothing).
pub async fn data_key(
    ctx: &Context,
    gpg: &str,
    sops: &Value,
    file: &str,
) -> Result<DataKey, ProviderError> {
    let age = stanzas(sops, "age");
    // age identities are not `Send`: they are used, then dropped, before
    // any await.
    let (key, no_identity) = from_age(ctx, &age)?;
    if let Some(key) = key {
        return Ok(key);
    }
    let pgp = stanzas(sops, "pgp");
    for enc in &pgp {
        if let Some(key) = gpg_decrypt(ctx, gpg, enc).await? {
            return Ok(key);
        }
    }
    if no_identity && pgp.is_empty() {
        return Err(ProviderError::Auth(format!(
            "sops: no age identity found to decrypt {file} (set {AGE_KEY_FILE_VAR})"
        )));
    }
    Err(ProviderError::Auth(format!(
        "sops: none of the keys of {file} can be decrypted with the available identities"
    )))
}

/// The data key from the first age stanza the user's identities open,
/// and whether there were age stanzas but no identity at all.
fn from_age(ctx: &Context, age: &[&str]) -> Result<(Option<DataKey>, bool), ProviderError> {
    if age.is_empty() {
        return Ok((None, false));
    }
    let identities = identities(ctx)?;
    let key = age.iter().find_map(|enc| age_decrypt(enc, &identities));
    Ok((key, identities.is_empty()))
}

/// Every identity from `SOPS_AGE_KEY`, `SOPS_AGE_KEY_FILE` and the default
/// key file.
fn identities(ctx: &Context) -> Result<Identities, ProviderError> {
    let mut found: Identities = Vec::new();
    let invalid =
        |source: &str| ProviderError::Config(format!("sops: invalid age identities in {source}"));
    if let Some(inline) = ctx.var(AGE_KEY_VAR) {
        found.extend(parse_identities(inline.as_bytes()).ok_or_else(|| invalid(AGE_KEY_VAR))?);
    }
    let file = ctx
        .var(AGE_KEY_FILE_VAR)
        .map(PathBuf::from)
        .or_else(|| default_key_file(ctx));
    if let Some(file) = file
        && let Ok(text) = std::fs::read(&file).map(Zeroizing::new)
    {
        found.extend(parse_identities(&text).ok_or_else(|| invalid(&file.display().to_string()))?);
    }
    Ok(found)
}

fn parse_identities(text: &[u8]) -> Option<Identities> {
    IdentityFile::from_buffer(text).ok()?.into_identities().ok()
}

/// Where SOPS looks for age keys by default: the user config directory's
/// `sops/age/keys.txt`.
fn default_key_file(ctx: &Context) -> Option<PathBuf> {
    let base = if cfg!(windows) {
        ctx.var("APPDATA").map(PathBuf::from)
    } else if let Some(xdg) = ctx.var("XDG_CONFIG_HOME") {
        Some(PathBuf::from(xdg))
    } else if cfg!(target_os = "macos") {
        ctx.home()
            .map(|h| h.join("Library").join("Application Support"))
    } else {
        ctx.home().map(|h| h.join(".config"))
    };
    base.map(|b| b.join("sops").join("age").join("keys.txt"))
}

fn age_decrypt(enc: &str, identities: &Identities) -> Option<DataKey> {
    if identities.is_empty() {
        return None;
    }
    let decryptor = Decryptor::new(ArmoredReader::new(enc.as_bytes())).ok()?;
    let mut reader = decryptor
        .decrypt(identities.iter().map(|i| i.as_ref() as &dyn Identity))
        .ok()?;
    let mut key = Zeroizing::new(Vec::new());
    reader.read_to_end(&mut key).ok()?;
    Some(key)
}

/// The data key of a PGP stanza via `gpg --decrypt`; `None` when this
/// user's keyring cannot decrypt it.
async fn gpg_decrypt(
    ctx: &Context,
    gpg: &str,
    enc: &str,
) -> Result<Option<DataKey>, ProviderError> {
    let argv = ["--batch", "--quiet", "--decrypt"].map(str::to_owned);
    let argv: Vec<String> = std::iter::once(gpg.to_owned()).chain(argv).collect();
    let output = process::run(ctx, &argv, Some(enc.as_bytes()))
        .await
        .map_err(|e| match e {
            RunError::NotFound => ProviderError::Unavailable(GPG_NOT_FOUND.to_owned()),
            RunError::Failed(e) => ProviderError::Unavailable(format!("sops: gpg: {e}")),
        })?;
    Ok(output
        .success
        .then(|| Zeroizing::new(output.stdout.to_vec())))
}
