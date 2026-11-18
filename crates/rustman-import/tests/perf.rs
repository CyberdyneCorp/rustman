//! Secret extraction stays linear in the size of the source: large bodies
//! and many secret-named keys plan in seconds, not minutes.

mod common;

use std::time::{Duration, Instant};

use common::Setup;
use rustman_import::Format;

/// A Postman collection with one request whose raw JSON body is `body`.
fn collection(body: &str) -> String {
    serde_json::json!({
        "info": {"name": "big", "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"},
        "item": [{"name": "r", "request": {"method": "POST", "url": "https://a", "body": {"mode": "raw", "raw": body}}}]
    })
    .to_string()
}

/// Plan `text`, returning how long it took.
fn plan_time(text: &str) -> (Duration, rustman_import::Plan) {
    let dir = tempfile::tempdir().unwrap();
    let start = Instant::now();
    let parsed = Format::Postman.parse(text, "big.json").unwrap();
    let plan = Setup::new(dir.path()).plan_collection(Format::Postman, parsed, "big.json");
    (start.elapsed(), plan)
}

// Generous bounds: debug builds on a slow CI runner. The quadratic
// versions took minutes on these inputs.
const BOUND: Duration = Duration::from_secs(60);

#[test]
fn many_secret_named_keys_plan_in_linear_time() {
    let pairs: Vec<String> = (0..20_000)
        .map(|i| format!("\"password{i}\":\"value-{i}\""))
        .collect();
    let (took, plan) = plan_time(&collection(&format!("{{{}}}", pairs.join(","))));
    assert!(took < BOUND, "{took:?}");
    assert_eq!(plan.secrets.len(), 20_000);
}

#[test]
fn one_secret_name_repeated_many_times_plans_in_linear_time() {
    let pairs: Vec<String> = (0..20_000)
        .map(|i| format!("\"password\":\"value-{i}\""))
        .collect();
    let (took, plan) = plan_time(&collection(&format!("{{{}}}", pairs.join(","))));
    assert!(took < BOUND, "{took:?}");
    assert_eq!(plan.secrets.len(), 20_000);
    assert!(
        plan.secrets
            .iter()
            .any(|m| m.secret.name() == "password_20000")
    );
}

#[test]
fn a_large_body_with_runs_of_braces_plans_in_linear_time() {
    let body = format!(
        "{{\"password\":\"{}{}\",{}}}",
        "a".repeat(2_000_000),
        "{".repeat(400_000),
        (0..2_000)
            .map(|i| format!("\"k{i}\":\"v\""))
            .collect::<Vec<_>>()
            .join(",")
    );
    let (took, plan) = plan_time(&collection(&body));
    assert!(took < BOUND, "{took:?}");
    assert_eq!(plan.secrets.len(), 1);
}

#[test]
fn storing_five_thousand_secrets_in_the_file_store_is_fast() {
    use rustman_core::{SecretStore, secrets::SecretBackend};
    let pairs: Vec<String> = (0..5_000)
        .map(|i| format!("\"password{i}\":\"value-{i}\""))
        .collect();
    let (_, plan) = plan_time(&collection(&format!("{{{}}}", pairs.join(","))));
    assert_eq!(plan.secrets.len(), 5_000);
    let dir = tempfile::tempdir().unwrap();
    let store = SecretStore::open(dir.path(), SecretBackend::File);
    let save = |batch: &[(&str, &rustman_core::Secret<String>)]| store.set_many(batch);
    let start = Instant::now();
    rustman_import::apply(&plan, dir.path(), &save).unwrap();
    let took = start.elapsed();
    assert!(took < Duration::from_secs(10), "{took:?}");
    let stored = store.get("local", "password4999").unwrap().unwrap();
    assert_eq!(stored.expose(), "value-4999");
}
