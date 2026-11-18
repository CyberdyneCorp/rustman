//! Insomnia, Bruno, HAR and HTTP sources (tasks 3.1-4.2) through the shared
//! pipeline: detection, snapshots, loadable files, no secret in any output,
//! reproducible trees and a valid report.

mod common;

use std::fs;
use std::path::Path;
use std::sync::Mutex;

use common::{SOURCE_FIXTURES, Setup, apply, fixture_path, plan_source, read_tree, tree};
use rustman_core::model::KindRegistry;
use rustman_import::{SourceOptions, load};

const CANARY: &str = "CANARY";

/// `bruno/shop` → `bruno-shop`, `har/shop-capture.har` → `har-shop-capture`.
fn snapshot_name(name: &str) -> String {
    let stem = name.split('.').next().unwrap();
    stem.replace('/', "-")
}

#[test]
fn every_source_fixture_is_detected_with_its_format() {
    for (name, format) in SOURCE_FIXTURES {
        let (found, _) = load(None, &fixture_path(name), name, &SourceOptions::default()).unwrap();
        assert_eq!(found, *format, "{name}");
    }
}

#[test]
fn source_fixture_snapshots() {
    for (name, _) in SOURCE_FIXTURES {
        let plan = plan_source(name);
        let mut settings = insta::Settings::clone_current();
        settings.set_prepend_module_to_snapshot(false);
        settings.bind(|| {
            insta::assert_snapshot!(snapshot_name(name), tree(&plan));
            insta::assert_snapshot!(
                format!("{}-summary", snapshot_name(name)),
                plan.report.summary_text()
            );
        });
    }
}

#[test]
fn every_written_request_file_loads() {
    let registry = KindRegistry::default();
    for (name, _) in SOURCE_FIXTURES {
        let plan = plan_source(name);
        let requests = plan.files.iter().filter(|f| f.path.ends_with(".rman.yaml"));
        for f in requests {
            let loaded = registry.load(Path::new(&f.path), &f.text);
            assert!(loaded.is_ok(), "{name}: {}: {loaded:?}\n{}", f.path, f.text);
        }
    }
}

#[test]
fn no_source_output_contains_an_injected_secret() {
    for (name, _) in SOURCE_FIXTURES {
        let plan = plan_source(name);
        for f in &plan.files {
            assert!(!f.text.contains(CANARY), "{name}: {}\n{}", f.path, f.text);
        }
        assert!(!plan.report.to_json().contains(CANARY), "{name}");
        assert!(!plan.report.summary_text().contains(CANARY), "{name}");
        assert!(!format!("{plan:?}").contains(CANARY), "{name}");
    }
}

#[test]
fn applying_writes_no_secret_into_the_workspace_and_stores_them_all() {
    let dir = tempfile::tempdir().unwrap();
    let store = Mutex::new(Vec::new());
    for (name, _) in SOURCE_FIXTURES {
        let plan = Setup::new(dir.path()).plan_source(name, &SourceOptions::default());
        apply(&plan, dir.path(), &store);
    }
    for (path, bytes) in read_tree(dir.path()) {
        let text = String::from_utf8(bytes).unwrap();
        assert!(!text.contains(CANARY), "{path}\n{text}");
    }
    let stored: Vec<String> = store.lock().unwrap().iter().map(|s| s.2.clone()).collect();
    let mut injected: Vec<String> = Vec::new();
    for (name, _) in SOURCE_FIXTURES {
        collect_canaries(&fixture_path(name), &mut injected);
    }
    // The HAR response body is never read, so it is never stored either.
    injected.retain(|c| !c.contains("response-body"));
    for canary in injected {
        assert!(
            stored.iter().any(|s| s.contains(&canary)),
            "{canary} not stored: {stored:?}"
        );
    }
}

/// Every `CANARY-...` token in the files under `path`.
fn collect_canaries(path: &Path, out: &mut Vec<String>) {
    if path.is_dir() {
        for entry in fs::read_dir(path).unwrap() {
            collect_canaries(&entry.unwrap().path(), out);
        }
        return;
    }
    let text = fs::read_to_string(path).unwrap();
    for (at, _) in text.match_indices(CANARY) {
        let token: String = text[at..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        if !out.contains(&token) {
            out.push(token);
        }
    }
}

#[test]
fn importing_twice_into_empty_roots_is_byte_identical() {
    let store = Mutex::new(Vec::new());
    let trees: Vec<_> = (0..2)
        .map(|_| {
            let dir = tempfile::tempdir().unwrap();
            for (name, _) in SOURCE_FIXTURES {
                let plan = Setup::new(dir.path()).plan_source(name, &SourceOptions::default());
                apply(&plan, dir.path(), &store);
            }
            read_tree(dir.path())
        })
        .collect();
    assert!(trees[0].len() > 20);
    assert_eq!(trees[0], trees[1]);
}

#[test]
fn the_json_report_validates_for_every_source() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/schemas/import-report.schema.json");
    let schema: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    for (name, _) in SOURCE_FIXTURES {
        let report: serde_json::Value =
            serde_json::from_str(&plan_source(name).report.to_json()).unwrap();
        let errors: Vec<String> = validator
            .iter_errors(&report)
            .map(|e| e.to_string())
            .collect();
        assert!(errors.is_empty(), "{name}: {errors:#?}");
    }
}
