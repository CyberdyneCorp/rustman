//! Deterministic emitter (task 2.6): golden-file round trips.

use std::fs;
use std::path::{Path, PathBuf};

use rustman_core::collection::CollectionMeta;
use rustman_core::format::from_yaml;
use rustman_core::format::write::{collection_to_yaml, environment_to_yaml, request_to_yaml};
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

#[test]
fn collection_source_and_parallel_round_trip() {
    let text = "\
name: Shop
parallel: true
source:
  format: openapi
  location: specs/shop.yaml
  sha256: '9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08'
";
    let meta: CollectionMeta = from_yaml(Path::new("_collection.yaml"), text).unwrap();
    assert_eq!(meta.source.as_ref().unwrap().format, "openapi");
    assert_eq!(collection_to_yaml(&meta), text);
}

#[test]
fn collection_data_file_and_secret_columns_round_trip() {
    let text = "\
name: Signup
data: data/users.csv
secret_columns:
  - password
";
    let meta: CollectionMeta = from_yaml(Path::new("_collection.yaml"), text).unwrap();
    assert_eq!(meta.data.as_deref(), Some("data/users.csv"));
    assert_eq!(collection_to_yaml(&meta), text);
}

#[test]
fn environments_round_trip() {
    let text = "\
variables:
  baseUrl: https://api.example.com/v1
  version: v1
secrets:
  - bearerAuth_token
production: true
";
    let file = Path::new("environments/production.yaml");
    let env = rustman_core::environment::parse(file, "production", text).unwrap();
    assert_eq!(environment_to_yaml(&env), text);
    let empty = rustman_core::environment::parse(file, "e", "").unwrap();
    assert_eq!(environment_to_yaml(&empty), "{}\n");
}
