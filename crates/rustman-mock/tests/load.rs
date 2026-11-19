//! Task 2.1: the document is read with the `openapi-import` parser; a
//! structural error names the JSON pointer of the problem.

mod common;

use rustman_explore::ImportError;
use rustman_mock::load;

#[tokio::test]
async fn a_dangling_reference_names_its_pointer() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("broken.yaml");
    let text = "openapi: 3.1.0\ninfo: {title: t, version: '1'}\npaths:\n  /a:\n    get:\n      responses:\n        '200':\n          description: ok\n          content:\n            application/json:\n              schema: {$ref: '#/components/schemas/Nope'}\n";
    std::fs::write(&path, text).unwrap();
    let err = load(&path.to_string_lossy(), false).await.unwrap_err();
    assert!(matches!(err, ImportError::Invalid(_)), "{err:?}");
    let message = err.to_string();
    assert!(
        message.contains("/paths/~1a/get/responses/200/content/application~1json/schema/$ref"),
        "{message}"
    );
    assert!(message.contains("#/components/schemas/Nope"), "{message}");
}

#[tokio::test]
async fn the_operations_are_counted() {
    let state = load(&common::fixture("pets.yaml"), false).await.unwrap();
    assert_eq!(state.operations(), 8);
}
