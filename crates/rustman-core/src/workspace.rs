//! Loading a whole workspace: settings, collections and request files.

use std::fs;
use std::path::{Path, PathBuf};

use crate::collection::{
    COLLECTION_FILE, Collection, CollectionMeta, Entry, RequestEntry, apply_order, is_tool_dir,
};
use crate::discovery::find_root;
use crate::environment::{self, Environment};
use crate::error::{ConfigError, LoadError};
use crate::format::from_yaml;
use crate::model::{Auth, KindRegistry, NameValue, REQUEST_FILE_SUFFIX, Request, Variables};
use crate::paths::{PathEscape, contain};
use crate::settings::{HTTP_PATH_KEYS, HttpDefaults, SETTINGS_FILE, Settings};

/// A loaded workspace.
#[derive(Clone, Debug)]
pub struct Workspace {
    pub root: PathBuf,
    pub settings: Settings,
    /// The root collection; its `path` is empty.
    pub tree: Collection,
    /// Environments from `environments/*.yaml`, sorted by name.
    pub environments: Vec<Environment>,
    /// Non-fatal problems (unknown keys, unmatched `order:` entries, ...).
    pub warnings: Vec<String>,
}

impl Workspace {
    /// Find the workspace from `start` (or `--workspace`) and load it.
    pub fn discover(start: &Path, explicit: Option<&Path>) -> Result<Self, ConfigError> {
        Self::load(&find_root(start, explicit)?)
    }

    /// Load the workspace at `root` with the default request kinds.
    pub fn load(root: &Path) -> Result<Self, ConfigError> {
        Self::load_with(root, &KindRegistry::default())
    }

    pub fn load_with(root: &Path, registry: &KindRegistry) -> Result<Self, ConfigError> {
        let (settings, mut warnings) = Settings::load(root)?;
        let envs = environment::load_all(root);
        warnings.extend(envs.warnings);
        let mut loader = Loader {
            root,
            registry,
            errors: envs.errors,
            warnings,
        };
        loader.check_settings_paths(&settings);
        let tree = loader
            .load_dir(Path::new(""))
            .unwrap_or_else(|| Collection {
                path: PathBuf::new(),
                meta: CollectionMeta::default(),
                entries: Vec::new(),
            });
        if !loader.errors.is_empty() {
            return Err(ConfigError::Invalid(loader.errors));
        }
        Ok(Self {
            root: root.to_path_buf(),
            settings,
            tree,
            environments: envs.environments,
            warnings: loader.warnings,
        })
    }

    /// The active environment: `flag` (`--env`), else `env_var`
    /// (`RUSTMAN_ENV`), else `[defaults] default_environment`, else none.
    /// Naming an environment that does not exist is an error.
    pub fn active_environment(
        &self,
        flag: Option<&str>,
        env_var: Option<&str>,
    ) -> Result<Option<&Environment>, ConfigError> {
        let default = self.settings.defaults.default_environment.as_deref();
        environment::active_name(flag, env_var, default)
            .map(|name| environment::find(&self.environments, name))
            .transpose()
    }

    /// Every collection below the root, depth first, in run order.
    pub fn collections(&self) -> Vec<&Collection> {
        self.tree.descendants()
    }

    /// Every request, in run order, with the collections it inherits from.
    pub fn requests(&self) -> Vec<RequestRef<'_>> {
        let mut out = Vec::new();
        let http = &self.settings.http;
        collect_requests(&self.root, http, &self.tree, &mut Vec::new(), &mut out);
        out
    }

    /// Requests whose `name` equals `name`.
    pub fn find_by_name(&self, name: &str) -> Vec<RequestRef<'_>> {
        self.requests()
            .into_iter()
            .filter(|r| r.entry.request.name() == name)
            .collect()
    }

    /// The request `target` names: a request file (relative to `cwd`), its
    /// id (the path from the root without `.rman.yaml`), or its unique `name`.
    pub fn find_request(&self, target: &str, cwd: &Path) -> Result<RequestRef<'_>, ConfigError> {
        let file = dunce::canonicalize(cwd.join(target)).ok();
        let same_file = |r: &RequestRef<'_>| {
            file.is_some() && dunce::canonicalize(self.root.join(&r.entry.path)).ok() == file
        };
        let requests = self.requests();
        let by_path = requests
            .iter()
            .find(|r| r.entry.id() == target || same_file(r));
        if let Some(found) = by_path {
            return Ok(found.clone());
        }
        let mut named = self.find_by_name(target);
        match named.len() {
            0 => Err(ConfigError::RequestNotFound(target.to_owned())),
            1 => Ok(named.remove(0)),
            _ => Err(ConfigError::AmbiguousRequest {
                name: target.to_owned(),
                paths: named
                    .iter()
                    .map(|r| r.entry.path.to_string_lossy().replace('\\', "/"))
                    .collect(),
            }),
        }
    }
}

