//! The descriptor cache (design D4): descriptors fetched by reflection are
//! kept at `.rustman/grpc/descriptors/<sha256(target)>.bin` with a JSON
//! sidecar recording the target and when they were fetched, and reused
//! until refreshed.

use std::fs;
use std::path::{Path, PathBuf};

use prost_reflect::DescriptorPool;
use rustman_core::Redactor;
use rustman_core::run::now_rfc3339;
use serde::{Deserialize, Serialize};

use crate::error::GrpcError;

/// The cache folder, relative to the workspace root.
pub const DESCRIPTOR_CACHE_DIR: &str = ".rustman/grpc/descriptors";

/// The version of the sidecar layout.
const CACHE_FORMAT: u32 = 1;

/// The sidecar of a cached descriptor set.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheMeta {
    pub format: u32,
    /// The canonical target, with any secret redacted.
    pub target: String,
    /// RFC 3339 UTC.
    pub fetched_at: String,
}

/// The descriptor cache of one workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DescriptorCache {
    dir: PathBuf,
}

impl DescriptorCache {
    /// The cache under the workspace at `root`.
    pub fn for_workspace(root: &Path) -> Self {
        Self::at(root.join(DESCRIPTOR_CACHE_DIR))
    }

    /// A cache in `dir`.
    pub fn at(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The files of `key` (a canonical target): `<hash>.bin` and
    /// `<hash>.json`. The hash keeps secrets out of file names.
    fn paths(&self, key: &str) -> (PathBuf, PathBuf) {
        let digest = ring::digest::digest(&ring::digest::SHA256, key.as_bytes());
        let name: String = digest.as_ref().iter().map(|b| format!("{b:02x}")).collect();
        (
            self.dir.join(format!("{name}.bin")),
            self.dir.join(format!("{name}.json")),
        )
    }

    /// The cached descriptors of `key`, if any can be read.
    pub fn read(&self, key: &str) -> Option<(DescriptorPool, CacheMeta)> {
        let (bin, json) = self.paths(key);
        let meta: CacheMeta = serde_json::from_slice(&fs::read(json).ok()?).ok()?;
        if meta.format != CACHE_FORMAT {
            return None;
        }
        let pool = DescriptorPool::decode(fs::read(bin).ok()?.as_slice()).ok()?;
        Some((pool, meta))
    }

    /// Store `pool` as the descriptors of `key`, fetched now. The sidecar
    /// shows `key` through `redactor`.
    pub fn write(
        &self,
        key: &str,
        pool: &DescriptorPool,
        redactor: &Redactor,
    ) -> Result<CacheMeta, GrpcError> {
        let failed = |e: std::io::Error| {
            GrpcError::Schema(format!("cannot write the descriptor cache: {e}"))
        };
        fs::create_dir_all(&self.dir).map_err(failed)?;
        let meta = CacheMeta {
            format: CACHE_FORMAT,
            target: redactor.text(key).into_owned(),
            fetched_at: now_rfc3339(),
        };
        let (bin, json) = self.paths(key);
        fs::write(bin, pool.encode_to_vec()).map_err(failed)?;
        let text = serde_json::to_vec_pretty(&meta).unwrap_or_default();
        fs::write(json, text).map_err(failed)?;
        Ok(meta)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustman_core::Secret;

    #[test]
    fn descriptors_round_trip_and_names_hide_the_target() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DescriptorCache::for_workspace(dir.path());
        let key = "grpc://s3cr3t.internal:50051";
        assert!(cache.read(key).is_none());
        let pool = DescriptorPool::global();
        let redactor = Redactor::new().with(&Secret::new("host", "s3cr3t.internal".to_owned()));
        let meta = cache.write(key, &pool, &redactor).unwrap();
        assert_eq!(meta.target, "grpc://«redacted:host»:50051");
        let (read, read_meta) = cache.read(key).unwrap();
        assert_eq!(read_meta, meta);
        assert!(
            read.get_message_by_name("google.protobuf.Timestamp")
                .is_some()
        );
        for entry in fs::read_dir(cache.dir()).unwrap() {
            let path = entry.unwrap().path();
            assert!(!path.to_string_lossy().contains("s3cr3t"));
            let text = String::from_utf8_lossy(&fs::read(&path).unwrap()).into_owned();
            assert!(!text.contains("s3cr3t"), "{}", path.display());
        }
        assert!(cache.dir().starts_with(dir.path().join(".rustman")));
    }
}
