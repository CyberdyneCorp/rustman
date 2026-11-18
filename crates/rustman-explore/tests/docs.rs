//! docs/import-openapi.md quotes real messages and generated files.

mod common;

use common::{load, load_err};
use rustman_explore::collection::generate;
use rustman_explore::openapi::error::SWAGGER_2_MESSAGE;

fn doc() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/import-openapi.md");
    std::fs::read_to_string(path).unwrap()
}

#[tokio::test]
async fn documented_messages_are_the_real_ones() {
    let doc = doc();
    assert!(doc.contains(SWAGGER_2_MESSAGE));
    let remote = load_err("refs/remote.yaml").await;
    assert!(doc.contains(&format!("error: {remote}")), "{remote}");
    let warnings = load("warnings.yaml").await.warnings;
    assert!(
        doc.contains(&format!("warning: {}", warnings[0])),
        "{}",
        warnings[0]
    );
}

#[tokio::test]
async fn the_documented_request_file_is_generated() {
    let generated = generate(&load("petstore-3.0.yaml").await.model);
    let show = generated
        .files
        .iter()
        .find(|f| f.path == "pets/show-pet-by-id.rman.yaml")
        .unwrap();
    assert!(
        doc().contains(&format!("```yaml\n{}```", show.contents)),
        "{}",
        show.contents
    );
}
