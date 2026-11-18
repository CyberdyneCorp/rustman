//! Collections: folders of request files, with optional `_collection.yaml`
//! metadata and ordering.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::model::{Auth, NameValue, REQUEST_FILE_SUFFIX, Request, Variables};

pub const COLLECTION_FILE: &str = "_collection.yaml";

/// The contents of `_collection.yaml`.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollectionMeta {
    pub name: Option<String>,
    pub description: Option<String>,
    /// Entries (request file stems or folder names) to run first, in this order.
    pub order: Option<Vec<String>>,
    /// Headers inherited by every request inside.
    pub headers: Option<Vec<NameValue>>,
    /// Auth inherited by requests that set none.
    pub auth: Option<Auth>,
    pub variables: Option<Variables>,
    /// Run this folder's requests concurrently under `rustman run --parallel`
    /// instead of one after another.
    pub parallel: Option<bool>,
    /// The default data file of `rustman run` for this folder, relative to
    /// this file; it must stay inside the workspace.
    pub data: Option<String>,
    /// Data columns whose values are secrets, besides the ones named like
    /// secrets.
    pub secret_columns: Option<Vec<String>>,
}

/// A folder of requests. The workspace root is the top-level collection.
#[derive(Clone, Debug, PartialEq)]
pub struct Collection {
    /// Path relative to the workspace root; empty for the root.
    pub path: PathBuf,
    pub meta: CollectionMeta,
    /// Requests and sub-collections in run order.
    pub entries: Vec<Entry>,
}

impl Collection {
    /// The display name: `name` from `_collection.yaml`, else the folder name.
    pub fn name(&self) -> String {
        if let Some(name) = &self.meta.name {
            return name.clone();
        }
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// Sub-collections, depth first, in run order.
    pub fn descendants(&self) -> Vec<&Collection> {
        let mut out = Vec::new();
        for entry in &self.entries {
            if let Entry::Collection(child) = entry {
                out.push(child);
                out.extend(child.descendants());
            }
        }
        out
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    Request(RequestEntry),
    Collection(Collection),
}

impl Entry {
    /// The name `order:` refers to: a request file stem or a folder name.
    pub fn key(&self) -> String {
        match self {
            Self::Request(r) => r.stem(),
            Self::Collection(c) => c
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        }
    }
}

/// One loaded request file.
#[derive(Clone, Debug, PartialEq)]
pub struct RequestEntry {
    /// Path relative to the workspace root.
    pub path: PathBuf,
    pub request: Request,
}

impl RequestEntry {
    /// The file name without `.rman.yaml`.
    pub fn stem(&self) -> String {
        let name = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default();
        name.strip_suffix(REQUEST_FILE_SUFFIX)
            .unwrap_or(&name)
            .to_owned()
    }

    /// A stable id: the relative path with `/` separators and without `.rman.yaml`.
    pub fn id(&self) -> String {
        let parts: Vec<String> = self
            .path
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        let joined = parts.join("/");
        joined
            .strip_suffix(REQUEST_FILE_SUFFIX)
            .unwrap_or(&joined)
            .to_owned()
    }
}

/// Put the entries named in `order` first, then the rest in their existing
/// (file name) order. Returns the order names that matched nothing.
pub(crate) fn apply_order(entries: Vec<Entry>, order: &[String]) -> (Vec<Entry>, Vec<String>) {
    let mut rest = entries;
    let mut ordered = Vec::with_capacity(rest.len());
    let mut unmatched = Vec::new();
    for key in order {
        match rest.iter().position(|e| e.key() == *key) {
            Some(index) => ordered.push(rest.remove(index)),
            None => unmatched.push(key.clone()),
        }
    }
    ordered.extend(rest);
    (ordered, unmatched)
}

/// Directories under `parent` that are never collections.
pub(crate) fn is_tool_dir(parent: &Path, name: &str) -> bool {
    let is_root = parent.as_os_str().is_empty();
    name.starts_with('.') || name.starts_with('_') || (is_root && name == "environments")
}
