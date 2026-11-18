//! Generating a workspace collection from an [`ApiModel`]: one sub-collection
//! per first tag (`untagged` when none) and one `kind: http` request file per
//! operation, named from its `operationId` in kebab-case (otherwise
//! `<method>-<path-slug>`), with `-2`, `-3`, ... on collisions. Servers
//! become environments and security schemes become auth config.

pub mod auth;
pub mod environments;
pub mod names;
mod request;
mod write;

use rustman_core::Environment;
use rustman_core::collection::{COLLECTION_FILE, CollectionMeta, CollectionSource};
use rustman_core::format::write::{collection_to_yaml, request_to_yaml};
use rustman_core::model::{HttpRequest, REQUEST_FILE_SUFFIX, Request};

use crate::openapi::model::{ApiModel, Operation};
use crate::openapi::{Diagnostic, Location};

pub use auth::Security;
pub use request::{BASE_URL, OPERATION_TAG, PATH_TAG, RequestContext, request_for};
pub use write::{WriteError, write};

/// The folder of operations without tags.
pub const UNTAGGED: &str = "untagged";

/// One generated file, at a `/`-separated path relative to the collection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeneratedFile {
    pub path: String,
    pub contents: String,
}

/// The request generated for one operation, at a `/`-separated path
/// relative to the collection.
#[derive(Clone, Debug, PartialEq)]
pub struct GeneratedRequest {
    pub path: String,
    pub request: HttpRequest,
}

/// A generated collection, not yet written.
#[derive(Clone, Debug, PartialEq)]
pub struct Generated {
    /// Every file of the collection folder, `_collection.yaml`s included.
    pub files: Vec<GeneratedFile>,
    pub warnings: Vec<Diagnostic>,
    /// How many request files (one per operation).
    pub requests: usize,
    /// The request of each operation, in document order per folder.
    pub operations: Vec<GeneratedRequest>,
    /// The root `_collection.yaml`.
    pub root: CollectionMeta,
    /// One environment per server, written under `environments/`.
    pub environments: Vec<Environment>,
}

/// What [`generate_with`] needs beyond the model.
#[derive(Clone, Debug, Default)]
pub struct GenerateOptions<'a> {
    /// Recorded in the root `_collection.yaml`.
    pub source: Option<CollectionSource>,
    /// Where the document was read from, to resolve relative server URLs.
    pub location: Option<&'a Location>,
}

/// The default folder name of a model's collection: its title in kebab-case.
pub fn collection_dir(model: &ApiModel) -> String {
    names::file_name(&model.title, "api")
}

/// One folder of the collection: a tag and its operations.
struct Folder<'a> {
    tag: Option<&'a str>,
    dir: String,
    operations: Vec<&'a Operation>,
}

/// The collection for `model`.
pub fn generate(model: &ApiModel) -> Generated {
    generate_with(model, &GenerateOptions::default())
}

/// The collection for `model`, with its source recorded.
pub fn generate_with(model: &ApiModel, options: &GenerateOptions<'_>) -> Generated {
    let folders = folders(model);
    let mut warnings = Vec::new();
    let security = Security::of(model, &mut warnings);
    let environments =
        environments::environments(model, &security.secrets, options.location, &mut warnings);
    let root = CollectionMeta {
        name: Some(model.title.clone()),
        description: model.description.clone(),
        order: Some(folders.iter().map(|f| f.dir.clone()).collect()),
        auth: security.collection.clone(),
        source: options.source.clone().map(Box::new),
        ..CollectionMeta::default()
    };
    let mut generated = Generated {
        files: Vec::new(),
        warnings,
        requests: 0,
        operations: Vec::new(),
        root,
        environments,
    };
    let root_yaml = collection_to_yaml(&generated.root);
    generated.push(COLLECTION_FILE.to_owned(), root_yaml);
    for folder in &folders {
        generated.folder(model, &security, folder);
    }
    generated
}

impl Generated {
    fn push(&mut self, path: String, contents: String) {
        self.files.push(GeneratedFile { path, contents });
    }

    fn folder(&mut self, model: &ApiModel, security: &Security, folder: &Folder<'_>) {
        let mut stems: Vec<String> = Vec::new();
        let mut files = Vec::new();
        for op in &folder.operations {
            let stem = self.stem(op, &folder.dir, &stems);
            let auth = security.request(op);
            let context = RequestContext {
                components: &model.components,
                auth: auth.as_ref(),
            };
            let request = request_for(op, &context, &mut self.warnings);
            let path = format!("{}/{stem}{REQUEST_FILE_SUFFIX}", folder.dir);
            let yaml = request_to_yaml(&Request::Http(Box::new(request.clone())));
            self.operations.push(GeneratedRequest {
                path: path.clone(),
                request,
            });
            files.push((path, yaml));
            stems.push(stem);
        }
        let description = folder
            .tag
            .and_then(|t| model.tags.iter().find(|d| d.name == t))
            .and_then(|d| d.description.clone());
        let meta = CollectionMeta {
            name: Some(folder.tag.unwrap_or(UNTAGGED).to_owned()),
            description,
            order: Some(stems),
            ..CollectionMeta::default()
        };
        self.push(
            format!("{}/{COLLECTION_FILE}", folder.dir),
            collection_to_yaml(&meta),
        );
        self.requests += files.len();
        for (path, contents) in files {
            self.push(path, contents);
        }
    }

    /// The file stem of `op`, unique within its folder.
    fn stem(&mut self, op: &Operation, dir: &str, taken: &[String]) -> String {
        let fallback = names::method_and_path(&op.method, &op.path);
        let base = match &op.operation_id {
            Some(id) => names::file_name(id, &fallback),
            None => names::file_name(&fallback, "request"),
        };
        let stem = names::unique(&base, taken);
        if stem != base {
            let message = format!(
                "the file name '{base}' is already used in '{dir}'; this operation is written to '{stem}{REQUEST_FILE_SUFFIX}'"
            );
            self.warnings
                .push(Diagnostic::warning(&op.pointer, message));
        }
        stem
    }
}

/// Operations grouped by first tag: declared tags first, in declaration
/// order, then undeclared tags as they appear, then `untagged`.
fn folders(model: &ApiModel) -> Vec<Folder<'_>> {
    let mut keys: Vec<Option<&str>> = Vec::new();
    let first_tags: Vec<Option<&str>> = model
        .operations
        .iter()
        .map(|op| op.tags.first().map(String::as_str))
        .collect();
    let declared = model.tags.iter().map(|t| Some(t.name.as_str()));
    for key in declared.chain(first_tags.iter().copied().filter(Option::is_some)) {
        if first_tags.contains(&key) && !keys.contains(&key) {
            keys.push(key);
        }
    }
    if first_tags.contains(&None) {
        keys.push(None);
    }
    let mut dirs: Vec<String> = Vec::new();
    keys.into_iter()
        .map(|tag| {
            let dir = names::unique(&names::file_name(tag.unwrap_or(UNTAGGED), UNTAGGED), &dirs);
            dirs.push(dir.clone());
            let operations = model
                .operations
                .iter()
                .zip(&first_tags)
                .filter(|(_, first)| **first == tag)
                .map(|(op, _)| op)
                .collect();
            Folder {
                tag,
                dir,
                operations,
            }
        })
        .collect()
}
