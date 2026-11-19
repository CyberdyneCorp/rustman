//! The real-world corpus (task 2.5): version pairs in
//! `fixtures/contract/corpus/<name>/{old,new}.yaml`, loaded from disk as
//! `rustman contract diff` loads them, with the expected differences in
//! `expected.txt` next to them. Regenerate an expectation deliberately with
//! `RUSTMAN_UPDATE_CORPUS=1 cargo test -p rustman-contract corpus`.

mod common;

use std::path::Path;

use rustman_contract::{Diff, Spec, diff};
use rustman_explore::ImportOptions;

fn load(path: &Path) -> Spec {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let source = path.to_string_lossy();
    runtime
        .block_on(Spec::load(&source, &ImportOptions::default()))
        .unwrap_or_else(|e| panic!("{source}: {e}"))
}

/// One line per difference; document names are left out (they are paths).
fn lines(diff: &Diff) -> String {
    let mut out = format!(
        "catalog version {}\n{}\n",
        diff.catalog_version,
        diff.summary()
    );
    for d in &diff.differences {
        out.push_str(&format!(
            "{} {} | {} | {}\n    old: {}\n    new: {}\n",
            d.classification, d.rule, d.operation, d.message, d.old_pointer, d.new_pointer
        ));
    }
    out
}

fn check(name: &str) {
    let dir = common::fixtures().join("corpus").join(name);
    let old = load(&dir.join("old.yaml"));
    let new = load(&dir.join("new.yaml"));
    let first = diff(&old, &new);
    for _ in 0..2 {
        assert_eq!(
            diff(&old, &new),
            first,
            "{name}: the diff is not deterministic"
        );
    }
    let actual = lines(&first);
    let expected_path = dir.join("expected.txt");
    if std::env::var_os("RUSTMAN_UPDATE_CORPUS").is_some() {
        std::fs::write(&expected_path, &actual).unwrap();
    }
    let expected = std::fs::read_to_string(&expected_path)
        .unwrap_or_else(|e| panic!("{}: {e}", expected_path.display()))
        .replace("\r\n", "\n");
    assert!(
        expected == actual,
        "{name}: expected\n{expected}\ngot\n{actual}"
    );
}

#[test]
fn corpus_petstore() {
    check("petstore");
}

#[test]
fn corpus_stripe() {
    check("stripe");
}

#[test]
fn corpus_github() {
    check("github");
}

#[test]
fn corpus_documents_compared_with_themselves_have_no_difference() {
    for name in ["petstore", "stripe", "github"] {
        let dir = common::fixtures().join("corpus").join(name);
        for side in ["old.yaml", "new.yaml"] {
            let spec = load(&dir.join(side));
            let same = diff(&spec, &spec);
            assert!(same.differences.is_empty(), "{name}/{side}: {:#?}", same);
        }
    }
}
