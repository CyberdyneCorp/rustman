//! Task 1.10: the three-way merge sync. Property tests on the merge
//! (user-only fields always survive) and a sync of a real collection: a
//! user-edited body is kept, a removed operation moves to `_removed/`, new
//! operations and folders are added, environments merge.

mod common;

use std::fs;
use std::path::Path;

use common::load;
use proptest::prelude::*;
use rustman_core::Workspace;
use rustman_core::format::from_yaml;
use rustman_core::model::{HttpRequest, KindRegistry, NameValue, Request, VarValue, Variables};
use rustman_explore::store::{Target, generate_for, write_import};
use rustman_explore::sync::merge::{merge_request, merge_rows};
use rustman_explore::sync::{self, SyncError};

/// A request with fields the import never generates, to mutate.
fn template() -> HttpRequest {
    let text = "\
version: 1
kind: http
name: createPet
tags:
  - openapi:operationId=createPet
method: POST
url: '{{baseUrl}}/pets'
settings:
  timeout_ms: 500
assertions:
  - status: 201
captures:
  - var: id
    from: json
    path: $.id
";
    match KindRegistry::default().load(Path::new("t.rman.yaml"), text) {
        Ok(Request::Http(http)) => *http,
        other => panic!("{other:?}"),
    }
}

fn rows_strategy() -> impl Strategy<Value = Vec<(String, String)>> {
    prop::collection::btree_map("[a-e]", "[xyz]", 0..5).prop_map(|m| m.into_iter().collect())
}

fn request(
    url: &str,
    headers: &[(String, String)],
    variables: Option<&[(String, String)]>,
) -> HttpRequest {
    let mut r = template();
    r.url = url.to_owned();
    r.headers = Some(headers.iter().map(|(n, v)| NameValue::new(n, v)).collect());
    match variables {
        Some(vars) => {
            let entries = vars
                .iter()
                .map(|(n, v)| (n.clone(), VarValue::String(v.clone())))
                .collect();
            r.variables = Some(Variables::new(entries));
        }
        None => {
            r.variables = None;
            r.assertions = None;
            r.captures = None;
            r.settings = None;
        }
    }
    r
}

