//! `sops`: SOPS-encrypted YAML or JSON files inside the workspace, with
//! `<path>` the workspace-relative file and `#field` a dotted key path.
//! The whole decrypted document is the raw secret; the field is selected
//! from it like any structured secret.

mod keys;
pub mod tree;

use std::path::Path;

use rustman_core::format::from_yaml;
use rustman_core::paths;
use rustman_core::secret::Secret;
use rustman_core::secrets::provider::{FetchFuture, ProviderError, SecretProvider};
use serde_json::{Map, Value};

pub use keys::{AGE_KEY_FILE_VAR, AGE_KEY_VAR, GPG_NOT_FOUND};
use tree::TreeError;

use crate::context::Context;

pub const PATH_ESCAPE: &str = "sops: path escapes the workspace";

pub struct Sops {
    ctx: Context,
    /// `gpg` on `PATH`, or `gpg_path` from the user config.
    gpg: String,
}

impl Sops {
    pub fn new(ctx: Context, gpg: Option<&str>) -> Self {
        Self {
            ctx,
            gpg: gpg.unwrap_or("gpg").to_owned(),
        }
    }

    async fn read(&self, path: &str) -> Result<String, ProviderError> {
        let root = self.ctx.root();
        let file = paths::contain(root, root, path)
            .map_err(|_| ProviderError::Unavailable(PATH_ESCAPE.to_owned()))?;
        let text = tokio::fs::read_to_string(&file)
            .await
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => {
                    ProviderError::NotFound(format!("sops: {path} not found"))
                }
                _ => ProviderError::Unavailable(format!("sops: cannot read {path}: {e}")),
            })?;
        let document = parse(path, &text)?;
        let sops = document
            .get("sops")
            .cloned()
            .ok_or_else(|| ProviderError::Config(format!("sops: {path} is not a SOPS file")))?;
        let key = keys::data_key(&self.ctx, &self.gpg, &sops, path).await?;
        let decrypted = tree::decrypt_document(&document, &sops, &key).map_err(|e| match e {
            TreeError::Malformed(m) => ProviderError::Config(format!("sops: {path}: {m}")),
            TreeError::Decrypt => ProviderError::Unavailable(format!(
                "sops: {path} does not decrypt with its data key"
            )),
            TreeError::MacMismatch => ProviderError::Unavailable(format!(
                "sops: MAC mismatch in {path} (the file was modified)"
            )),
        })?;
        Ok(decrypted.to_string())
    }
}

impl SecretProvider for Sops {
    fn fetch<'a>(&'a self, path: &'a str) -> FetchFuture<'a> {
        Box::pin(async move { Ok(Secret::new(path, self.read(path).await?)) })
    }
}

/// A SOPS file as a mapping: JSON for `.json` files, YAML otherwise.
fn parse(path: &str, text: &str) -> Result<Map<String, Value>, ProviderError> {
    let invalid = || ProviderError::Config(format!("sops: {path} is not a SOPS YAML or JSON file"));
    let value: Value = if path.ends_with(".json") {
        serde_json::from_str(text).map_err(|_| invalid())?
    } else {
        from_yaml(Path::new(path), text).map_err(|_| invalid())?
    };
    match value {
        Value::Object(map) => Ok(map),
        _ => Err(invalid()),
    }
}
