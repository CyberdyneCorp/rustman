//! Saving captures as request files (spec: Save captures as request files,
//! Path and id parameterization, Base URL extraction, Secrets become
//! placeholders; design D5). Captures become the importers' neutral IR,
//! then go through the same planning and writing as `rustman import`:
//! secret extraction, deterministic file names and the workspace writer,
//! so re-saving the same captures changes nothing.
//!
//! 1. templatize each path ([`template`]) and group by method, origin and
//!    templated path, keeping the most recent capture as the example;
//! 2. put each origin in the environment (`base_url`, `base_url_2`...),
//!    with the first value of every path variable as its default (a
//!    secret one for segments that look like secrets, see [`template`]);
//! 3. parameterize credential headers ([`secrets`]), then name the other
//!    secrets with the importer's extraction, reusing the names the
//!    environment already declares (`resave`);
//! 4. plan and apply with [`rustman_import`], keeping the `order:` entries
//!    of earlier saves; raw values are stored only through the store the
//!    caller passes (`--store-secrets`).

mod resave;
pub mod secrets;
pub mod template;

use std::collections::BTreeMap;
use std::path::Path;

use rustman_core::model::{KindRegistry, VarValue};
use rustman_core::{Environment, Workspace};
use rustman_import::apply::StoreSecret;
use rustman_import::detect::Format;
use rustman_import::ir::{
    Folder, ImportedBody, ImportedCollection, ImportedEnvironment, ImportedRequest, Item, Location,
    Pair, Variable,
};
use rustman_import::report::Action;
use rustman_import::secrets::{Existing, environment_name};
use rustman_import::{ImportError, OnConflict, PlanOptions, Report, apply, lock_workspace, plan};

use crate::capture::{Capture, Exchange};
use crate::filter::header;
use secrets::{HeaderSecrets, entropy_warnings};
use template::{Templated, templatize};

/// The environment that receives `base_url` when `--env` is not given.
pub const DEFAULT_ENVIRONMENT: &str = "recorded";

/// The collection folder captures go to when `--into` is not given; each
/// origin gets a folder of its own inside it.
pub const DEFAULT_DIR: &str = "recorded";

/// The name messages and reports give the source.
const SOURCE: &str = "recording";

/// Where to save.
#[derive(Clone, Debug)]
pub struct SaveOptions<'a> {
    /// The workspace root.
    pub root: &'a Path,
    /// The folder for the request files, relative to the root with `/`
    /// separators; `None` for `recorded/<host>/`.
    pub into: Option<String>,
    /// The environment for `base_url`, path defaults and secrets.
    pub environment: String,
}

/// What a save did.
#[derive(Debug)]
pub struct Saved {
    /// `None` when there was no request to save, and nothing was written.
    pub report: Option<Report>,
    /// Request files written.
    pub written: usize,
    /// Request files already up to date.
    pub unchanged: usize,
    /// The secret names that replaced raw values.
    pub secrets: Vec<String>,
    /// Whether their values went to the secret store.
    pub stored: bool,
    pub warnings: Vec<String>,
}

/// Why a save failed.
#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error(transparent)]
    Import(#[from] ImportError),
    #[error("record: cannot read the workspace: {0}")]
    Workspace(String),
    #[error("record: invalid environment name {0:?} (use lowercase letters, digits and -)")]
    Environment(String),
}

