//! Reading a declared plugin's component and manifest from its source:
//! a workspace path, or the cache of a URL source.

use std::path::{Path, PathBuf};

use rustman_core::secrets::{STATE_DIR, ensure_private, write_private};

use crate::error::LoadError;
use crate::grant::normalize_relative;
use crate::manifest::{MANIFEST_FILE, MANIFEST_SECTION};

/// The cache of downloaded plugins, relative to the workspace root.
pub const CACHE_DIR: &str = ".rustman/plugins";

/// The component a folder source holds when its manifest names none.
pub const DEFAULT_COMPONENT: &str = "plugin.wasm";

/// A plugin's bytes and manifest text, before any check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Package {
    pub wasm: Vec<u8>,
    pub manifest: String,
    /// The manifest came from a `rustman-plugin.toml` file next to the
    /// component, which the sha256 pin does not cover.
    pub sidecar: bool,
}

impl Package {
    /// The SHA-256 of the manifest when it came from a file next to the
    /// component (what the trust fingerprint covers besides the pin);
    /// empty when the component embeds it.
    pub fn manifest_digest(&self) -> String {
        if self.sidecar {
            crate::pin::sha256_hex(self.manifest.as_bytes())
        } else {
            String::new()
        }
    }
}

/// Where a URL source with pin `sha256` is cached.
pub fn cache_path(root: &Path, sha256: &str) -> PathBuf {
    root.join(CACHE_DIR).join(format!("{sha256}.wasm"))
}

/// The cached download of pin `sha256`; `None` when it is not cached.
pub fn read_cache(root: &Path, plugin: &str, sha256: &str) -> Result<Option<Vec<u8>>, LoadError> {
    let path = cache_path(root, sha256);
    match std::fs::read(&path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(LoadError::io(plugin, &path, &e)),
    }
}

/// Save a verified download in the cache, which must be a real,
/// git-ignored folder.
pub fn write_cache(root: &Path, plugin: &str, sha256: &str, wasm: &[u8]) -> Result<(), LoadError> {
    ensure_private(root, CACHE_DIR)
        .map_err(|e| LoadError::invalid(plugin, format!("{STATE_DIR}: {e}")))?;
    let path = cache_path(root, sha256);
    write_private(&path, wasm).map_err(|e| LoadError::io(plugin, &path, &e))
}

/// Read a path source: a `.wasm` file (manifest embedded, or next to it),
/// or a folder holding `rustman-plugin.toml` and the component it names.
pub fn read_path(root: &Path, plugin: &str, path: &str) -> Result<Package, LoadError> {
    let full = root.join(path);
    if full.is_dir() {
        return read_folder(plugin, &full);
    }
    let wasm = read(plugin, &full)?;
    let (manifest, sidecar) = match embedded_manifest(&wasm) {
        Some(text) => (text, false),
        None => (
            read_text(plugin, &full.with_file_name(MANIFEST_FILE))?,
            true,
        ),
    };
    Ok(Package {
        wasm,
        manifest,
        sidecar,
    })
}

/// The component of a path source alone, for re-hashing: a `.wasm` file
/// needs no manifest; a folder's names its component.
pub fn read_path_wasm(root: &Path, plugin: &str, path: &str) -> Result<Vec<u8>, LoadError> {
    let full = root.join(path);
    if full.is_dir() {
        return read_folder(plugin, &full).map(|package| package.wasm);
    }
    read(plugin, &full)
}

fn read_folder(plugin: &str, dir: &Path) -> Result<Package, LoadError> {
    let manifest = read_text(plugin, &dir.join(MANIFEST_FILE))?;
    let component = component_field(&manifest).unwrap_or_else(|| DEFAULT_COMPONENT.to_owned());
    let parts = normalize_relative(&component)
        .ok()
        .filter(|parts| !parts.is_empty())
        .ok_or_else(|| {
            let message = format!("{MANIFEST_FILE}: component '{component}' escapes the plugin");
            LoadError::invalid(plugin, message)
        })?;
    let wasm = read(plugin, &dir.join(parts.join("/")))?;
    Ok(Package {
        wasm,
        manifest,
        sidecar: true,
    })
}

/// The manifest of a URL source: it must be embedded in the component.
pub fn package_from_download(plugin: &str, wasm: Vec<u8>) -> Result<Package, LoadError> {
    let manifest = embedded_manifest(&wasm).ok_or_else(|| {
        let message = format!("missing {MANIFEST_FILE} (a URL source must embed it)");
        LoadError::invalid(plugin, message)
    })?;
    Ok(Package {
        wasm,
        manifest,
        sidecar: false,
    })
}

fn component_field(manifest: &str) -> Option<String> {
    let table: toml::Table = toml::from_str(manifest).ok()?;
    table.get("component")?.as_str().map(str::to_owned)
}

