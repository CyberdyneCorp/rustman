//! OpenRPC method catalogs (design D5): method names, param names and
//! schemas, and result schemas, read from an OpenRPC document.
//!
//! A catalog comes from `rustman.toml [jsonrpc.catalogs]` (a file per
//! endpoint) or from the cache `rustman rpc discover` fills, at
//! `.rustman/cache/openrpc/<sha256 of the endpoint>.json`. Calls are
//! checked against it before they are sent: the method must exist (with a
//! "did you mean" suggestion), required params must be given, and each
//! param must match its schema (validated with `jsonschema`).

use std::path::{Path, PathBuf};

use rustman_core::Redactor;
use rustman_core::run::now_rfc3339;
use rustman_core::settings::RpcSettings;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The cache folder, relative to the workspace root.
pub const CATALOG_CACHE_DIR: &str = ".rustman/cache/openrpc";

/// The version of the cache file layout.
const CACHE_FORMAT: u32 = 1;

/// A catalog that cannot be read, parsed or written.
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("{path}: {message}")]
    File { path: String, message: String },
    #[error("invalid OpenRPC document: {0}")]
    Invalid(String),
    #[error("invalid endpoint '{0}'")]
    Endpoint(String),
}

/// One param of a method.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamSpec {
    pub name: String,
    pub required: bool,
    /// Its JSON Schema (`{}` when the document has none).
    pub schema: Value,
}

/// One method of a catalog.
#[derive(Clone, Debug, PartialEq)]
pub struct MethodSpec {
    pub name: String,
    pub summary: Option<String>,
    pub params: Vec<ParamSpec>,
    /// The JSON Schema of its result, when the document gives one.
    pub result: Option<Value>,
}

/// The methods of an OpenRPC document.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Catalog {
    pub methods: Vec<MethodSpec>,
    /// `components`, which `$ref`s in schemas point into.
    components: Value,
}

impl Catalog {
    /// Read the OpenRPC document `doc`.
    pub fn parse(doc: &Value) -> Result<Self, CatalogError> {
        let invalid = |m: &str| CatalogError::Invalid(m.to_owned());
        let methods = doc
            .get("methods")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid("no 'methods' list"))?;
        let components = doc.get("components").cloned().unwrap_or(Value::Null);
        let mut catalog = Self {
            methods: Vec::with_capacity(methods.len()),
            components,
        };
        for method in methods {
            let method = catalog.resolve(method);
            let name = method
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("a method has no name"))?;
            let spec = MethodSpec {
                name: name.to_owned(),
                summary: method
                    .get("summary")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                params: catalog.params(method),
                result: method
                    .get("result")
                    .map(|r| catalog.resolve(r))
                    .and_then(|r| r.get("schema").cloned()),
            };
            catalog.methods.push(spec);
        }
        Ok(catalog)
    }

    fn params(&self, method: &Value) -> Vec<ParamSpec> {
        let Some(params) = method.get("params").and_then(Value::as_array) else {
            return Vec::new();
        };
        params
            .iter()
            .map(|p| self.resolve(p))
            .map(|p| ParamSpec {
                name: p
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                required: p.get("required").and_then(Value::as_bool).unwrap_or(false),
                schema: p
                    .get("schema")
                    .cloned()
                    .unwrap_or(Value::Object(Map::new())),
            })
            .collect()
    }

    /// `value`, or what its local `$ref` points to.
    fn resolve<'v>(&'v self, value: &'v Value) -> &'v Value {
        let pointer = value
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|r| r.strip_prefix("#/components"));
        match pointer {
            Some(pointer) => self.components.pointer(pointer).unwrap_or(value),
            None => value,
        }
    }

    /// The method named `name`.
    pub fn method(&self, name: &str) -> Option<&MethodSpec> {
        self.methods.iter().find(|m| m.name == name)
    }

    /// The methods whose name starts with `prefix`, in catalog order.
    pub fn completions(&self, prefix: &str) -> Vec<&MethodSpec> {
        self.methods
            .iter()
            .filter(|m| m.name.starts_with(prefix))
            .collect()
    }

    /// The closest method name to `name`, when one is close enough to be
    /// a likely typo.
    pub fn suggest(&self, name: &str) -> Option<&str> {
        let limit = (name.chars().count() / 3).max(2);
        self.methods
            .iter()
            .map(|m| (strsim::levenshtein(name, &m.name), m.name.as_str()))
            .filter(|(distance, _)| *distance <= limit)
            .min_by_key(|(distance, _)| *distance)
            .map(|(_, found)| found)
    }

    /// Every violation of calling `method` with `params`, each with its
    /// param path. Empty when the call matches the catalog.
    pub fn check(&self, method: &str, params: Option<&Value>) -> Vec<String> {
        let Some(spec) = self.method(method) else {
            let hint = self
                .suggest(method)
                .map(|s| format!(" (did you mean {s}?)"))
                .unwrap_or_default();
            return vec![format!("unknown method '{method}'{hint}")];
        };
        spec.params
            .iter()
            .enumerate()
            .filter_map(|(index, param)| self.check_param(index, param, params))
            .collect()
    }

    fn check_param(
        &self,
        index: usize,
        param: &ParamSpec,
        params: Option<&Value>,
    ) -> Option<String> {
        let (path, value) = match params {
            Some(Value::Object(named)) => {
                (format!("params.{}", param.name), named.get(&param.name))
            }
            Some(Value::Array(list)) => (format!("params[{index}]"), list.get(index)),
            _ => (format!("params[{index}]"), None),
        };
        let Some(value) = value else {
            return param
                .required
                .then(|| format!("{path}: missing required param '{}'", param.name));
        };
        let schema = self.resolve(&param.schema);
        if let Some(message) = type_mismatch(schema, value) {
            return Some(format!("{path}: {message}"));
        }
        self.schema_violation(schema, value)
            .map(|message| format!("{path}: {message}"))
    }

    /// The first violation of `schema` by `value`; a schema the validator
    /// cannot compile is not enforced.
    fn schema_violation(&self, schema: &Value, value: &Value) -> Option<String> {
        let mut root = schema.clone();
        if let (Value::Object(map), Value::Object(_)) = (&mut root, &self.components) {
            map.entry("components").or_insert(self.components.clone());
        }
        let validator = jsonschema::validator_for(&root).ok()?;
        validator.iter_errors(value).next().map(|e| e.to_string())
    }
}

