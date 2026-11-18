//! The schema cache (design D2): one file per endpoint at
//! `.rustman/graphql/schemas/<hash>.json`, holding the raw introspection
//! result (lossless), the endpoint, the fetch time and the SDL hash.
//!
//! The key is the SHA-256 of the normalized endpoint URL (scheme, host,
//! port, path; user info, query and fragment stripped), so environments
//! with different hosts do not collide. A cached schema is used until it is
//! refreshed explicitly; nothing here sends a request.

use std::path::{Path, PathBuf};

use rustman_core::Redactor;
use rustman_core::run::now_rfc3339;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::GraphqlError;
use crate::introspection;
use crate::schema::{GraphqlSchema, sha256_hex};

/// The cache folder, relative to the workspace root.
pub const SCHEMA_CACHE_DIR: &str = ".rustman/graphql/schemas";

/// The version of the cache file layout.
const CACHE_FORMAT: u32 = 1;

/// `endpoint` reduced to scheme, host, port and path.
pub fn normalize_endpoint(endpoint: &str) -> Result<String, GraphqlError> {
    let invalid = || GraphqlError::InvalidEndpoint(endpoint.to_owned());
    let url = url::Url::parse(endpoint).map_err(|_| invalid())?;
    let host = url.host_str().ok_or_else(invalid)?;
    let port = url
        .port_or_known_default()
        .map_or_else(String::new, |p| format!(":{p}"));
    Ok(format!("{}://{host}{port}{}", url.scheme(), url.path()))
}

/// The cache key of `endpoint`: the SHA-256 of its normalized form.
pub fn cache_key(endpoint: &str) -> Result<String, GraphqlError> {
    Ok(sha256_hex(normalize_endpoint(endpoint)?.as_bytes()))
}

/// A cache file as written on disk.
#[derive(Serialize, Deserialize)]
struct CacheFile {
    format: u32,
    /// The normalized endpoint, with any secret redacted.
    endpoint: String,
    /// RFC 3339 UTC.
    fetched_at: String,
    sdl_sha256: String,
    type_count: usize,
    /// `{"__schema": ...}` as the server returned it.
    introspection: Value,
}

/// A schema read from (or just written to) the cache.
#[derive(Clone, Debug)]
pub struct CachedSchema {
    pub path: PathBuf,
    /// The normalized endpoint, with any secret redacted.
    pub endpoint: String,
    pub fetched_at: String,
    pub sdl_sha256: String,
    pub schema: GraphqlSchema,
}

/// The schema cache of one workspace.
#[derive(Clone, Debug)]
pub struct SchemaCache {
    dir: PathBuf,
}

impl SchemaCache {
    /// The cache under `root/.rustman/graphql/schemas`.
    pub fn for_workspace(root: &Path) -> Self {
        Self::new(root.join(SCHEMA_CACHE_DIR))
    }

    /// A cache in `dir`.
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The file caching the schema of `endpoint`.
    pub fn path_for(&self, endpoint: &str) -> Result<PathBuf, GraphqlError> {
        Ok(self.dir.join(format!("{}.json", cache_key(endpoint)?)))
    }

    /// The cached schema of `endpoint`, or `None` when there is none.
    pub fn load(&self, endpoint: &str) -> Result<Option<CachedSchema>, GraphqlError> {
        let path = self.path_for(endpoint)?;
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(read_error(&path, &e.to_string())),
        };
        let file: CacheFile =
            serde_json::from_str(&text).map_err(|e| read_error(&path, &e.to_string()))?;
        if file.format != CACHE_FORMAT {
            let message = format!(
                "unsupported cache format {}; refresh the schema",
                file.format
            );
            return Err(read_error(&path, &message));
        }
        let schema = GraphqlSchema::from_introspection(&file.introspection, &file.endpoint)?;
        Ok(Some(CachedSchema {
            path,
            endpoint: file.endpoint,
            fetched_at: file.fetched_at,
            sdl_sha256: file.sdl_sha256,
            schema,
        }))
    }

    /// Cache `result` (an introspection result) as the schema of
    /// `endpoint`, replacing any earlier one. Every secret `redactor` knows
    /// is redacted from the file.
    pub fn store(
        &self,
        endpoint: &str,
        result: &Value,
        schema: GraphqlSchema,
        redactor: &Redactor,
    ) -> Result<CachedSchema, GraphqlError> {
        let path = self.path_for(endpoint)?;
        let schema_object = introspection::schema_object(result)
            .ok_or_else(|| GraphqlError::InvalidIntrospection("no __schema object".into()))?;
        let file = CacheFile {
            format: CACHE_FORMAT,
            endpoint: redactor.text(&normalize_endpoint(endpoint)?).into_owned(),
            fetched_at: now_rfc3339(),
            sdl_sha256: schema.sdl_sha256(),
            type_count: schema.type_count(),
            introspection: json!({ "__schema": schema_object }),
        };
        let text = serde_json::to_string_pretty(&file).expect("cache files serialize") + "\n";
        write_atomic(&path, &redactor.text(&text))?;
        Ok(CachedSchema {
            path,
            endpoint: file.endpoint,
            fetched_at: file.fetched_at,
            sdl_sha256: file.sdl_sha256,
            schema,
        })
    }
}

