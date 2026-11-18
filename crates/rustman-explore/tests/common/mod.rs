//! Helpers shared by the import tests.
#![allow(dead_code)]

use std::path::PathBuf;

use rustman_explore::{ImportError, ImportOptions, Imported, import};

/// The path of a fixture under `tests/fixtures`.
pub fn fixture(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
        .to_string_lossy()
        .into_owned()
}

pub async fn load(name: &str) -> Imported {
    match import(&fixture(name), &ImportOptions::default()).await {
        Ok(imported) => imported,
        Err(e) => panic!("{name}: {e}"),
    }
}

pub async fn load_err(name: &str) -> ImportError {
    import(&fixture(name), &ImportOptions::default())
        .await
        .err()
        .unwrap_or_else(|| panic!("{name} imported"))
}

/// The `bad-api` fixture's document, imported from a temporary file.
pub async fn load_bad_api() -> Imported {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad-api.yaml");
    std::fs::write(&path, rustman_test_support::bad_api::OPENAPI).unwrap();
    match import(&path.to_string_lossy(), &ImportOptions::default()).await {
        Ok(imported) => imported,
        Err(e) => panic!("bad-api: {e}"),
    }
}