/// Save the exchanges among `captures` into the workspace. Secret values
/// are handed to `store` when given, and kept nowhere otherwise.
pub fn save(
    captures: &[impl AsRef<Capture>],
    options: &SaveOptions<'_>,
    store: Option<StoreSecret<'_>>,
) -> Result<Saved, SaveError> {
    if environment_name(&options.environment) != options.environment {
        return Err(SaveError::Environment(options.environment.clone()));
    }
    let exchanges: Vec<&Exchange> = captures
        .iter()
        .filter_map(|c| c.as_ref().exchange())
        .collect();
    if exchanges.is_empty() {
        return Ok(Saved::nothing());
    }
    let _lock = lock_workspace(options.root)?;
    let registry = KindRegistry::default().with_opaque_fallback();
    let ws = Workspace::load_with(options.root, &registry)
        .map_err(|e| SaveError::Workspace(e.to_string()))?;
    let known = known_variables(&ws.environments, &options.environment);
    let mut recording = collection_with(
        &exchanges,
        &options.environment,
        options.into.is_none(),
        &known,
    );
    resave::name_secrets(
        &mut recording.collection,
        &options.environment,
        &ws.environments,
    );
    let no_lookup = |_: &str, _: &str| None;
    let plan_options = PlanOptions {
        root: options.root,
        source: SOURCE.to_owned(),
        format: Format::Har,
        destination: Some(
            options
                .into
                .clone()
                .unwrap_or_else(|| DEFAULT_DIR.to_owned()),
        ),
        on_conflict: OnConflict::Overwrite,
        environment: Some(options.environment.clone()),
        existing: Existing {
            environments: &ws.environments,
            lookup: &no_lookup,
        },
        dry_run: false,
    };
    let mut plan = plan(recording.collection, &plan_options)?;
    resave::keep_earlier_order(&mut plan, options.root);
    let discard = |_: &[(&str, &_)]| Ok(());
    apply(&plan, options.root, store.unwrap_or(&discard))?;
    let mut secrets: Vec<String> = plan
        .secrets
        .iter()
        .map(|m| m.secret.name().to_owned())
        .collect();
    secrets.dedup();
    let requests = plan
        .files
        .iter()
        .filter(|f| f.kind == rustman_import::report::FileKind::Request);
    let (unchanged, written): (Vec<_>, Vec<_>) =
        requests.partition(|f| f.action == Action::Unchanged);
    recording.warnings.extend(
        plan.files
            .iter()
            .filter(|f| f.action == Action::Overwrite)
            .map(|f| format!("warning: {} overwritten with the recorded version", f.path)),
    );
    Ok(Saved {
        written: written.len(),
        unchanged: unchanged.len(),
        secrets,
        stored: store.is_some(),
        warnings: recording.warnings,
        report: Some(plan.report),
    })
}

impl Saved {
    /// A save of no request: nothing written, no environment touched.
    fn nothing() -> Self {
        Self {
            report: None,
            written: 0,
            unchanged: 0,
            secrets: Vec::new(),
            stored: false,
            warnings: Vec::new(),
        }
    }

    /// Whether there was no request to save (only tunnels, rejected
    /// handshakes, or no capture at all).
    pub fn is_nothing(&self) -> bool {
        self.report.is_none()
    }
}

/// The string variables of the workspace environment `name`.
fn known_variables(environments: &[Environment], name: &str) -> Vec<(String, String)> {
    let Some(env) = environments.iter().find(|e| e.name == name) else {
        return Vec::new();
    };
    env.variables
        .iter()
        .filter_map(|(n, v)| match v {
            VarValue::String(s) => Some((n.to_owned(), s.clone())),
            _ => None,
        })
        .collect()
}

/// The captures as an importer collection, and the warnings building it.
pub struct Recording {
    pub collection: ImportedCollection,
    pub warnings: Vec<String>,
}

/// One request file: the latest exchange of a method, origin and
/// templated path.
struct Group<'a> {
    templated: Templated,
    latest: &'a Exchange,
}

/// The parts of an absolute URL.
pub(crate) struct Url<'a> {
    pub(crate) origin: &'a str,
    pub(crate) authority: &'a str,
    pub(crate) path: &'a str,
    pub(crate) query: &'a str,
}

pub(crate) fn split(url: &str) -> Url<'_> {
    let scheme_end = url.find("://").map_or(0, |i| i + 3);
    let after = &url[scheme_end..];
    let authority_len = after.find(['/', '?', '#']).unwrap_or(after.len());
    let origin = &url[..scheme_end + authority_len];
    let rest = &after[authority_len..];
    let rest = rest.split('#').next().unwrap_or_default();
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    Url {
        origin,
        authority: &after[..authority_len],
        path,
        query,
    }
}

/// Build the collection for `exchanges`, oldest first. With `per_host`,
/// each origin gets a folder named after its host.
pub fn collection(exchanges: &[&Exchange], environment: &str, per_host: bool) -> Recording {
    collection_with(exchanges, environment, per_host, &[])
}

