//! Helpers shared by the plugin host's integration tests.

#![allow(dead_code)]

use std::path::Path;
use std::sync::Arc;

use rustman_plugin::declare::{Declaration, Source};
use rustman_plugin::limits::Limits;
use rustman_plugin::pin::sha256_hex;
use rustman_plugin::sandbox::HostContext;
use rustman_plugin::source::Package;
use rustman_plugin::workspace::check;
use rustman_plugin::{Plugin, VerifiedPlugin};
use tempfile::TempDir;

/// A workspace in a temporary folder.
pub struct Ws {
    pub dir: TempDir,
}

impl Ws {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("rustman.toml"),
            "[workspace]\nname = \"w\"\nversion = 1\n",
        )
        .unwrap();
        Self { dir }
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    pub fn write(&self, rel: &str, bytes: impl AsRef<[u8]>) {
        let path = self.root().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    /// Append `[plugins.<name>]` to `rustman.toml`.
    pub fn declare(&self, name: &str, source: &str, sha256: &str, grants: &[&str]) {
        let grants: Vec<String> = grants.iter().map(|g| format!("\"{g}\"")).collect();
        let table = format!(
            "\n[plugins.{name}]\nsource = \"{source}\"\nsha256 = \"{sha256}\"\ngrants = [{}]\n",
            grants.join(", ")
        );
        let path = self.root().join("rustman.toml");
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str(&table);
        std::fs::write(path, text).unwrap();
    }
}

/// `wasm` with `manifest`, checked against a declaration with `grants`.
pub fn verified(name: &str, wasm: Vec<u8>, manifest: &str, grants: &[&str]) -> VerifiedPlugin {
    verified_with(name, wasm, manifest, grants, Limits::default())
}

pub fn verified_with(
    name: &str,
    wasm: Vec<u8>,
    manifest: &str,
    grants: &[&str],
    limits: Limits,
) -> VerifiedPlugin {
    let declaration = Declaration {
        name: name.to_owned(),
        source: Source::Path(format!("plugins/{name}.wasm")),
        sha256: sha256_hex(&wasm),
        grants: grants.iter().map(|g| (*g).to_owned()).collect(),
        limits,
    };
    let package = Package {
        wasm,
        manifest: manifest.to_owned(),
        sidecar: false,
    };
    check(declaration, package).unwrap()
}

/// A plugin ready to call in `context`.
pub fn plugin(verified: &VerifiedPlugin, context: HostContext) -> Plugin {
    Plugin::new(verified, Arc::new(context)).unwrap()
}