/// `expected <type>, got <type>` when `schema` names a type `value` is not.
fn type_mismatch(schema: &Value, value: &Value) -> Option<String> {
    let allowed: Vec<&str> = match schema.get("type")? {
        Value::String(one) => vec![one.as_str()],
        Value::Array(many) => many.iter().filter_map(Value::as_str).collect(),
        _ => return None,
    };
    let actual = json_type(value);
    let fits = allowed
        .iter()
        .any(|t| *t == actual || (*t == "number" && actual == "integer"));
    (!fits).then(|| format!("expected {}, got {actual}", allowed.join(" or ")))
}

/// The JSON Schema type name of `value`.
fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// `endpoint` reduced to scheme, host, port and path.
pub fn normalize_endpoint(endpoint: &str) -> Result<String, CatalogError> {
    let invalid = || CatalogError::Endpoint(endpoint.to_owned());
    let url = url::Url::parse(endpoint).map_err(|_| invalid())?;
    let host = url.host_str().ok_or_else(invalid)?;
    let port = url
        .port_or_known_default()
        .map_or_else(String::new, |p| format!(":{p}"));
    Ok(format!("{}://{host}{port}{}", url.scheme(), url.path()))
}

/// The cache file of `endpoint` under the workspace `root`: named by the
/// SHA-256 of the normalized endpoint and its query (endpoints that differ
/// only by `?network=` serve different catalogs), so a key in the URL
/// stays private.
pub fn cache_path(root: &Path, endpoint: &str) -> Result<PathBuf, CatalogError> {
    let mut key = normalize_endpoint(endpoint)?;
    if let Some(query) = url::Url::parse(endpoint)
        .ok()
        .and_then(|u| u.query().map(str::to_owned))
    {
        key = format!("{key}?{query}");
    }
    let digest = ring::digest::digest(&ring::digest::SHA256, key.as_bytes());
    let name: String = digest.as_ref().iter().map(|b| format!("{b:02x}")).collect();
    Ok(root.join(CATALOG_CACHE_DIR).join(format!("{name}.json")))
}

/// A cache file as written on disk.
#[derive(Serialize, Deserialize)]
struct CacheFile {
    format: u32,
    /// The normalized endpoint (no query), with any secret redacted.
    endpoint: String,
    /// RFC 3339 UTC.
    fetched_at: String,
    /// The OpenRPC document as `rpc.discover` returned it, redacted.
    openrpc: Value,
}

