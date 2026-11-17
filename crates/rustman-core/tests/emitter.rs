//! Deterministic emitter (task 2.6): golden-file round trips.

use std::fs;
use std::path::{Path, PathBuf};

use rustman_core::collection::CollectionMeta;
use rustman_core::format::from_yaml;
use rustman_core::format::write::{collection_to_yaml, request_to_yaml};
use rustman_core::model::KindRegistry;

fn fixtures() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/requests");
    let mut paths: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    paths.sort();
    paths
}

#[test]
fn every_fixture_round_trips_byte_for_byte() {
    let registry = KindRegistry::default();
    let paths = fixtures();
    assert!(paths.len() >= 15, "fixtures missing");
    for path in paths {
        let original = fs::read_to_string(&path).unwrap();
        let request = registry.load(&path, &original).unwrap();
        let saved = request_to_yaml(&request);
        assert_eq!(saved, original, "{} changed on save", path.display());
    }
}

#[test]
fn hand_written_files_normalize_to_a_fixed_point() {
    let text = r#"kind: http
version: 1
url: "{{base_url}}/users"
name: Create user
headers:
  - { name: Authorization, value: "Bearer {{token}}" }
body:
  content: { name: "Ana", role: "admin", tags: [a, b], n: 1.0 }
  type: json
assertions:
  - status: 201
  - json: { path: "$.id", exists: true }
  - latency: { total_lt: 500 }
captures:
  - { var: user_id, from: json, path: "$.id" }
"#;
    let registry = KindRegistry::default();
    let first = request_to_yaml(&registry.load(Path::new("a"), text).unwrap());
    let expected = "\
version: 1
kind: http
name: Create user
url: '{{base_url}}/users'
headers:
  - name: Authorization
    value: Bearer {{token}}
body:
  type: json
  content:
    name: Ana
    role: admin
    tags:
      - a
      - b
    n: 1.0
assertions:
  - status: 201
  - json:
      path: $.id
      exists: true
  - latency:
      total_lt: 500
captures:
  - var: user_id
    from: json
    path: $.id
";
    assert_eq!(first, expected);
    let second = request_to_yaml(&registry.load(Path::new("a"), &first).unwrap());
    assert_eq!(second, first);
}

#[test]
fn collection_metadata_round_trips() {
    let text = "\
name: Users
description: User management
order:
  - login
  - list
headers:
  - name: X-Team
    value: core
auth:
  type: bearer
  token: '{{token}}'
variables:
  page_size: 50
";
    let meta: CollectionMeta = from_yaml(Path::new("_collection.yaml"), text).unwrap();
    assert_eq!(collection_to_yaml(&meta), text);
}
