//! The conversion pipeline (design D1): translate variables, extract
//! secrets, suggest assertions, then lay the tree out as files with
//! deterministic names and the conflict policy (task 1.5). Planning only
//! reads the workspace; [`crate::apply`] writes.

use std::collections::HashSet;
use std::path::Path;

use rustman_core::Redactor;
use rustman_core::collection::{COLLECTION_FILE, CollectionMeta};
use rustman_core::format::from_yaml;
use rustman_core::format::write::{collection_to_yaml, environment_to_yaml};
use rustman_core::model::REQUEST_FILE_SUFFIX;

use crate::detect::Format;
use crate::envs;
use crate::error::ImportError;
use crate::ir::{Folder, ImportedCollection, ImportedRequest, Item, Location, Note, NoteKind};
use crate::render::{Target, collection_file, request_file};
use crate::report::{
    Action, Conflict, FileEntry, FileKind, Migration, Report, RequestMigration, SecretEntry,
    SourceInfo,
};
use crate::secrets::{Existing, MovedSecret, environment_name, extract, redact_tokens};
use crate::slug::{Slugs, path_limit_error, slug};
use crate::{scripts, translate};

/// What to do with a file that already exists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OnConflict {
    /// Keep the existing file.
    #[default]
    Skip,
    /// Replace it.
    Overwrite,
    /// Write next to it, with the next free suffix.
    Rename,
}

/// Where and how to import.
pub struct PlanOptions<'a> {
    /// The workspace root.
    pub root: &'a Path,
    /// The source as shown in messages and the report.
    pub source: String,
    pub format: Format,
    /// The collection folder relative to the root, with `/` separators;
    /// `None` for the slug of the source collection's name.
    pub destination: Option<String>,
    pub on_conflict: OnConflict,
    /// The environment that receives secrets moved out of requests; `None`
    /// for a new one named after the source collection.
    pub environment: Option<String>,
    pub existing: Existing<'a>,
    pub dry_run: bool,
}

/// A file to write, relative to the workspace root.
#[derive(Clone, Debug)]
pub struct PlannedFile {
    pub path: String,
    pub kind: FileKind,
    pub action: Action,
    pub text: String,
}

/// Everything an import would do. Its `Debug` never shows secret values.
#[derive(Debug)]
pub struct Plan {
    pub files: Vec<PlannedFile>,
    /// Secrets to store, by environment.
    pub secrets: Vec<MovedSecret>,
    pub report: Report,
}

/// The default collection folder for a source named `name`.
pub fn default_destination(name: &str) -> String {
    let dest = slug(name, "imported");
    if dest == rustman_core::environment::ENVIRONMENTS_DIR {
        format!("{dest}-collection")
    } else {
        dest
    }
}

/// Give environments whose names share a file name (`Prod` and `prod`)
/// distinct names (`prod-2`), with a note, so none is merged into another.
fn distinct_environment_names(collection: &mut ImportedCollection) {
    let mut slugs = Slugs::default();
    for env in &mut collection.environments {
        let file = environment_name(&env.name);
        let unique = slugs.unique(&file, "imported");
        if unique != file {
            let message = format!(
                "environment '{}' imported as '{unique}': its file name is taken by another environment",
                env.name
            );
            collection
                .notes
                .push(Note::new(NoteKind::Info, message, &env.location));
            env.name = unique;
        }
    }
}

/// Convert `collection` into the files and secrets of an import.
pub fn plan(mut collection: ImportedCollection, o: &PlanOptions<'_>) -> Result<Plan, ImportError> {
    translate::translate(&mut collection);
    crate::pathvars::separate(&mut collection);
    collection.name = redact_tokens(&collection.name);
    distinct_environment_names(&mut collection);
    let target = o
        .environment
        .clone()
        .unwrap_or_else(|| environment_name(&collection.name));
    let mut moved = extract(&mut collection, &target, &o.existing);
    scripts::suggest(&mut collection);
    let redactor = moved.iter().fold(Redactor::new(), |r, m| r.with(&m.secret));
    let mut layout = Layout {
        root: o.root,
        policy: o.on_conflict,
        redactor,
        files: Vec::new(),
        planned: HashSet::new(),
        conflicts: Vec::new(),
        notes: Vec::new(),
        migration: Vec::new(),
    };
    let has_tree = !collection.root.items.is_empty() || collection.environments.is_empty();
    let destination = has_tree.then(|| {
        o.destination
            .clone()
            .unwrap_or_else(|| default_destination(&collection.name))
    });
    if let Some(dest) = &destination {
        layout.folder(&collection.root, dest, &collection.name)?;
    }
    let kept = layout.environments(&collection, &moved, &o.existing);
    if let Some(message) = layout.files.iter().find_map(|f| path_limit_error(&f.path)) {
        return Err(ImportError::invalid(&o.source, message));
    }
    let kept: HashSet<(String, String)> = kept.into_iter().collect();
    moved.retain(|m| !kept.contains(&(m.environment.clone(), m.secret.name().to_owned())));
    let env_path = format!("environments/{target}.yaml");
    let created = layout
        .files
        .iter()
        .any(|f| f.path == env_path && f.action == Action::Create);
    if created && moved.iter().any(|m| !m.from_environment) {
        let why = if o.environment.is_none() {
            " (no active environment)"
        } else {
            ""
        };
        let message = format!("secrets stored in new environment '{target}'{why}");
        let at = Location::item(&o.source, "");
        layout.notes.push(Note::new(NoteKind::Info, message, &at));
    }
    let report = report(&collection, o, destination, &layout, &moved);
    Ok(Plan {
        files: layout.files,
        secrets: moved,
        report,
    })
}

