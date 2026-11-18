//! Task 1.5: validation diagnostics with JSON pointers. Errors abort the
//! import; warnings are returned with the model.

mod common;

use common::{load, load_err};
use rustman_explore::ImportError;
use rustman_explore::openapi::diagnostic::render;

#[tokio::test]
async fn structural_errors_abort_with_pointers() {
    let err = load_err("invalid.yaml").await;
    let ImportError::Invalid(diagnostics) = &err else {
        panic!("{err:?}");
    };
    insta::assert_snapshot!("invalid", render(diagnostics));
}

#[tokio::test]
async fn warnings_do_not_stop_the_import() {
    let imported = load("warnings.yaml").await;
    insta::assert_snapshot!("warnings", render(&imported.warnings));
    let missing = imported
        .warnings
        .iter()
        .find(|w| w.message.contains("no operationId"))
        .unwrap();
    assert_eq!(missing.pointer, "/paths/~1users/get");
}

#[tokio::test]
async fn swagger_2_is_rejected() {
    let err = load_err("swagger-2.0.json").await;
    assert_eq!(
        err.to_string(),
        "Swagger 2.0 is not supported; convert it to OpenAPI 3 first (e.g. with swagger2openapi)"
    );
}
