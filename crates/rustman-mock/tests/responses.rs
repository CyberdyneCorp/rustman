//! Tasks 2.4 and 2.5: response selection (examples, `Prefer`, content
//! negotiation) and seeded synthesis that validates against the schema.

mod common;

use std::sync::Arc;

use common::{config, fixture, get, header, json, start, start_with, status};
use rustman_explore::openapi::schema;
use rustman_mock::{MockConfig, MockServer, load};
use serde_json::{Value, json};

#[tokio::test]
async fn default_success_response() {
    let mock = start().await;
    let response = get(mock.local_addr(), "/pets/1", &[]).await;
    assert_eq!(status(&response), 200, "{response}");
    assert_eq!(header(&response, "content-type"), Some("application/json"));
    assert_eq!(
        json(&response),
        json!({"id": 1, "name": "Tom", "kind": "cat"})
    );
    mock.stop().await;
}

#[tokio::test]
async fn ask_for_a_404_example() {
    let mock = start().await;
    let prefer = "Prefer: code=404, example=notFound";
    let response = get(mock.local_addr(), "/pets/1", &[prefer]).await;
    assert_eq!(status(&response), 404, "{response}");
    assert_eq!(json(&response), json!({"error": "no such pet"}));
    mock.stop().await;
}

#[tokio::test]
async fn undocumented_code() {
    let mock = start().await;
    let response = get(mock.local_addr(), "/pets/1", &["Prefer: code=418"]).await;
    assert_eq!(status(&response), 400, "{response}");
    assert_eq!(
        header(&response, "content-type"),
        Some("application/problem+json")
    );
    let detail = json(&response)["detail"].as_str().unwrap().to_owned();
    assert!(detail.ends_with("documented: 200, 404"), "{detail}");
    mock.stop().await;
}

#[tokio::test]
async fn no_prefer_ignores_the_header() {
    let mock = start_with(MockConfig {
        prefer: false,
        ..config()
    })
    .await;
    let response = get(mock.local_addr(), "/pets/1", &["Prefer: code=404"]).await;
    assert_eq!(status(&response), 200, "{response}");
    mock.stop().await;
}

#[tokio::test]
async fn xml_requested_but_not_documented() {
    let mock = start().await;
    let response = get(mock.local_addr(), "/pets/1", &["Accept: application/xml"]).await;
    assert_eq!(status(&response), 406, "{response}");
    assert_eq!(
        header(&response, "content-type"),
        Some("application/problem+json")
    );
    mock.stop().await;
}

async fn things(seed: u64) -> Value {
    let mock = start_with(MockConfig { seed, ..config() }).await;
    let response = get(mock.local_addr(), "/things", &[]).await;
    assert_eq!(status(&response), 200, "{response}");
    mock.stop().await;
    json(&response)
}

fn assert_valid(value: &Value, model: &rustman_explore::ApiModel) {
    let thing = json!({"$ref": "#/components/schemas/Thing"});
    let validator = schema::validator(&thing, &model.components).unwrap();
    let found = schema::violations(&validator, value);
    assert!(found.is_empty(), "{value}: {found:?}");
}

#[tokio::test]
async fn stable_synthesized_data() {
    let model = load(&fixture("pets.yaml"), false).await.unwrap().model;
    let mock = start().await;
    let first = json(&get(mock.local_addr(), "/things", &[]).await);
    let second = json(&get(mock.local_addr(), "/things", &[]).await);
    mock.stop().await;
    assert_eq!(first, second);
    assert_valid(&first, &model);
}

#[tokio::test]
async fn a_different_seed_gives_a_different_valid_body() {
    let model = load(&fixture("pets.yaml"), false).await.unwrap().model;
    let zero = things(0).await;
    let seven = things(7).await;
    assert_ne!(zero, seven);
    assert_eq!(zero, things(0).await);
    assert_valid(&zero, &model);
    assert_valid(&seven, &model);
}

#[tokio::test]
async fn h2c_is_served_on_the_same_port() {
    use http_body_util::{BodyExt, Empty};
    use hyper_util::rt::{TokioExecutor, TokioIo};

    let state = Arc::new(load(&fixture("pets.yaml"), false).await.unwrap());
    let mock = MockServer::bind(&config(), state).await.unwrap().spawn();
    let tcp = tokio::net::TcpStream::connect(mock.local_addr())
        .await
        .unwrap();
    let (mut sender, conn) =
        hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(tcp))
            .await
            .unwrap();
    tokio::spawn(conn);
    let request = http::Request::get(format!("http://{}/users/me", mock.local_addr()))
        .body(Empty::<bytes::Bytes>::new())
        .unwrap();
    let response = sender.send_request(request).await.unwrap();
    assert_eq!(response.status(), 200);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(body.as_ref(), br#"{"id":"me"}"#);
    drop(sender);
    mock.stop().await;
}