fn collect_requests<'a>(
    root: &'a Path,
    http: &'a HttpDefaults,
    collection: &'a Collection,
    ancestors: &mut Vec<&'a Collection>,
    out: &mut Vec<RequestRef<'a>>,
) {
    ancestors.push(collection);
    for entry in &collection.entries {
        match entry {
            Entry::Request(request) => out.push(RequestRef {
                root,
                http,
                entry: request,
                ancestors: ancestors.clone(),
            }),
            Entry::Collection(child) => collect_requests(root, http, child, ancestors, out),
        }
    }
    ancestors.pop();
}

/// A request together with the collections that contain it.
#[derive(Clone, Debug)]
pub struct RequestRef<'a> {
    /// The workspace root.
    pub root: &'a Path,
    /// `rustman.toml [http]`: the workspace client certificate.
    pub http: &'a HttpDefaults,
    pub entry: &'a RequestEntry,
    /// From the root collection down to the nearest folder.
    pub ancestors: Vec<&'a Collection>,
}

impl<'a> RequestRef<'a> {
    pub fn request(&self) -> &'a Request {
        &self.entry.request
    }

    /// Resolve a file path written in this request (relative to the request
    /// file's folder) and check it stays inside the workspace. Paths with
    /// `{{variables}}` go through this after interpolation.
    pub fn contain_file(&self, raw: &str) -> Result<PathBuf, PathEscape> {
        let folder = self.entry.path.parent().unwrap_or(Path::new(""));
        contain(self.root, &self.root.join(folder), raw)
    }

    /// Resolve a path from `rustman.toml` (relative to the workspace root)
    /// and check it stays inside the workspace.
    pub fn contain_workspace_file(&self, raw: &str) -> Result<PathBuf, PathEscape> {
        contain(self.root, self.root, raw)
    }

    /// Inherited headers merged with the request's own. A header set at a
    /// deeper level replaces every same-named (case-insensitive) header above it.
    pub fn effective_headers(&self) -> Vec<NameValue> {
        let inherited = self
            .ancestors
            .iter()
            .filter_map(|c| c.meta.headers.as_deref());
        let own = match self.request() {
            Request::Http(http) => http.headers.as_deref(),
            Request::Opaque(_) => None,
        };
        let mut merged: Vec<NameValue> = Vec::new();
        for level in inherited.chain(own) {
            merged.retain(|h| !level.iter().any(|l| l.name.eq_ignore_ascii_case(&h.name)));
            merged.extend(level.iter().cloned());
        }
        merged
    }

    /// The request's own auth, else the nearest collection's. `Auth::None`
    /// is returned as is, so an explicit `type: none` stops inheritance.
    pub fn effective_auth(&self) -> Option<&'a Auth> {
        let own = match self.request() {
            Request::Http(http) => http.auth.as_ref(),
            Request::Opaque(_) => None,
        };
        own.or_else(|| {
            self.ancestors
                .iter()
                .rev()
                .find_map(|c| c.meta.auth.as_ref())
        })
    }

    /// Collection `variables`, nearest folder first.
    pub fn collection_variables(&self) -> Vec<&'a Variables> {
        self.ancestors
            .iter()
            .rev()
            .filter_map(|c| c.meta.variables.as_ref())
            .collect()
    }
}

struct Loader<'a> {
    root: &'a Path,
    registry: &'a KindRegistry,
    errors: Vec<LoadError>,
    warnings: Vec<String>,
}

enum Item {
    Dir,
    Request,
    CollectionFile,
    Other,
}