/// [`collection`], where an origin that `known` (the environment's
/// variables) already holds as `base_url` or `base_url_N` keeps that
/// name, and a new origin takes the first name `known` does not use.
pub fn collection_with(
    exchanges: &[&Exchange],
    environment: &str,
    per_host: bool,
    known: &[(String, String)],
) -> Recording {
    let mut origins: Vec<(String, String)> = Vec::new();
    let mut defaults: Vec<Variable> = Vec::new();
    let mut groups: BTreeMap<(usize, String, String), Group<'_>> = BTreeMap::new();
    for exchange in exchanges {
        let url = split(&exchange.url);
        let index = match origins.iter().position(|(o, _)| o == url.origin) {
            Some(index) => index,
            None => {
                origins.push((url.origin.to_owned(), url.authority.to_owned()));
                origins.len() - 1
            }
        };
        let templated = templatize(url.path);
        let plain_vars = templated.variables.iter().map(|v| (v, false));
        let secret_vars = templated.secrets.iter().map(|v| (v, true));
        for ((name, value), secret) in plain_vars.chain(secret_vars) {
            if !defaults.iter().any(|d| &d.name == name) {
                defaults.push(Variable {
                    name: name.clone(),
                    value: value.clone(),
                    secret,
                    path: false,
                });
            }
        }
        let key = (index, templated.path.clone(), exchange.method.clone());
        groups.insert(
            key,
            Group {
                templated,
                latest: exchange,
            },
        );
    }
    let names = base_url_names(&origins, known);
    let mut builder = Builder::new(per_host, &origins, names);
    for ((index, _, _), group) in &groups {
        builder.add(*index, group);
    }
    builder.finish(environment, &origins, defaults)
}

struct Builder {
    per_host: bool,
    /// The base URL variable of each origin.
    names: Vec<String>,
    root: Vec<Item>,
    folders: Vec<Folder>,
    secrets: HeaderSecrets,
    warnings: Vec<String>,
}

impl Builder {
    fn new(per_host: bool, origins: &[(String, String)], names: Vec<String>) -> Self {
        let folders = origins
            .iter()
            .map(|(_, authority)| Folder {
                name: authority.clone(),
                location: Location::item(SOURCE, authority),
                ..Folder::default()
            })
            .collect();
        Self {
            per_host,
            names,
            root: Vec::new(),
            folders,
            secrets: HeaderSecrets::default(),
            warnings: Vec::new(),
        }
    }

    fn add(&mut self, origin: usize, group: &Group<'_>) {
        let exchange = group.latest;
        let name = format!("{} {}", exchange.method, group.templated.shape);
        let url = split(&exchange.url);
        let mut request = ImportedRequest {
            name: name.clone(),
            method: exchange.method.clone(),
            url: format!("{{{{{}}}}}{}", self.names[origin], group.templated.path),
            query: query_pairs(url.query),
            headers: request_headers(exchange),
            body: self.body(exchange, &name),
            location: Location::item(SOURCE, &name),
            ..ImportedRequest::default()
        };
        self.secrets.parameterize(&mut request);
        self.warnings.extend(entropy_warnings(&request, &name));
        if exchange.status.is_none() {
            // The request is still a useful starting point; say it was
            // never answered so it is not mistaken for a working one.
            self.warnings.push(format!(
                "warning: {name} got no response when recorded; its request is saved anyway"
            ));
        }
        let item = Item::Request(Box::new(request));
        if self.per_host {
            self.folders[origin].items.push(item);
        } else {
            self.root.push(item);
        }
    }

    /// The request body as an example, unless it was truncated or is not
    /// text.
    fn body(&mut self, exchange: &Exchange, shown: &str) -> Option<ImportedBody> {
        let captured = &exchange.request_body;
        if captured.bytes.is_empty() {
            return None;
        }
        if captured.truncated {
            self.warnings.push(format!(
                "warning: the body of {shown} was truncated when recorded and is not saved"
            ));
            return None;
        }
        let Ok(text) = std::str::from_utf8(&captured.bytes) else {
            self.warnings.push(format!(
                "warning: the body of {shown} is not text and is not saved"
            ));
            return None;
        };
        let mime = header(&exchange.request_headers, "content-type")
            .unwrap_or_default()
            .to_ascii_lowercase();
        Some(if mime.contains("json") {
            ImportedBody::Json {
                text: text.to_owned(),
            }
        } else if mime.starts_with("application/x-www-form-urlencoded") {
            ImportedBody::Form {
                fields: query_pairs(text),
            }
        } else {
            ImportedBody::Text {
                text: text.to_owned(),
            }
        })
    }

