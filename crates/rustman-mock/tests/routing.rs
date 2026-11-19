//! Task 2.2 and 2.3: routes from the document's paths, literal segments
//! over parameters, base-path stripping, 404 and 405 with `Allow`, and the
//! problem+json bodies of those errors.

mod common;

use common::{get, header, json, request, start, status};

#[tokio::test]
async fn static_beats_templated() {
    let mock = start().await;
    let me = get(mock.local_addr(), "/users/me", &[]).await;
    assert_eq!(
        (status(&me), json(&me)),
        (200, serde_json::json!({"id": "me"}))
    );
    let other = get(mock.local_addr(), "/users/42", &[]).await;
    assert_eq!(json(&other), serde_json::json!({"id": "someone"}));
    mock.stop().await;
}

#[tokio::test]
async fn method_not_allowed_lists_the_declared_methods() {
    let mock = start().await;
    let response = request(mock.local_addr(), "DELETE", "/users", &[]).await;
    assert_eq!(status(&response), 405, "{response}");
    assert_eq!(header(&response, "allow"), Some("GET, POST"));
    assert_eq!(
        header(&response, "content-type"),
        Some("application/problem+json")
    );
    assert_eq!(json(&response)["status"], 405);
    mock.stop().await;
}

#[tokio::test]
async fn unknown_route_body() {
    let mock = start().await;
    let response = get(mock.local_addr(), "/nope", &[]).await;
    assert_eq!(status(&response), 404);
    assert_eq!(
        header(&response, "content-type"),
        Some("application/problem+json")
    );
    let problem = json(&response);
    insta::assert_snapshot!(serde_json::to_string_pretty(&problem).unwrap(), @r#"
    {
      "type": "urn:rustman:mock:no-matching-operation",
      "title": "No matching operation",
      "status": 404,
      "detail": "GET /nope matches no operation of the document"
    }
    "#);
    mock.stop().await;
}

#[tokio::test]
async fn the_server_base_path_is_stripped() {
    let mock = start().await;
    let response = get(mock.local_addr(), "/api/pets/1", &[]).await;
    assert_eq!(json(&response)["name"], "Tom");
    let unprefixed = get(mock.local_addr(), "/pets/1", &[]).await;
    assert_eq!(status(&unprefixed), 200);
    mock.stop().await;
}