fn report(
    collection: &ImportedCollection,
    o: &PlanOptions<'_>,
    destination: Option<String>,
    layout: &Layout<'_>,
    moved: &[MovedSecret],
) -> Report {
    let mut notes = collection.notes.clone();
    notes.extend(layout.notes.iter().cloned());
    let mut report = Report {
        schema: crate::report::SCHEMA,
        source: SourceInfo {
            path: o.source.clone(),
            format: o.format.as_str().to_owned(),
            name: collection.name.clone(),
        },
        destination,
        dry_run: o.dry_run,
        summary: Default::default(),
        files: layout
            .files
            .iter()
            .map(|f| FileEntry {
                path: f.path.clone(),
                kind: f.kind,
                action: f.action,
            })
            .collect(),
        conflicts: layout.conflicts.clone(),
        secrets: moved
            .iter()
            .map(|m| SecretEntry {
                name: m.secret.name().to_owned(),
                environment: m.environment.clone(),
                location: m.location.clone(),
            })
            .collect(),
        notes,
        migration: Migration::new(layout.migration.clone()),
    };
    report.count();
    report
}

struct Layout<'a> {
    root: &'a Path,
    policy: OnConflict,
    redactor: Redactor,
    files: Vec<PlannedFile>,
    /// The paths of `files`, for constant-time conflict checks.
    planned: HashSet<String>,
    conflicts: Vec<Conflict>,
    notes: Vec<Note>,
    migration: Vec<RequestMigration>,
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_owned()
    } else {
        format!("{dir}/{name}")
    }
}

