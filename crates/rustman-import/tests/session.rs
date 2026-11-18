//! `session::run`, the import the TUI's "Import…" action runs: a dry run
//! writes nothing, a real one writes the planned files and stores the
//! moved secrets, and the summary never shows a secret value.

mod common;

use common::fixture_path;
use rustman_core::{SecretBackend, SecretStore};
use rustman_import::session::{Session, run};

const BILLING: &str = "postman/billing-api.postman_collection.json";

/// Every value the fixtures inject as a secret contains this marker.
const CANARY: &str = "CANARY";

fn workspace() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    rustman_core::init::init(&root, "ws", false).unwrap();
    (tmp, root)
}

fn session<'a>(root: &'a std::path::Path, src: &'a std::path::Path, dry_run: bool) -> Session<'a> {
    Session {
        root,
        src,
        shown: "billing.json",
        environment: Some("local"),
        backend: SecretBackend::File,
        dry_run,
    }
}

fn files(root: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else {
                out.push(path.strip_prefix(root).unwrap().display().to_string());
            }
        }
    }
    out.sort();
    out
}

#[test]
fn a_dry_run_previews_without_writing() {
    let (_tmp, root) = workspace();
    let before = files(&root);
    let src = fixture_path(BILLING);
    let outcome = run(&session(&root, &src, true), &mut Vec::new()).unwrap();
    assert_eq!(files(&root), before);
    assert!(
        outcome
            .summary
            .starts_with("would import Billing API (postman)")
    );
    assert!(outcome.summary.contains("dry run: nothing was written"));
    assert!(
        outcome.summary.contains("  create billing-api/"),
        "{}",
        outcome.summary
    );
    assert!(outcome.requests > 0);
    assert!(!outcome.summary.contains(CANARY), "{}", outcome.summary);
}

#[test]
fn a_real_import_writes_files_and_stores_secrets() {
    let (_tmp, root) = workspace();
    let src = fixture_path(BILLING);
    let outcome = run(&session(&root, &src, false), &mut Vec::new()).unwrap();
    assert!(
        outcome
            .summary
            .starts_with("imported Billing API (postman)")
    );
    assert!(!outcome.summary.contains(CANARY), "{}", outcome.summary);
    assert!(root.join("billing-api/_collection.yaml").is_file());
    let store = SecretStore::open(&root, SecretBackend::File);
    let env = rustman_core::Workspace::load(&root).unwrap();
    let local = env.environments.iter().find(|e| e.name == "local").unwrap();
    assert!(!local.secrets.is_empty());
    let stored = store.get("local", &local.secrets[0]).unwrap().unwrap();
    assert!(stored.expose().contains(CANARY));
    for file in files(&root).iter().filter(|f| !f.starts_with(".rustman")) {
        let text = std::fs::read_to_string(root.join(file)).unwrap_or_default();
        assert!(!text.contains(CANARY), "{file}");
    }
}

#[test]
fn an_undetectable_source_is_an_error() {
    let (tmp, root) = workspace();
    let src = tmp.path().join("notes.txt");
    std::fs::write(&src, "hello").unwrap();
    let err = run(&session(&root, &src, true), &mut Vec::new()).unwrap_err();
    assert!(err.to_string().contains("cannot detect format"), "{err}");
}
