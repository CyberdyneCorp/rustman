//! Where message types come from (spec "Server reflection", "Local schema
//! sources", "Schema cache"): a local `.proto` compile, a descriptor set,
//! or server reflection with its on-disk cache.

mod cache;
mod local;
mod reflection;

use std::path::PathBuf;

use prost_reflect::DescriptorPool;
use rustman_core::Redactor;
use rustman_core::vars::ResolvedSchema;

pub use cache::{CacheMeta, DESCRIPTOR_CACHE_DIR, DescriptorCache};
pub use local::{compile_protos, load_descriptor_set};
pub use reflection::reflect;

use crate::connect::Remote;
use crate::error::GrpcError;

/// Where a [`Schema`] came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SchemaOrigin {
    /// Fetched from the server just now.
    Reflection,
    /// Read from the descriptor cache, fetched at that time (RFC 3339).
    Cache { fetched_at: String },
    /// Compiled from `.proto` files.
    Protos,
    /// Loaded from a descriptor set file.
    DescriptorSet,
}

/// The message types of a target, and where they came from.
#[derive(Clone, Debug)]
pub struct Schema {
    pub pool: DescriptorPool,
    pub origin: SchemaOrigin,
}

impl Schema {
    pub fn new(pool: DescriptorPool, origin: SchemaOrigin) -> Self {
        Self { pool, origin }
    }

    /// The notice shown with a schema read from the cache: it may be stale.
    pub fn notice(&self) -> Option<String> {
        match &self.origin {
            SchemaOrigin::Cache { fetched_at } => Some(format!(
                "schema from the descriptor cache (fetched {fetched_at}); it may be stale, refresh with `rustman grpc list --refresh`"
            )),
            _ => None,
        }
    }
}

/// The schema source a request names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SchemaSource {
    Protos {
        protos: Vec<PathBuf>,
        import_paths: Vec<PathBuf>,
    },
    /// The contained path, and the path as written (for messages).
    DescriptorSet {
        path: PathBuf,
        shown: String,
    },
    Reflection,
}

impl SchemaSource {
    /// The source of a resolved request's `schema`.
    pub fn of(schema: &ResolvedSchema) -> Self {
        if let Some(set) = &schema.descriptor_set {
            return Self::DescriptorSet {
                path: set.path.clone(),
                shown: set.raw.clone(),
            };
        }
        if schema.protos.is_empty() {
            return Self::Reflection;
        }
        let paths = |files: &[rustman_core::vars::ResolvedFile]| {
            files.iter().map(|f| f.path.clone()).collect::<Vec<_>>()
        };
        Self::Protos {
            protos: paths(&schema.protos),
            import_paths: paths(&schema.import_paths),
        }
    }
}

/// How reflection results are cached and refreshed.
#[derive(Clone, Debug, Default)]
pub struct CachePolicy {
    /// No cache: always ask the server, keep nothing.
    pub cache: Option<DescriptorCache>,
    /// Ask the server even when the cache has the target.
    pub refresh: bool,
    /// Shows the target in the cache sidecar.
    pub redactor: Redactor,
}

/// The schema `source` names; reflection asks `remote`, through the cache.
pub async fn load_schema(
    source: &SchemaSource,
    remote: &Remote,
    policy: &CachePolicy,
) -> Result<Schema, GrpcError> {
    match source {
        SchemaSource::Protos {
            protos,
            import_paths,
        } => Ok(Schema::new(
            compile_protos(protos, import_paths)?,
            SchemaOrigin::Protos,
        )),
        SchemaSource::DescriptorSet { path, shown } => Ok(Schema::new(
            load_descriptor_set(path, shown)?,
            SchemaOrigin::DescriptorSet,
        )),
        SchemaSource::Reflection => reflect_schema(remote, policy).await,
    }
}

/// The schema of `remote` by reflection: the cached one unless refreshing,
/// else the server's (then cached). When the server cannot be reached, a
/// cached schema stands in, with its stale notice.
pub async fn reflect_schema(remote: &Remote, policy: &CachePolicy) -> Result<Schema, GrpcError> {
    let key = remote.key();
    let cached = policy.cache.as_ref().and_then(|c| c.read(&key));
    if let Some((pool, meta)) = cached.clone().filter(|_| !policy.refresh) {
        return Ok(from_cache(pool, meta));
    }
    match fetch(remote).await {
        Ok(pool) => {
            if let Some(cache) = &policy.cache {
                cache.write(&key, &pool, &policy.redactor)?;
            }
            Ok(Schema::new(pool, SchemaOrigin::Reflection))
        }
        Err(e) if e.is_unreachable() => cached.map(|(p, m)| from_cache(p, m)).ok_or(e),
        Err(e) => Err(e),
    }
}

/// Reflection bounded by the remote's schema timeout: a server that
/// accepts the connection and never answers cannot hold the caller.
async fn fetch(remote: &Remote) -> Result<DescriptorPool, GrpcError> {
    let reflected = async {
        let channel = remote.channel().await?;
        reflect(channel, &remote.target.text).await
    };
    let limit = remote.schema_timeout;
    tokio::time::timeout(limit, reflected)
        .await
        .unwrap_or_else(|_| {
            Err(GrpcError::Reflection {
                target: remote.target.text.clone(),
                message: format!("no answer within {} ms", limit.as_millis()),
            })
        })
}

fn from_cache(pool: DescriptorPool, meta: CacheMeta) -> Schema {
    Schema::new(
        pool,
        SchemaOrigin::Cache {
            fetched_at: meta.fetched_at,
        },
    )
}