impl Loader<'_> {
    fn check_settings_paths(&mut self, settings: &Settings) {
        let http = &settings.http;
        for key in HTTP_PATH_KEYS {
            let Some(raw) = http.path(key) else {
                continue;
            };
            if let Err(err) = contain(self.root, self.root, raw) {
                let position = http.positions.get(key).copied();
                self.errors.push(LoadError::new(
                    Path::new(SETTINGS_FILE),
                    position,
                    err.to_string(),
                ));
            }
        }
    }

    /// Load the folder at `rel`. Returns `None` for a folder (other than the
    /// root) with no request file at any depth.
    fn load_dir(&mut self, rel: &Path) -> Option<Collection> {
        let mut meta = CollectionMeta::default();
        let mut entries = Vec::new();
        for name in self.list_dir(rel) {
            let child = rel.join(&name);
            match self.classify(rel, &name) {
                Item::Dir => entries.extend(self.load_dir(&child).map(Entry::Collection)),
                Item::Request => entries.extend(self.load_request(&child).map(Entry::Request)),
                Item::CollectionFile => meta = self.load_meta(&child).unwrap_or_default(),
                Item::Other => {}
            }
        }
        if entries.is_empty() && !rel.as_os_str().is_empty() {
            return None;
        }
        let (entries, unmatched) = apply_order(entries, meta.order.as_deref().unwrap_or_default());
        for key in unmatched {
            let file = rel.join(COLLECTION_FILE);
            self.warnings.push(format!(
                "{}: order entry '{key}' matches no request or folder",
                file.display()
            ));
        }
        Some(Collection {
            path: rel.to_path_buf(),
            meta,
            entries,
        })
    }

    /// File names in `rel`, sorted.
    fn list_dir(&mut self, rel: &Path) -> Vec<String> {
        let dir = self.root.join(rel);
        let read = fs::read_dir(&dir).and_then(|it| it.collect::<Result<Vec<_>, _>>());
        let mut names: Vec<String> = match read {
            Ok(entries) => entries
                .iter()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect(),
            Err(err) => {
                self.errors.push(LoadError::new(rel, None, err.to_string()));
                Vec::new()
            }
        };
        names.sort();
        names
    }

    /// What `name` inside `rel` is. Symlinked folders are not followed.
    fn classify(&self, rel: &Path, name: &str) -> Item {
        let Ok(meta) = fs::symlink_metadata(self.root.join(rel).join(name)) else {
            return Item::Other;
        };
        if meta.is_dir() {
            return if is_tool_dir(rel, name) {
                Item::Other
            } else {
                Item::Dir
            };
        }
        if name == COLLECTION_FILE {
            Item::CollectionFile
        } else if name.ends_with(REQUEST_FILE_SUFFIX) {
            Item::Request
        } else {
            Item::Other
        }
    }

    fn read(&mut self, rel: &Path) -> Option<String> {
        let checked = rel
            .to_str()
            .ok_or_else(|| "file name is not valid UTF-8".to_owned())
            .and_then(|raw| check_path(self.root, self.root, raw));
        let result = checked.and_then(|path| fs::read_to_string(path).map_err(|e| e.to_string()));
        result
            .map_err(|message| self.errors.push(LoadError::new(rel, None, message)))
            .ok()
    }

    fn load_meta(&mut self, rel: &Path) -> Option<CollectionMeta> {
        let text = self.read(rel)?;
        from_yaml(rel, &text).map_err(|e| self.errors.push(e)).ok()
    }

    fn load_request(&mut self, rel: &Path) -> Option<RequestEntry> {
        let text = self.read(rel)?;
        let request = self
            .registry
            .load(rel, &text)
            .map_err(|e| self.errors.push(e))
            .ok()?;
        self.check_request_paths(rel, &request);
        Some(RequestEntry {
            path: rel.to_path_buf(),
            request,
        })
    }

    fn check_request_paths(&mut self, rel: &Path, request: &Request) {
        let base = self.root.join(rel.parent().unwrap_or(Path::new("")));
        self.errors
            .extend(request_path_errors(self.root, &base, rel, request));
    }
}

/// The errors of `request`'s file paths that leave the workspace at `root`,
/// for a request file in `base` (reported as `shown`).
fn request_path_errors(
    root: &Path,
    base: &Path,
    shown: &Path,
    request: &Request,
) -> Vec<LoadError> {
    let Request::Http(http) = request else {
        return Vec::new();
    };
    http.file_paths()
        .into_iter()
        .filter_map(|path| {
            let message = check_path(root, base, path.get()).err()?;
            Some(LoadError::new(shown, path.position(), message))
        })
        .collect()
}

/// Check a request about to be written to `file` (a new file, given as the
/// user typed it): inside a workspace, its file paths must stay in that
/// workspace, or the workspace would no longer load. Outside any workspace
/// (or when the folder does not exist yet) there is nothing to check.
pub fn check_new_request_file(file: &Path, request: &Request) -> Result<(), ConfigError> {
    let folder = file.parent().filter(|p| !p.as_os_str().is_empty());
    let Ok(base) = dunce::canonicalize(folder.unwrap_or(Path::new("."))) else {
        return Ok(());
    };
    let Ok(root) = crate::discovery::find_root(&base, None) else {
        return Ok(());
    };
    let errors = request_path_errors(&root, &base, file, request);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ConfigError::Invalid(errors))
    }
}

/// Check that `raw` stays inside the workspace. Paths with `{{variables}}`
/// are checked again after interpolation, by `resolve_request`
/// (see [`RequestRef::contain_file`]).
fn check_path(root: &Path, base: &Path, raw: &str) -> Result<PathBuf, String> {
    if raw.contains("{{") {
        return Ok(base.join(raw));
    }
    contain(root, base, raw).map_err(|e| e.to_string())
}
