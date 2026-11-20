//! The catalog's versioning and its generated documentation (tasks 2.4 and
//! 2.6).
//!
//! `tests/catalog/<version>.txt` records every rule and classification of
//! each catalog version. Changing a classification, or adding or removing
//! a rule, fails `classifications_match_the_catalog_version` until
//! `CATALOG_VERSION` is bumped and the new version's file is added; files
//! of released versions are never edited.

use std::path::{Path, PathBuf};

use rustman_contract::catalog::{self, CATALOG_VERSION, RuleId};

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/catalog")
}

fn classifications() -> String {
    RuleId::ALL
        .iter()
        .map(|rule| format!("{} {}\n", rule.id(), rule.classification()))
        .collect()
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .replace("\r\n", "\n")
}

#[test]
fn classifications_match_the_catalog_version() {
    let path = dir().join(format!("{CATALOG_VERSION}.txt"));
    if std::env::var_os("RUSTMAN_UPDATE_CATALOG").is_some() && !path.exists() {
        std::fs::write(&path, classifications()).unwrap();
    }
    assert!(
        path.exists(),
        "no {} for catalog version {CATALOG_VERSION}; record it with RUSTMAN_UPDATE_CATALOG=1",
        path.display()
    );
    assert_eq!(
        read(&path),
        classifications(),
        "the rule catalog changed: bump CATALOG_VERSION and record the new version \
         with RUSTMAN_UPDATE_CATALOG=1 cargo test -p rustman-contract --test catalog"
    );
}

#[test]
fn every_recorded_version_differs_from_the_previous_one() {
    let mut versions: Vec<u32> = std::fs::read_dir(dir())
        .unwrap()
        .filter_map(|e| {
            let name = e.unwrap().file_name().to_string_lossy().into_owned();
            name.strip_suffix(".txt")?.parse().ok()
        })
        .collect();
    versions.sort_unstable();
    assert_eq!(versions.last(), Some(&CATALOG_VERSION));
    for pair in versions.windows(2) {
        let older = read(&dir().join(format!("{}.txt", pair[0])));
        let newer = read(&dir().join(format!("{}.txt", pair[1])));
        assert_ne!(
            older, newer,
            "versions {} and {} are the same",
            pair[0], pair[1]
        );
    }
}

#[test]
fn the_json_listing_has_every_rule_and_the_version() {
    let json = serde_json::to_value(catalog::listing()).unwrap();
    let list = json.as_array().unwrap();
    assert_eq!(list.len(), RuleId::ALL.len());
    for (entry, rule) in list.iter().zip(RuleId::ALL) {
        assert_eq!(entry["id"], rule.id());
        assert_eq!(entry["classification"], rule.classification().as_str());
        assert!(entry["description"].is_string());
        assert!(entry["example"].is_string());
        assert_eq!(entry["catalog_version"], CATALOG_VERSION);
    }
}

#[test]
fn the_rule_catalog_page_is_up_to_date() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/contract-rules.md");
    let generated = catalog::markdown();
    if std::env::var_os("RUSTMAN_UPDATE_DOCS").is_some() {
        std::fs::write(&path, &generated).unwrap();
    }
    assert!(
        read(&path) == generated,
        "docs/contract-rules.md is stale; regenerate it with \
         RUSTMAN_UPDATE_DOCS=1 cargo test -p rustman-contract --test catalog"
    );
    for rule in RuleId::ALL {
        assert!(generated.contains(&format!("| `{}` |", rule.id())));
    }
}