/// Cache `doc`, the OpenRPC document `endpoint` returned, with every value
/// `redactor` knows redacted. Returns where it was written and its catalog.
pub fn write_cache(
    root: &Path,
    endpoint: &str,
    doc: Value,
    redactor: &Redactor,
) -> Result<(PathBuf, Catalog), CatalogError> {
    let catalog = Catalog::parse(&doc)?;
    let path = cache_path(root, endpoint)?;
    let file = CacheFile {
        format: CACHE_FORMAT,
        endpoint: redactor.text(&normalize_endpoint(endpoint)?).into_owned(),
        fetched_at: now_rfc3339(),
        openrpc: redacted(&doc, redactor),
    };
    let text = serde_json::to_string_pretty(&file).unwrap_or_default();
    let failed = |e: std::io::Error| CatalogError::File {
        path: shown(root, &path),
        message: e.to_string(),
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(failed)?;
    }
    std::fs::write(&path, text).map_err(failed)?;
    Ok((path, catalog))
}

/// `doc` with every value `redactor` knows replaced by its placeholder.
fn redacted(doc: &Value, redactor: &Redactor) -> Value {
    let text = doc.to_string();
    match redactor.text(&text) {
        std::borrow::Cow::Borrowed(_) => doc.clone(),
        std::borrow::Cow::Owned(shown) => {
            serde_json::from_str(&shown).unwrap_or(Value::String(shown))
        }
    }
}

/// The catalog for a request to `resolved` (written as `written` in the
/// request file): the `[jsonrpc.catalogs]` file matching either form,
/// else the cached document. `None` when there is neither.
pub fn catalog_for(
    root: &Path,
    settings: &RpcSettings,
    written: &str,
    resolved: &str,
) -> Result<Option<Catalog>, CatalogError> {
    if let Some(file) = linked_file(settings, written, resolved) {
        return read_document(root, &root.join(file)).map(Some);
    }
    let Ok(path) = cache_path(root, resolved) else {
        return Ok(None);
    };
    if !path.is_file() {
        return Ok(None);
    }
    let cached: CacheFile = read_json(root, &path)?;
    Catalog::parse(&cached.openrpc).map(Some)
}

/// The `[jsonrpc.catalogs]` entry for an endpoint.
fn linked_file<'s>(settings: &'s RpcSettings, written: &str, resolved: &str) -> Option<&'s str> {
    let normalized = normalize_endpoint(resolved).ok();
    settings
        .catalogs
        .iter()
        .find(|(key, _)| {
            key.as_str() == written
                || key.as_str() == resolved
                || normalized.is_some() && normalize_endpoint(key).ok() == normalized
        })
        .map(|(_, file)| file.as_str())
}

/// An OpenRPC document file: JSON, or YAML for `.yaml` and `.yml`.
fn read_document(root: &Path, path: &Path) -> Result<Catalog, CatalogError> {
    let yaml = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("yaml") || e.eq_ignore_ascii_case("yml"));
    let doc: Value = if yaml {
        let text = read_text(root, path)?;
        rustman_core::format::from_yaml(path, &text).map_err(|e| CatalogError::File {
            path: shown(root, path),
            message: e.message.clone(),
        })?
    } else {
        read_json(root, path)?
    };
    Catalog::parse(&doc)
}

fn read_text(root: &Path, path: &Path) -> Result<String, CatalogError> {
    std::fs::read_to_string(path).map_err(|e| CatalogError::File {
        path: shown(root, path),
        message: e.to_string(),
    })
}

fn read_json<T: serde::de::DeserializeOwned>(root: &Path, path: &Path) -> Result<T, CatalogError> {
    let text = read_text(root, path)?;
    serde_json::from_str(&text).map_err(|e| CatalogError::File {
        path: shown(root, path),
        message: e.to_string(),
    })
}