impl Layout<'_> {
    fn target<'b>(&'b self, path: &'b str) -> Target<'b> {
        Target {
            root: self.root,
            path,
            redactor: &self.redactor,
        }
    }

    /// Plan `folder` (named `name`) into the directory `dir`.
    fn folder(&mut self, folder: &Folder, dir: &str, name: &str) -> Result<(), ImportError> {
        let mut slugs = Slugs::default();
        let mut order = Vec::new();
        for item in &folder.items {
            match item {
                Item::Folder(child) => {
                    let s = slugs.unique(&child.name, "folder");
                    self.folder(child, &join(dir, &s), &child.name)?;
                    order.push(s);
                }
                Item::Request(request) => {
                    let s = slugs.unique(&request.name, "request");
                    order.push(self.request(request, dir, &s)?);
                }
            }
        }
        let path = join(dir, COLLECTION_FILE);
        if let Some((text, added)) = self.extended_order(&path, &order) {
            self.conflict(&path, format!("order extended with {}", added.join(", ")));
            self.push(path, FileKind::Collection, Action::Update, text);
            return Ok(());
        }
        let text = collection_file(folder, name, order, &self.target(&path))?;
        self.place(path, text, FileKind::Collection);
        Ok(())
    }

    /// Under `rename`, the existing `_collection.yaml` at `path` with the
    /// entries of `order` it lacks (renamed requests) appended to its
    /// `order:`, and those entries. `None` when there is nothing to add, or
    /// the file has no `order:` (file-name order) or cannot be read.
    fn extended_order(&self, path: &str, order: &[String]) -> Option<(String, Vec<String>)> {
        if self.policy != OnConflict::Rename {
            return None;
        }
        let existing = self.existing_text(path)?;
        let mut meta: CollectionMeta = from_yaml(Path::new(path), &existing).ok()?;
        let current = meta.order.as_mut()?;
        let added: Vec<String> = order
            .iter()
            .filter(|s| !current.contains(s))
            .cloned()
            .collect();
        if added.is_empty() {
            return None;
        }
        current.extend(added.iter().cloned());
        let comments: String = existing
            .lines()
            .take_while(|l| l.starts_with('#'))
            .map(|l| format!("{l}\n"))
            .collect();
        Some((format!("{comments}{}", collection_to_yaml(&meta)), added))
    }

    /// Plan one request file; returns the stem it ends up with.
    fn request(
        &mut self,
        r: &ImportedRequest,
        dir: &str,
        slug: &str,
    ) -> Result<String, ImportError> {
        let path = join(dir, &format!("{slug}{REQUEST_FILE_SUFFIX}"));
        let mut notes = Vec::new();
        let text = request_file(r, &self.target(&path), &mut notes)?;
        self.notes.extend(notes);
        let placed = self.place(path, text, FileKind::Request);
        self.migrated(r, &placed);
        let name = placed.rsplit('/').next().unwrap_or(&placed);
        Ok(name.trim_end_matches(REQUEST_FILE_SUFFIX).to_owned())
    }

    /// Record the script statements of `r`, written to `path`.
    fn migrated(&mut self, r: &ImportedRequest, path: &str) {
        if r.statements.is_empty() {
            return;
        }
        let mapped = r.statements.iter().filter(|s| s.mapped).count();
        let statements: Vec<String> = r
            .statements
            .iter()
            .filter(|s| !s.mapped)
            .map(|s| self.redactor.text(&s.text).into_owned())
            .collect();
        self.migration.push(RequestMigration {
            request: r.location.item.clone().unwrap_or_else(|| r.name.clone()),
            path: path.to_owned(),
            found: r.statements.len(),
            mapped,
            unconverted: statements.len(),
            statements,
        });
    }

    fn is_planned(&self, path: &str) -> bool {
        self.planned.contains(path)
    }

    fn existing_text(&self, path: &str) -> Option<String> {
        let full = self.root.join(path);
        full.exists()
            .then(|| std::fs::read_to_string(full).unwrap_or_default())
    }

    /// Add `text` at `path` under the conflict policy; returns the path used.
    fn place(&mut self, path: String, text: String, kind: FileKind) -> String {
        let existing = self.existing_text(&path);
        if existing.is_none() && !self.is_planned(&path) {
            return self.push(path, kind, Action::Create, text);
        }
        let differs = existing.as_deref() != Some(text.as_str());
        if !differs && !self.is_planned(&path) {
            return self.push(path, kind, Action::Unchanged, text);
        }
        let rename = kind == FileKind::Request
            && (self.is_planned(&path) || (self.policy == OnConflict::Rename && differs));
        if rename {
            let free = self.free_name(&path);
            self.conflict(&path, format!("renamed to {free}"));
            return self.push(free, kind, Action::Create, text);
        }
        if self.policy == OnConflict::Overwrite && differs {
            self.conflict(&path, "overwritten".to_owned());
            return self.push(path, kind, Action::Overwrite, text);
        }
        self.conflict(&path, "skipped (exists)".to_owned());
        self.push(path, kind, Action::Skip, text)
    }

    fn push(&mut self, path: String, kind: FileKind, action: Action, text: String) -> String {
        self.planned.insert(path.clone());
        self.files.push(PlannedFile {
            path: path.clone(),
            kind,
            action,
            text,
        });
        path
    }

    fn conflict(&mut self, path: &str, resolution: String) {
        self.conflicts.push(Conflict {
            path: path.to_owned(),
            resolution,
        });
    }

    /// `x-2.rman.yaml`, `x-3.rman.yaml`...: the first that neither exists
    /// nor is planned.
    fn free_name(&self, path: &str) -> String {
        let stem = path.trim_end_matches(REQUEST_FILE_SUFFIX);
        (2..)
            .map(|n| format!("{stem}-{n}{REQUEST_FILE_SUFFIX}"))
            .find(|p| !self.is_planned(p) && !self.root.join(p).exists())
            .expect("some suffix is free")
    }

    /// Plan the environment files; returns the (environment, secret) pairs
    /// whose stored values are kept.
    fn environments(
        &mut self,
        collection: &ImportedCollection,
        moved: &[MovedSecret],
        existing: &Existing<'_>,
    ) -> Vec<(String, String)> {
        let mut kept = Vec::new();
        for change in envs::changes(collection, moved) {
            let merged = envs::merge(&change, existing, self.policy);
            let exists = existing.environments.iter().any(|e| e.name == change.name);
            let action = match (exists, merged.changed) {
                (false, _) => Action::Create,
                (true, true) => Action::Update,
                (true, false) => Action::Skip,
            };
            let path = format!("environments/{}.yaml", change.name);
            self.conflicts.extend(merged.conflicts);
            kept.extend(
                merged
                    .kept_secrets
                    .into_iter()
                    .map(|s| (change.name.clone(), s)),
            );
            let text = environment_to_yaml(&merged.env);
            self.push(path, FileKind::Environment, action, text);
        }
        kept
    }
}
