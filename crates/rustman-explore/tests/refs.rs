//! Task 1.4: `$ref` resolution. Local and relative file references are
//! resolved, remote ones refused unless allowed, and cycles stay references.

mod common;

use common::{fixture, load, load_err};
use rustman_explore::openapi::schema;
use rustman_explore::{ImportError, ImportOptions, import};
use rustman_test_support::TestServer;
use serde_json::json;

#[tokio::test]
async fn relative_file_references_are_resolved() {
    let imported = load("refs/root.yaml").await;
    let model = &imported.model;
    let get_pet = &model.operations[0];
    assert_eq!(get_pet.parameters[0].name, "petId");
    assert_eq!(
        get_pet.parameters[0].schema,
        Some(json!({"type": "integer"}))
    );
    let pet_ref = get_pet.responses[0].content[0].schema.clone().unwrap();
    assert_eq!(
        pet_ref,
        json!({"$ref": "#/components/x-rustman-external/pet.yaml_Pet"})
    );

    let external = &model.components["x-rustman-external"];
    let pet = &external["pet.yaml_Pet"]["properties"];
    assert_eq!(
        pet["category"],
        json!({"$ref": "#/components/x-rustman-external/pet.yaml_Category"})
    );
    // A file referencing itself stays a reference.
    assert_eq!(
        pet["parent"],
        json!({"$ref": "#/components/x-rustman-external/pet.yaml_Pet"})
    );
    // A file referencing the root's components points back into them.
    assert_eq!(pet["owner"], json!({"$ref": "#/components/schemas/Node"}));
    assert_eq!(external["pet.yaml_Category"]["enum"], json!(["dog", "cat"]));

    let v = schema::validator(&pet_ref, &model.components).unwrap();
    let ok = json!({"name": "Rex", "category": "dog", "parent": {"name": "Ma"}, "owner": {"children": []}});
    assert!(schema::violations(&v, &ok).is_empty());
    assert!(!schema::violations(&v, &json!({"name": "Rex", "category": "cow"})).is_empty());
}

#[tokio::test]
async fn circular_local_references_are_preserved() {
    let model = load("refs/root.yaml").await.model;
    let node = &model.components["schemas"]["Node"];
    assert_eq!(
        node["properties"]["children"]["items"],
        json!({"$ref": "#/components/schemas/Node"})
    );
    let tree = &model.operations[1];
    let schema_ref = tree.responses[0].content[0].schema.clone().unwrap();
    let v = schema::validator(&schema_ref, &model.components).unwrap();
    let deep = json!({"children": [{"children": [{"children": [{"name": "leaf"}]}]}]});
    assert!(schema::violations(&v, &deep).is_empty());
}

#[tokio::test]
async fn path_item_references_are_followed() {
    let model = load("refs/root.yaml").await.model;
    let same: Vec<_> = model
        .operations
        .iter()
        .filter(|op| op.path == "/same")
        .collect();
    assert_eq!(same.len(), 1);
    assert_eq!(same[0].operation_id.as_deref(), Some("getTree"));
}

#[tokio::test]
async fn remote_references_are_refused_by_default() {
    let err = load_err("refs/remote.yaml").await;
    assert!(matches!(err, ImportError::RemoteRef { .. }), "{err:?}");
    assert_eq!(
        err.to_string(),
        "remote reference 'https://schemas.example.com/user.json' at \
         /paths/~1users/get/responses/200/content/application~1json/schema \
         is not resolved; pass --allow-remote-refs to fetch it"
    );
}

#[tokio::test]
async fn remote_references_are_fetched_when_allowed() {
    let server = TestServer::http().await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("api.yaml");
    let text = std::fs::read_to_string(fixture("refs/remote.yaml"))
        .unwrap()
        .replace(
            "https://schemas.example.com/user.json",
            &format!("{}#/Pet", server.url("/openapi/pet.json")),
        );
    std::fs::write(&root, text).unwrap();
    let source = root.to_string_lossy().into_owned();

    let refused = import(&source, &ImportOptions::default())
        .await
        .unwrap_err();
    assert!(
        matches!(refused, ImportError::RemoteRef { .. }),
        "{refused:?}"
    );

    let options = ImportOptions {
        allow_remote_refs: true,
        ..ImportOptions::default()
    };
    let model = import(&source, &options).await.unwrap().model;
    assert_eq!(
        model.components["x-rustman-external"]["pet.json_Pet"]["properties"]["name"],
        json!({"type": "string"})
    );
}

#[tokio::test]
async fn a_missing_referenced_file_is_an_error() {
    let err = load_err("refs/missing.yaml").await;
    assert!(matches!(err, ImportError::Source(_)), "{err:?}");
    assert!(err.to_string().contains("nope.yaml"), "{err}");
}

/// A document in `<tmp>/spec/` whose response schema is `reference`, and a
/// valid schema file `<tmp>/outside.yaml` beside its directory.
async fn import_referencing(reference: impl Fn(&std::path::Path) -> String) -> ImportError {
    let dir = tempfile::tempdir().unwrap();
    let outside = dir.path().join("outside.yaml");
    std::fs::write(&outside, "S: {type: string}\n").unwrap();
    let spec = dir.path().join("spec");
    std::fs::create_dir(&spec).unwrap();
    let document = format!(
        "openapi: 3.1.0\ninfo: {{title: t, version: '1'}}\npaths:\n  /x:\n    get:\n      \
         responses:\n        '200':\n          description: ok\n          content:\n            \
         application/json:\n              schema: {{$ref: '{}'}}\n",
        reference(&outside)
    );
    let root = spec.join("api.yaml");
    std::fs::write(&root, document).unwrap();
    match import(&root.to_string_lossy(), &ImportOptions::default()).await {
        Ok(_) => panic!("the reference was followed"),
        Err(err) => err,
    }
}

#[tokio::test]
async fn file_references_cannot_leave_the_document_directory() {
    let climbing = import_referencing(|_| "../outside.yaml#/S".into()).await;
    let absolute = import_referencing(|outside| format!("{}#/S", outside.display())).await;
    for err in [climbing, absolute] {
        assert!(matches!(err, ImportError::Reference(_)), "{err:?}");
        assert!(
            err.to_string().contains("leaves the document's directory"),
            "{err}"
        );
    }
}