fn as_set(rows: &Option<Vec<NameValue>>) -> std::collections::BTreeMap<String, String> {
    rows.iter()
        .flatten()
        .map(|r| (r.name.to_ascii_lowercase(), r.value.clone()))
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn user_only_fields_always_survive(
        base_rows in rows_strategy(),
        local_rows in rows_strategy(),
        remote_rows in rows_strategy(),
        urls in prop::sample::subsequence(vec!["u1", "u2", "u3"], 3),
        user_vars in rows_strategy(),
    ) {
        let base = request(urls[0], &base_rows, None);
        let local = request(urls[1], &local_rows, Some(&user_vars));
        let remote = request(urls[2], &remote_rows, None);
        let merged = merge_request(Some(&base), &local, &remote).request;
        // Fields the import never generates are the user's.
        prop_assert_eq!(&merged.variables, &local.variables);
        prop_assert_eq!(&merged.assertions, &local.assertions);
        prop_assert_eq!(&merged.captures, &local.captures);
        prop_assert_eq!(&merged.settings, &local.settings);
        // Rows only the user has keep the user's value.
        let merged_rows = as_set(&merged.headers);
        for (name, value) in &local_rows {
            if !base_rows.iter().any(|(b, _)| b == name) {
                prop_assert_eq!(merged_rows.get(name.as_str()), Some(value));
            }
        }
        // A field the user changed is never overwritten.
        prop_assert_eq!(&merged.url, &local.url);
    }

    #[test]
    fn untouched_files_fast_forward_and_unchanged_documents_keep_edits(
        base_rows in rows_strategy(),
        other_rows in rows_strategy(),
    ) {
        let base = request("u1", &base_rows, None);
        let other = request("u2", &other_rows, None);
        let forward = merge_request(Some(&base), &base, &other).request;
        prop_assert_eq!(&forward.url, &other.url);
        prop_assert_eq!(as_set(&forward.headers), as_set(&other.headers));
        let kept = merge_request(Some(&base), &other, &base).request;
        prop_assert_eq!(kept, other);
    }

    #[test]
    fn keyed_rows_never_drop_user_rows(
        base in rows_strategy(),
        local in rows_strategy(),
        remote in rows_strategy(),
    ) {
        let (merged, _) = merge_rows(&base, &local, &remote, |r| r.0.clone());
        for row in &local {
            let in_base = base.iter().any(|b| b.0 == row.0);
            if !in_base {
                prop_assert!(merged.contains(row), "{row:?} lost");
            }
        }
    }
}

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("rustman.toml"),
        "[workspace]\nname = \"t\"\nversion = 1\n",
    )
    .unwrap();
    dir
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn http(path: &Path) -> HttpRequest {
    match KindRegistry::default().load(path, &read(path)) {
        Ok(Request::Http(h)) => *h,
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn sync_merges_a_new_document_into_an_edited_collection() {
    let ws = workspace();
    let target = Target::new(ws.path(), ws.path().join("shop"));
    let v1 = load("sync/v1.yaml").await;
    write_import(&v1, &generate_for(&v1, &target), &target).unwrap();
    let pets = target.dir.join("pets");

    // The user edits a body, adds a header, an assertion and a variable.
    let create = pets.join("create-pet.rman.yaml");
    let edited = read(&create).replace("name: Rex", "name: Mine");
    fs::write(&create, edited).unwrap();
    let list = pets.join("list-pets.rman.yaml");
    let mut text = read(&list);
    text.push_str("headers:\n  - name: X-Mine\n    value: '1'\nassertions:\n  - status: 200\n");
    fs::write(&list, text).unwrap();
    let staging = ws.path().join("environments/staging.yaml");
    let env = read(&staging).replace("variables:\n", "variables:\n  petId: p1\n");
    fs::write(&staging, env).unwrap();

    let v2 = load("sync/v2.yaml").await;
    let plan = sync::plan(&v2, &target).unwrap();
    let summary = plan.summary.clone();
    let text = summary.to_string();
    assert_eq!(
        text,
        "added: uploadPhoto (pets/upload-photo.rman.yaml)\n\
         added: createOrder (orders/create-order.rman.yaml)\n\
         changed: listPets (headers)\n\
         conflict: createPet body: kept local\n\
         removed: deletePet (moved to _removed/delete-pet.rman.yaml)\n\
         environment staging: updated\n\
         environment production: updated\n\
         2 added, 1 changed, 1 removed, 1 conflict, 1 unchanged\n"
    );
    sync::apply(&plan).unwrap();

    // The user's body, header, assertion and variable survive.
    assert!(read(&create).contains("name: Mine"));
    let list = http(&list);
    let headers: Vec<&str> = list
        .headers
        .iter()
        .flatten()
        .map(|h| h.name.as_str())
        .collect();
    assert_eq!(headers, ["X-Mine", "X-Trace"]);
    assert!(list.assertions.is_some());
    // The removed operation is moved, never deleted.
    assert!(!pets.join("delete-pet.rman.yaml").exists());
    assert!(target.dir.join("_removed/delete-pet.rman.yaml").is_file());
    // New operations and folders, listed in `order`.
    assert!(target.dir.join("orders/create-order.rman.yaml").is_file());
    assert!(target.dir.join("orders/_collection.yaml").is_file());
    let pets_order = read(&pets.join("_collection.yaml"));
    assert!(pets_order.contains("upload-photo") && !pets_order.contains("delete-pet"));
    assert!(read(&target.dir.join("_collection.yaml")).contains("  - orders\n"));
    // Environments: new baseUrl, the user's variable kept.
    let staging = read(&staging);
    assert!(
        staging.contains("baseUrl: https://staging.example.com/v2"),
        "{staging}"
    );
    assert!(staging.contains("petId: p1"), "{staging}");
    // The snapshot and the recorded hash are the new document's.
    assert_eq!(
        read(&target.snapshot_path()),
        rustman_explore::store::snapshot_text(&v2.document)
    );
    let root: rustman_core::collection::CollectionMeta = from_yaml(
        Path::new("_collection.yaml"),
        &read(&target.dir.join("_collection.yaml")),
    )
    .unwrap();
    assert_eq!(
        root.source.unwrap().sha256,
        rustman_explore::store::sha256(&v2.text)
    );
    // The synced collection loads, and syncing again changes nothing.
    let loaded = Workspace::load(ws.path()).unwrap();
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    let again = sync::plan(&v2, &target).unwrap();
    assert!(again.actions.is_empty(), "{:?}", again.actions);
    assert_eq!(again.summary.unchanged, 5);
}

#[tokio::test]
async fn a_collection_without_snapshot_is_not_synced() {
    let ws = workspace();
    let target = Target::new(ws.path(), ws.path().join("shop"));
    fs::create_dir_all(&target.dir).unwrap();
    let v2 = load("sync/v2.yaml").await;
    let err = sync::plan(&v2, &target).unwrap_err();
    assert!(matches!(err, SyncError::NoSnapshot { .. }), "{err}");
}