fn read_error(path: &Path, message: &str) -> GraphqlError {
    GraphqlError::Read {
        path: path.display().to_string(),
        message: message.to_owned(),
    }
}

/// Write `text` to a temporary file next to `path`, then rename it over
/// `path`, so a reader never sees half a file.
fn write_atomic(path: &Path, text: &str) -> Result<(), GraphqlError> {
    let failed = |e: std::io::Error| GraphqlError::Write {
        path: path.display().to_string(),
        message: e.to_string(),
    };
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(failed)?;
    let tmp = temporary_path(path);
    std::fs::write(&tmp, text).map_err(failed)?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        failed(e)
    })
}

/// A temporary name next to `path`, distinct per process and per write, so
/// concurrent refreshes (the TUI and the CLI, say) never share one.
fn temporary_path(path: &Path) -> PathBuf {
    static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}.{n}.tmp", std::process::id()));
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_normalize_to_scheme_host_port_and_path() {
        for (endpoint, normalized) in [
            (
                "https://api.example.com/graphql",
                "https://api.example.com:443/graphql",
            ),
            (
                "HTTPS://user:pw@API.example.com:443/graphql?token=x#frag",
                "https://api.example.com:443/graphql",
            ),
            ("http://localhost:4000", "http://localhost:4000/"),
            ("ws://h/sub", "ws://h:80/sub"),
        ] {
            assert_eq!(normalize_endpoint(endpoint).unwrap(), normalized);
        }
        assert_eq!(
            normalize_endpoint("not a url").unwrap_err().to_string(),
            "graphql: invalid endpoint URL 'not a url'"
        );
    }

    #[test]
    fn every_write_gets_its_own_temporary_file() {
        let path = Path::new("cache").join("entry.json");
        let (a, b) = (temporary_path(&path), temporary_path(&path));
        assert_ne!(a, b);
        assert_eq!(a.parent(), path.parent());
        let name = a.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("entry.json."), "{name}");
        assert!(name.contains(&std::process::id().to_string()), "{name}");
    }

    /// Windows may refuse a rename while another one replaces the same
    /// file; the distinct temporary names are tested above on every OS.
    #[cfg(unix)]
    #[test]
    fn concurrent_writes_of_one_entry_never_collide() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("schemas").join("entry.json");
        let texts: Vec<String> = (0..8).map(|i| format!("{{\"writer\":{i}}}")).collect();
        std::thread::scope(|scope| {
            for text in &texts {
                let path = &path;
                scope.spawn(move || {
                    for _ in 0..25 {
                        write_atomic(path, text).unwrap();
                    }
                });
            }
        });
        let last = std::fs::read_to_string(&path).unwrap();
        assert!(texts.contains(&last), "{last}");
        let left = std::fs::read_dir(path.parent().unwrap()).unwrap().count();
        assert_eq!(left, 1, "no temporary file is left behind");
    }

    #[test]
    fn hosts_get_distinct_keys_and_queries_do_not() {
        let a = cache_key("https://staging.example.com/graphql").unwrap();
        let b = cache_key("https://api.example.com/graphql").unwrap();
        let c = cache_key("https://api.example.com/graphql?x=1").unwrap();
        assert_ne!(a, b);
        assert_eq!(b, c);
        assert_eq!(b.len(), 64);
    }
}