fn read(plugin: &str, path: &Path) -> Result<Vec<u8>, LoadError> {
    std::fs::read(path).map_err(|e| LoadError::io(plugin, path, &e))
}

fn read_text(plugin: &str, path: &Path) -> Result<String, LoadError> {
    match std::fs::read_to_string(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(LoadError::invalid(
            plugin,
            format!("missing {MANIFEST_FILE}"),
        )),
        other => other.map_err(|e| LoadError::io(plugin, path, &e)),
    }
}

/// The manifest in the component's top-level custom section
/// [`MANIFEST_SECTION`], when there is one.
pub fn embedded_manifest(wasm: &[u8]) -> Option<String> {
    let mut rest = wasm.get(8..)?;
    while let Some((&id, after_id)) = rest.split_first() {
        let (size, after_size) = leb_u32(after_id)?;
        let size = usize::try_from(size).ok()?;
        let payload = after_size.get(..size)?;
        rest = &after_size[size..];
        if id != 0 {
            continue;
        }
        let (len, after_len) = leb_u32(payload)?;
        let len = usize::try_from(len).ok()?;
        let name = after_len.get(..len)?;
        if name == MANIFEST_SECTION.as_bytes() {
            return String::from_utf8(after_len[len..].to_vec()).ok();
        }
    }
    None
}

/// An unsigned LEB128 `u32` and the bytes after it.
fn leb_u32(bytes: &[u8]) -> Option<(u32, &[u8])> {
    let mut value: u32 = 0;
    for (i, &byte) in bytes.iter().enumerate().take(5) {
        value |= u32::from(byte & 0x7f) << (7 * i);
        if byte & 0x80 == 0 {
            return Some((value, &bytes[i + 1..]));
        }
    }
    None
}

/// `wasm` with `manifest` appended as a top-level custom section.
pub fn embed_manifest(wasm: &[u8], manifest: &str) -> Vec<u8> {
    let mut payload = leb(MANIFEST_SECTION.len());
    payload.extend_from_slice(MANIFEST_SECTION.as_bytes());
    payload.extend_from_slice(manifest.as_bytes());
    let mut out = wasm.to_vec();
    out.push(0);
    out.extend(leb(payload.len()));
    out.extend(payload);
    out
}

fn leb(mut value: usize) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An empty component: the preamble only.
    const EMPTY: [u8; 8] = [0, b'a', b's', b'm', 0x0d, 0, 1, 0];

    #[test]
    fn manifests_embed_and_read_back() {
        assert_eq!(embedded_manifest(&EMPTY), None);
        let long = "x".repeat(300);
        let wasm = embed_manifest(&EMPTY, &long);
        assert_eq!(embedded_manifest(&wasm).as_deref(), Some(long.as_str()));
        assert_eq!(embedded_manifest(&wasm[..wasm.len() - 1]), None);
    }

    #[test]
    fn folder_sources_read_the_named_component() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = dir.path().join("plugins/hmac");
        std::fs::create_dir_all(plugin.join("out")).unwrap();
        std::fs::write(plugin.join(MANIFEST_FILE), "component = \"out/h.wasm\"\n").unwrap();
        std::fs::write(plugin.join("out/h.wasm"), EMPTY).unwrap();
        let package = read_path(dir.path(), "hmac", "plugins/hmac").unwrap();
        assert_eq!(package.wasm, EMPTY);
        std::fs::write(plugin.join(MANIFEST_FILE), "component = \"../../x.wasm\"\n").unwrap();
        let err = read_path(dir.path(), "hmac", "plugins/hmac").unwrap_err();
        assert_eq!(
            err.to_string(),
            "plugin hmac: rustman-plugin.toml: component '../../x.wasm' escapes the plugin"
        );
        let err = read_path(dir.path(), "hmac", "plugins").unwrap_err();
        assert_eq!(err.to_string(), "plugin hmac: missing rustman-plugin.toml");
    }

    #[test]
    fn file_sources_prefer_the_embedded_manifest() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("p.wasm"), EMPTY).unwrap();
        std::fs::write(dir.path().join(MANIFEST_FILE), "next = 1\n").unwrap();
        assert_eq!(
            read_path(dir.path(), "p", "p.wasm").unwrap().manifest,
            "next = 1\n"
        );
        std::fs::write(dir.path().join("p.wasm"), embed_manifest(&EMPTY, "inside")).unwrap();
        assert_eq!(
            read_path(dir.path(), "p", "p.wasm").unwrap().manifest,
            "inside"
        );
        let err = package_from_download("p", EMPTY.to_vec()).unwrap_err();
        assert!(err.to_string().contains("must embed it"));
    }
}