    fn finish(
        mut self,
        environment: &str,
        origins: &[(String, String)],
        defaults: Vec<Variable>,
    ) -> Recording {
        let mut variables: Vec<Variable> = Vec::new();
        for ((origin, _), name) in origins.iter().zip(&self.names) {
            let name = name.clone();
            if name != base_url_name(0) {
                self.warnings.push(format!(
                    "warning: captures from {origin} use {{{{{name}}}}} (more than one origin saved)"
                ));
            }
            variables.push(plain(name, origin.clone()));
        }
        let unguessable: Vec<&str> = defaults
            .iter()
            .filter(|d| !d.secret && template::is_unguessable_id(&d.value))
            .map(|d| d.name.as_str())
            .collect();
        if !unguessable.is_empty() {
            self.warnings.push(format!(
                "warning: path variables {} keep their recorded UUID or ULID values as defaults \
                 in environments/{environment}.yaml; review them before committing",
                unguessable.join(", ")
            ));
        }
        variables.extend(defaults);
        variables.extend(
            self.secrets
                .moved
                .into_iter()
                .map(|(name, value)| Variable {
                    name,
                    value,
                    secret: true,
                    path: false,
                }),
        );
        let items = if self.per_host {
            let folders = self.folders.into_iter().filter(|f| !f.items.is_empty());
            folders.map(|f| Item::Folder(Box::new(f))).collect()
        } else {
            self.root
        };
        let collection = ImportedCollection {
            name: SOURCE.to_owned(),
            root: Folder {
                name: SOURCE.to_owned(),
                items,
                location: Location::item(SOURCE, ""),
                ..Folder::default()
            },
            environments: vec![ImportedEnvironment {
                name: environment.to_owned(),
                variables,
                location: Location::item(SOURCE, environment),
            }],
            notes: Vec::new(),
        };
        Recording {
            collection,
            warnings: self.warnings,
        }
    }
}

fn plain(name: String, value: String) -> Variable {
    Variable {
        name,
        value,
        secret: false,
        path: false,
    }
}

/// The base URL variable of each origin: the name `known` gives its value,
/// else the first of `base_url`, `base_url_2`... that is free.
fn base_url_names(origins: &[(String, String)], known: &[(String, String)]) -> Vec<String> {
    let is_base_url = |name: &str| {
        name == "base_url"
            || name
                .strip_prefix("base_url_")
                .is_some_and(|n| n.parse::<usize>().is_ok())
    };
    let mut names: Vec<String> = Vec::new();
    for (origin, _) in origins {
        let reused = known
            .iter()
            .find(|(name, value)| value == origin && is_base_url(name) && !names.contains(name));
        let name = match reused {
            Some((name, _)) => name.clone(),
            None => (0..)
                .map(base_url_name)
                .find(|n| !names.contains(n) && !known.iter().any(|(k, _)| k == n))
                .expect("some suffix is free"),
        };
        names.push(name);
    }
    names
}

/// `base_url` for the first origin, `base_url_2`... for the others.
fn base_url_name(index: usize) -> String {
    match index {
        0 => "base_url".to_owned(),
        n => format!("base_url_{}", n + 1),
    }
}

/// The request headers worth saving: no transport or hop-by-hop headers.
fn request_headers(exchange: &Exchange) -> Vec<Pair> {
    exchange
        .request_headers
        .iter()
        .filter(|(name, _)| !rustman_import::har::is_transport_header(name))
        .filter(|(name, _)| !name.to_ascii_lowercase().starts_with("proxy-"))
        .map(|(name, value)| Pair::new(name, value))
        .collect()
}

/// `a=1&b` as pairs, still percent-encoded.
fn query_pairs(query: &str) -> Vec<Pair> {
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (name, value) = p.split_once('=').unwrap_or((p, ""));
            Pair::new(name, value)
        })
        .collect()
}

#[cfg(test)]
mod tests;
