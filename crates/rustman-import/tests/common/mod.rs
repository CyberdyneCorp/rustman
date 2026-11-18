//! Helpers for the import tests: fixtures, planning into a temporary
//! workspace root, and a text view of the planned tree.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use rustman_core::{Environment, Secret};
use rustman_import::ir::ImportedCollection;
use rustman_import::secrets::Existing;
use rustman_import::{Format, OnConflict, Plan, PlanOptions, SourceOptions, load, plan};

/// The path of fixture `name` under `tests/fixtures/`.
pub fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// The text of fixture `name` under `tests/fixtures/`.
pub fn fixture(name: &str) -> String {
    fs::read_to_string(fixture_path(name)).unwrap()
}

/// How an import is planned in a test.
pub struct Setup<'a> {
    pub root: &'a Path,
    pub policy: OnConflict,
    pub environment: Option<&'a str>,
    pub environments: &'a [Environment],
    pub lookup: &'a dyn Fn(&str, &str) -> Option<Secret<String>>,
}

impl<'a> Setup<'a> {
    pub fn new(root: &'a Path) -> Self {
        Self {
            root,
            policy: OnConflict::Skip,
            environment: Some("local"),
            environments: &[],
            lookup: &|_, _| None,
        }
    }

    /// Plan the fixture `name` (a Postman collection or environment).
    pub fn plan(&self, name: &str) -> Plan {
        let text = fixture(name);
        let format = rustman_import::detect(&text).expect("the fixture is detected");
        let shown = name.rsplit('/').next().unwrap();
        let collection = format.parse(&text, shown).unwrap();
        self.plan_collection(format, collection, shown)
    }

    /// Plan the fixture file or folder `name` through [`load`], detecting
    /// its format, with `options`.
    pub fn plan_source(&self, name: &str, options: &SourceOptions) -> Plan {
        let path = fixture_path(name);
        let shown = name.rsplit('/').next().unwrap();
        let (format, collection) = load(None, &path, shown, options).unwrap();
        self.plan_collection(format, collection, shown)
    }

    pub fn plan_collection(
        &self,
        format: Format,
        collection: ImportedCollection,
        shown: &str,
    ) -> Plan {
        let options = PlanOptions {
            root: self.root,
            source: shown.to_owned(),
            format,
            destination: None,
            on_conflict: self.policy,
            environment: self.environment.map(str::to_owned),
            existing: Existing {
                environments: self.environments,
                lookup: self.lookup,
            },
            dry_run: false,
        };
        plan(collection, &options).unwrap()
    }
}

/// Plan the fixture file or folder `name` into an empty root with the
/// defaults, through [`load`].
pub fn plan_source(name: &str) -> Plan {
    let dir = tempfile::tempdir().unwrap();
    Setup::new(dir.path()).plan_source(name, &SourceOptions::default())
}

/// Plan `name` into an empty root with the defaults.
pub fn plan_fixture(name: &str) -> Plan {
    let dir = tempfile::tempdir().unwrap();
    Setup::new(dir.path()).plan(name)
}

/// Every planned file as `=== path (action)` and its text.
pub fn tree(plan: &Plan) -> String {
    let mut out = String::new();
    for file in &plan.files {
        out.push_str(&format!(
            "=== {} ({})\n{}",
            file.path,
            file.action.as_str(),
            file.text
        ));
    }
    out
}

/// The planned file at `path`.
pub fn file<'p>(plan: &'p Plan, path: &str) -> &'p str {
    let found = plan.files.iter().find(|f| f.path == path);
    &found
        .unwrap_or_else(|| panic!("no {path} in\n{}", tree(plan)))
        .text
}

/// Apply `plan` under `root`, keeping stored secrets in `store`.
pub fn apply(plan: &Plan, root: &Path, store: &std::sync::Mutex<Vec<(String, String, String)>>) {
    let save = |batch: &[(&str, &Secret<String>)]| {
        for (env, secret) in batch {
            store.lock().unwrap().push((
                (*env).to_owned(),
                secret.name().to_owned(),
                secret.expose().clone(),
            ));
        }
        Ok(())
    };
    rustman_import::apply(plan, root, &save).unwrap();
}

/// Every file under `root`, by `/`-separated relative path.
pub fn read_tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut dirs = vec![PathBuf::new()];
    while let Some(rel) = dirs.pop() {
        for entry in fs::read_dir(root.join(&rel)).unwrap() {
            let entry = entry.unwrap();
            let child = rel.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                dirs.push(child);
            } else {
                let key: Vec<String> = child
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect();
                out.insert(key.join("/"), fs::read(root.join(&child)).unwrap());
            }
        }
    }
    out
}

/// The Postman fixtures, with their formats.
pub const POSTMAN_FIXTURES: &[(&str, Format)] = &[
    (
        "postman/billing-api.postman_collection.json",
        Format::Postman,
    ),
    (
        "postman/legacy-v20.postman_collection.json",
        Format::Postman,
    ),
    (
        "postman/body-modes.postman_collection.json",
        Format::Postman,
    ),
    (
        "postman/auth-types.postman_collection.json",
        Format::Postman,
    ),
    ("postman/migration.postman_collection.json", Format::Postman),
    (
        "postman/staging.postman_environment.json",
        Format::PostmanEnv,
    ),
];

/// The Insomnia, Bruno, HAR and HTTP fixtures (files and folders), with
/// their formats.
pub const SOURCE_FIXTURES: &[(&str, Format)] = &[
    ("insomnia/shop-v4.json", Format::Insomnia),
    ("insomnia/shop-v5.yaml", Format::Insomnia),
    ("bruno/shop", Format::Bruno),
    ("har/shop-capture.har", Format::Har),
    ("http/jetbrains/api.http", Format::Http),
    ("http/rest-client", Format::Http),
];
