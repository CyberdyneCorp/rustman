//! Tasks 5.1 and 5.2: the stateful store, reset, the reserved prefix and
//! the per-resource cap.

mod common;

use common::{config, get, json, post_json, request, send, start_fixture, status};
use rustman_mock::MockConfig;
use serde_json::Value;

fn stateful() -> MockConfig {
    MockConfig {
        stateful: true,
        ..config()
    }
}

async fn create(addr: std::net::SocketAddr, name: &str) -> String {
    let body = format!(r#"{{"name":"{name}"}}"#);
    let response = post_json(addr, "POST", "/users", &body).await;
    assert_eq!(status(&response), 201, "{response}");
    let created = json(&response);
    assert_eq!(created["name"], name);
    created["id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn create_then_read() {
    let mock = start_fixture("store.yaml", None, stateful()).await;
    let addr = mock.local_addr();
    let id = create(addr, "Ana").await;
    let read = get(addr, &format!("/users/{id}"), &[]).await;
    assert_eq!(status(&read), 200, "{read}");
    let user = json(&read);
    assert_eq!(user["name"], "Ana");
    assert_eq!(user["id"], Value::String(id.clone()));
    let list = json(&get(addr, "/users", &[]).await);
    assert_eq!(list.as_array().unwrap().len(), 1);
    let patched = post_json(addr, "PATCH", &format!("/users/{id}"), r#"{"name":"Bia"}"#).await;
    assert_eq!(json(&patched)["name"], "Bia");
    assert_eq!(json(&patched)["id"], Value::String(id));
    mock.stop().await;
}

#[tokio::test]
async fn read_after_delete() {
    let mock = start_fixture("store.yaml", None, stateful()).await;
    let addr = mock.local_addr();
    let id = create(addr, "Ana").await;
    let path = format!("/users/{id}");
    let deleted = request(addr, "DELETE", &path, &[]).await;
    assert_eq!(status(&deleted), 204, "{deleted}");
    let read = get(addr, &path, &[]).await;
    assert_eq!(status(&read), 404, "{read}");
    assert_eq!(json(&read)["title"], "Unknown id");
    assert_eq!(status(&request(addr, "DELETE", &path, &[]).await), 404);
    mock.stop().await;
}

#[tokio::test]
async fn unknown_ids_are_404_only_in_stateful_mode() {
    let unknown = "/users/123e4567-e89b-42d3-a456-426614174000";
    let mock = start_fixture("store.yaml", None, stateful()).await;
    assert_eq!(status(&get(mock.local_addr(), unknown, &[]).await), 404);
    mock.stop().await;
    let plain = start_fixture("store.yaml", None, config()).await;
    assert_eq!(status(&get(plain.local_addr(), unknown, &[]).await), 200);
    plain.stop().await;
}

#[tokio::test]
async fn reset() {
    let mock = start_fixture("store.yaml", None, stateful()).await;
    let addr = mock.local_addr();
    let mut ids = Vec::new();
    for i in 0..5 {
        ids.push(create(addr, &format!("user{i}")).await);
    }
    let reset = send(addr, "POST", "/__rustman/reset", &[], "").await;
    assert_eq!(status(&reset), 204, "{reset}");
    for id in ids {
        assert_eq!(status(&get(addr, &format!("/users/{id}"), &[]).await), 404);
    }
    mock.stop().await;
}

#[tokio::test]
async fn reserved_prefix_is_never_routed() {
    let mock = start_fixture("store.yaml", None, stateful()).await;
    let response = get(mock.local_addr(), "/__rustman/users", &[]).await;
    assert_eq!(status(&response), 404, "{response}");
    assert_eq!(json(&response)["title"], "Reserved path");
    let wrong_method = get(mock.local_addr(), "/__rustman/reset", &[]).await;
    assert_eq!(status(&wrong_method), 404, "{wrong_method}");
    mock.stop().await;
}

#[tokio::test]
async fn per_resource_cap() {
    let mock = start_fixture(
        "store.yaml",
        None,
        MockConfig {
            max_items: 2,
            ..stateful()
        },
    )
    .await;
    let addr = mock.local_addr();
    create(addr, "a").await;
    create(addr, "b").await;
    let full = post_json(addr, "POST", "/users", r#"{"name":"c"}"#).await;
    assert_eq!(status(&full), 507, "{full}");
    assert_eq!(json(&full)["status"], 507);
    mock.stop().await;
}

#[tokio::test]
async fn the_byte_budget_refuses_large_writes() {
    let config = MockConfig {
        max_store_bytes: 200,
        ..stateful()
    };
    let mock = start_fixture("store.yaml", None, config).await;
    let addr = mock.local_addr();
    let id = create(addr, "Ana").await;
    let big = format!(r#"{{"name":"{}"}}"#, "x".repeat(300));
    let full = post_json(addr, "POST", "/users", &big).await;
    assert_eq!(status(&full), 507, "{full}");
    assert!(
        json(&full)["detail"]
            .as_str()
            .unwrap()
            .contains("200 bytes")
    );
    let path = format!("/users/{id}");
    let replaced = post_json(addr, "PATCH", &path, &big).await;
    assert_eq!(status(&replaced), 507, "{replaced}");
    assert_eq!(json(&get(addr, &path, &[]).await)["name"], "Ana");
    mock.stop().await;
}

#[tokio::test]
async fn the_store_answers_before_prefer() {
    let mock = start_fixture("store.yaml", None, stateful()).await;
    let addr = mock.local_addr();
    let id = create(addr, "Ana").await;
    let read = get(addr, &format!("/users/{id}"), &["Prefer: dynamic=true"]).await;
    assert_eq!(json(&read)["name"], "Ana", "{read}");
    mock.stop().await;
}

#[tokio::test]
async fn the_default_cap_is_ten_thousand() {
    assert_eq!(MockConfig::default().max_items, 10_000);
}