/// `path` relative to `root`, with `/`.
fn shown(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn eth() -> Catalog {
        Catalog::parse(&json!({
            "openrpc": "1.2.6",
            "info": {"title": "node", "version": "1"},
            "methods": [
                {"name": "eth_blockNumber", "params": [], "result": {"name": "n", "schema": {"type": "string"}}},
                {"name": "eth_getBalance", "params": [
                    {"name": "address", "required": true, "schema": {"$ref": "#/components/schemas/Address"}},
                    {"name": "block", "schema": {"type": "string"}}
                ]},
                {"$ref": "#/components/methods/eth_chainId"}
            ],
            "components": {
                "schemas": {"Address": {"type": "string", "pattern": "^0x[0-9a-f]{40}$"}},
                "methods": {"eth_chainId": {"name": "eth_chainId", "params": []}}
            }
        }))
        .unwrap()
    }

    #[test]
    fn unknown_methods_get_a_suggestion() {
        assert_eq!(
            eth().check("eth_blockNumbr", None),
            ["unknown method 'eth_blockNumbr' (did you mean eth_blockNumber?)"]
        );
        assert_eq!(
            eth().check("net_peerCount", None),
            ["unknown method 'net_peerCount'"]
        );
        assert!(eth().check("eth_chainId", None).is_empty(), "refs resolve");
    }

    #[test]
    fn param_types_and_required_params_are_checked() {
        let catalog = eth();
        assert_eq!(
            catalog.check("eth_getBalance", Some(&json!([42]))),
            ["params[0]: expected string, got integer"]
        );
        assert_eq!(
            catalog.check("eth_getBalance", Some(&json!({"block": "latest"}))),
            ["params.address: missing required param 'address'"]
        );
        let bad = catalog.check("eth_getBalance", Some(&json!(["0xZZ"])));
        assert_eq!(bad.len(), 1);
        assert!(bad[0].starts_with("params[0]: "), "{bad:?}");
        let ok = json!(["0x00000000000000000000000000000000000000aa", "latest"]);
        assert!(catalog.check("eth_getBalance", Some(&ok)).is_empty());
    }

    #[test]
    fn completions_list_methods_by_prefix() {
        let catalog = eth();
        let names: Vec<&str> = catalog
            .completions("eth_get")
            .iter()
            .map(|m| m.name.as_str())
            .collect();
        assert_eq!(names, ["eth_getBalance"]);
        assert_eq!(
            catalog.method("eth_getBalance").unwrap().params[0].name,
            "address"
        );
    }

    #[test]
    fn documents_without_methods_are_invalid() {
        let err = Catalog::parse(&json!({"openrpc": "1.2.6"})).unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid OpenRPC document: no 'methods' list"
        );
    }

    #[test]
    fn the_cache_is_keyed_by_the_normalized_endpoint() {
        let root = Path::new("ws");
        let a = cache_path(root, "https://node.example/v3/KEY").unwrap();
        let b = cache_path(root, "https://node.example:443/v3/KEY").unwrap();
        assert_eq!(a, b);
        assert!(!a.to_string_lossy().contains("KEY"));
        assert!(a.starts_with(root.join(CATALOG_CACHE_DIR)));
        // Regression: endpoints differing only by query shared one file.
        let main = cache_path(root, "https://node.example/?network=main").unwrap();
        let test = cache_path(root, "https://node.example/?network=test").unwrap();
        assert_ne!(main, test);
        assert!(!main.to_string_lossy().contains("network"));
    }

    #[test]
    fn catalogs_come_from_settings_or_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let settings = RpcSettings::default();
        let url = "http://127.0.0.1:8545/";
        assert!(catalog_for(root, &settings, url, url).unwrap().is_none());
        let doc = json!({"methods": [{"name": "getblockcount", "params": []}]});
        let (path, catalog) = write_cache(root, url, doc, &Redactor::new()).unwrap();
        assert!(path.is_file());
        assert_eq!(catalog.methods.len(), 1);
        let cached = catalog_for(root, &settings, "{{rpc_url}}", url)
            .unwrap()
            .unwrap();
        assert_eq!(cached.methods[0].name, "getblockcount");
        std::fs::write(root.join("node.yaml"), "methods:\n  - name: getblock\n").unwrap();
        let mut linked = RpcSettings::default();
        linked
            .catalogs
            .insert("{{rpc_url}}".into(), "node.yaml".into());
        let file = catalog_for(root, &linked, "{{rpc_url}}", url)
            .unwrap()
            .unwrap();
        assert_eq!(file.methods[0].name, "getblock");
    }

    #[test]
    fn the_cached_document_is_redacted_by_write_cache_itself() {
        let dir = tempfile::tempdir().unwrap();
        let secret = rustman_core::Secret::new("key", "doc-secret-91".to_owned());
        let redactor = Redactor::new().with(&secret);
        let doc = json!({"info": {"title": "node doc-secret-91"}, "methods": []});
        let url = "http://127.0.0.1:8545/";
        let (path, _) = write_cache(dir.path(), url, doc, &redactor).unwrap();
        let text = std::fs::read_to_string(path).unwrap();
        assert!(!text.contains("doc-secret-91"), "{text}");
        assert!(text.contains("node «redacted:key»"), "{text}");
    }
}
