//! Task 1.6: the generated collection: tag folders, kebab-case file names
//! with collision suffixes, and parameters as variables.

mod common;

use std::path::Path;

use common::load;
use rustman_core::Workspace;
use rustman_core::format::write::environment_to_yaml;
use rustman_core::model::Request;
use rustman_explore::collection::{Generated, WriteError, collection_dir, generate, write};

/// The tree as one text: each file's path, then its contents, then the
/// environments.
fn tree(generated: &Generated) -> String {
    let files = generated
        .files
        .iter()
        .map(|f| format!("=== {}\n{}", f.path, f.contents));
    let environments = generated.environments.iter().map(|e| {
        format!(
            "=== environments/{}.yaml\n{}",
            e.name,
            environment_to_yaml(e)
        )
    });
    files.chain(environments).collect()
}

#[tokio::test]
async fn petstore_collection_tree() {
    let model = load("petstore-3.0.yaml").await.model;
    let generated = generate(&model);
    assert_eq!(collection_dir(&model), "swagger-petstore");
    assert_eq!(generated.requests, 5);
    assert!(generated.warnings.is_empty(), "{:?}", generated.warnings);
    insta::assert_snapshot!("petstore-tree", tree(&generated));
}

#[tokio::test]
async fn collisions_get_suffixes_and_missing_ids_use_method_and_path() {
    let model = load("warnings.yaml").await.model;
    let generated = generate(&model);
    let paths: Vec<&str> = generated.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "_collection.yaml",
            "untagged/_collection.yaml",
            "untagged/get-users.rman.yaml",
            "untagged/create-user.rman.yaml",
            "untagged/create-user-2.rman.yaml",
            "untagged/create-user-3.rman.yaml",
        ]
    );
    let messages: Vec<String> = generated.warnings.iter().map(ToString::to_string).collect();
    assert_eq!(
        messages,
        [
            "/paths/~1users~1{id}/get: the file name 'create-user' is already used in 'untagged'; \
             this operation is written to 'create-user-2.rman.yaml'",
            "/paths/~1users~1{id}/put: the file name 'create-user' is already used in 'untagged'; \
             this operation is written to 'create-user-3.rman.yaml'",
        ]
    );
}

#[tokio::test]
async fn generated_files_load_as_a_workspace() {
    let model = load("petstore-3.1.json").await.model;
    let generated = generate(&model);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("rustman.toml"),
        "[workspace]\nname = \"t\"\nversion = 1\n",
    )
    .unwrap();
    let target = dir.path().join(collection_dir(&model));
    write(&generated, &target).unwrap();
    assert!(target.join("pets/list-pets.rman.yaml").is_file());
    assert!(target.join("pets/create-pet.rman.yaml").is_file());

    let ws = Workspace::load(dir.path()).unwrap();
    let show = ws.find_by_name("showPetById");
    let Request::Http(http) = show[0].request() else {
        panic!("not http");
    };
    assert_eq!(http.url, "{{baseUrl}}/pets/{{petId}}");
    let names: Vec<String> = ws.requests().iter().map(|r| r.entry.stem()).collect();
    assert_eq!(
        names,
        [
            "list-pets",
            "create-pet",
            "show-pet-by-id",
            "delete-pet",
            "get-inventory"
        ]
    );
}

#[tokio::test]
async fn a_non_empty_target_is_refused_and_left_alone() {
    let model = load("petstore-3.0.yaml").await.model;
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("api");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("mine.txt"), "keep").unwrap();
    let err = write(&generate(&model), &target).unwrap_err();
    assert!(matches!(err, WriteError::Exists(_)), "{err:?}");
    let entries: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
    assert_eq!(entries.len(), 1, "no staging folder is left behind");
    assert_eq!(
        std::fs::read_to_string(target.join("mine.txt")).unwrap(),
        "keep"
    );
}

#[tokio::test]
async fn an_empty_target_is_used() {
    let model = load("petstore-3.0.yaml").await.model;
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("api");
    std::fs::create_dir(&target).unwrap();
    write(&generate(&model), &target).unwrap();
    assert!(Path::new(&target.join("_collection.yaml")).is_file());
}

/// Issue #95: `{{secret:...}}` text in the document (a parameter name, an
/// example, a server URL, a security scheme) is written escaped, with a
/// warning, so no generated file resolves a local secret.
#[tokio::test]
async fn secret_references_from_the_document_are_neutralized() {
    const REF: &str = "{{secret:vault/kv/prod/db#password}}";
    let spec = format!(
        r#"openapi: 3.0.3
info: {{title: Evil, version: "1"}}
servers:
  - url: "https://evil.test/{REF}"
paths:
  /leak:
    post:
      operationId: leak
      parameters:
        - {{name: "{REF}", in: query, required: true, schema: {{type: string}}}}
      requestBody:
        content:
          application/json:
            example: {{pw: "{REF}", "{REF}": 1}}
      responses:
        "200": {{description: ok}}
security:
  - key: []
components:
  securitySchemes:
    key: {{type: apiKey, in: header, name: "{REF}"}}
"#
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("evil.yaml");
    std::fs::write(&path, spec).unwrap();
    let imported = rustman_explore::import(
        &path.to_string_lossy(),
        &rustman_explore::ImportOptions::default(),
    )
    .await
    .unwrap();
    let generated = generate(&imported.model);
    let all = tree(&generated);
    assert!(
        all.contains("\\{{secret:vault/kv/prod/db#password}}"),
        "{all}"
    );
    for (at, _) in all.match_indices("{{secret:") {
        assert!(all[..at].ends_with('\\'), "{all}");
    }
    let warnings: Vec<String> = generated.warnings.iter().map(ToString::to_string).collect();
    let neutralized: Vec<&String> = warnings
        .iter()
        .filter(|w| w.contains(REF) && w.contains("neutralized"))
        .collect();
    // The operation, the server and the collection auth.
    for pointer in ["/paths/~1leak/post", "/servers/0", "/security"] {
        assert!(
            neutralized.iter().any(|w| w.starts_with(pointer)),
            "{pointer}: {warnings:?}"
        );
    }
}
