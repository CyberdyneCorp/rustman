//! Fixture pairs: one YAML file with `old` and `new` documents (each
//! completed with `openapi: 3.1.0`, `info` and `paths` when missing) and the
//! `expect`ed rule ids, in report order.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use rustman_contract::{Diff, Spec, diff};
use rustman_explore::openapi::document;
use serde_json::{Value, json};

pub fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/contract")
}

pub struct Pair {
    pub diff: Diff,
    pub expect: Vec<String>,
}

/// Load `fixtures/contract/<relative>` and diff its two documents.
pub fn pair(relative: &str) -> Pair {
    let path = fixtures().join(relative);
    let text = std::fs::read_to_string(&path).unwrap();
    let (_, raw) = document::parse(&text, &path.to_string_lossy()).unwrap();
    let old = spec("old.yaml", complete(&raw["old"]));
    let new = spec("new.yaml", complete(&raw["new"]));
    let expect = raw["expect"]
        .as_array()
        .map(|list| {
            list.iter()
                .map(|v| v.as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default();
    Pair {
        diff: diff(&old, &new),
        expect,
    }
}

/// Diff two documents given inline.
pub fn diff_values(old: Value, new: Value) -> Diff {
    diff(
        &spec("old.yaml", complete(&old)),
        &spec("new.yaml", complete(&new)),
    )
}

pub fn spec(name: &str, doc: Value) -> Spec {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    runtime
        .block_on(Spec::from_value(name, doc))
        .unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn complete(doc: &Value) -> Value {
    let mut doc = doc.clone();
    let map = doc.as_object_mut().expect("a document is a mapping");
    map.entry("openapi").or_insert(json!("3.1.0"));
    map.entry("info")
        .or_insert(json!({"title": "fixture", "version": "1"}));
    map.entry("paths").or_insert(json!({}));
    doc
}

/// The rule ids reported, in order.
pub fn rule_ids(diff: &Diff) -> Vec<String> {
    diff.differences
        .iter()
        .map(|d| d.rule.id().to_owned())
        .collect()
}
