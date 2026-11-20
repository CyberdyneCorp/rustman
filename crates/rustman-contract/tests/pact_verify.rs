//! Pact provider verification (tasks 6.1, 6.2; spec "Provider
//! verification", "Provider states", "Verification passes the safety
//! policy") against a local fixture provider with a state-change endpoint.
//! Every request goes through `GovernedSenders`.

use rustman_contract::pact::model::{Pact, PactVersion};
use rustman_contract::pact::read;
use rustman_contract::pact::verify::{Outcome, STATES_URL_MISSING, Verification, Verifier};
use rustman_contract::sender::{GovernedSenders, Targets};
use rustman_core::safety::{Permission, SafetySettings};
use rustman_test_support::{GraphqlHttpServer, RecordedRequest, Reply};
use serde_json::{Value, json};

/// The fixture provider: users 42 (and 7, which answers its id as the string "42"), a create
/// endpoint, and a state endpoint that fails for the state `broken`.
async fn provider() -> GraphqlHttpServer {
    GraphqlHttpServer::start(|req: &RecordedRequest| {
        let json = "application/json";
        match (req.method.as_str(), req.uri.as_str()) {
            ("GET", "/users/42") => Reply::raw(200, json, r#"{"id":42,"name":"Ana"}"#),
            ("GET", "/users/7") => Reply::raw(200, json, r#"{"id":"42"}"#),
            ("POST", "/users") | ("DELETE", "/users/42") => Reply::raw(201, json, r#"{"id":1}"#),
            ("POST", "/_pact/state") if req.body_text().contains("broken") => {
                Reply::raw(500, json, "{}")
            }
            ("POST", "/_pact/state") => Reply::raw(200, json, "{}"),
            _ => Reply::raw(404, json, "{}"),
        }
    })
    .await
}

fn interaction(description: &str, method: &str, path: &str, states: &[&str], body: Value) -> Value {
    let states: Vec<Value> = states
        .iter()
        .map(|s| json!({"name": s, "params": {"id": 42}}))
        .collect();
    json!({
        "type": "Synchronous/HTTP",
        "description": description,
        "providerStates": states,
        "request": {"method": method, "path": path},
        "response": {"status": 200, "body": {"content": body, "contentType": "application/json", "encoded": false}},
    })
}

fn pact(interactions: Vec<Value>) -> Pact {
    let text = json!({
        "consumer": {"name": "web"},
        "provider": {"name": "users"},
        "interactions": interactions,
        "metadata": {"pactSpecification": {"version": "4.0"}},
    })
    .to_string();
    read("web-users.json", &text).unwrap().pact
}

async fn verify(
    server: &GraphqlHttpServer,
    pact: Pact,
    states: bool,
    permission: Permission,
) -> Result<Verification, String> {
    let provider_url = server.url("");
    let states_url = server.url("/_pact/state");
    let states_url = states.then_some(states_url.as_str());
    let targets = Targets {
        provider_url: &provider_url,
        states_url,
        permission,
        production: None,
        environment: Default::default(),
    };
    let sender = GovernedSenders::new(&SafetySettings::default(), targets).unwrap();
    let verifier = Verifier {
        sender: &sender,
        provider_url: &provider_url,
        states_url,
    };
    verifier
        .verify(&[("web-users.json".to_owned(), pact)])
        .await
}

fn read_only() -> Permission {
    Permission::from_flags(false, false)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_body_mismatch_names_the_json_path() {
    let server = provider().await;
    let pact = pact(vec![interaction(
        "get user 7",
        "GET",
        "/users/7",
        &[],
        json!({"id": 42}),
    )]);
    let result = verify(&server, pact, false, read_only()).await.unwrap();
    assert_eq!(
        result.results[0].outcome,
        Outcome::Failed(vec![
            r#"$.id: expected number 42, got string "42""#.to_owned()
        ])
    );
    assert_eq!(result.summary(), "0 passed, 1 failed, 0 blocked");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_interaction_passes() {
    let server = provider().await;
    let pact = pact(vec![
        interaction("get user", "GET", "/users/42", &[], json!({"id": 42})),
        interaction(
            "get user name",
            "GET",
            "/users/42",
            &[],
            json!({"name": "Ana"}),
        ),
    ]);
    let result = verify(&server, pact, false, read_only()).await.unwrap();
    assert!(
        result.results.iter().all(|r| r.outcome == Outcome::Passed),
        "{result:?}"
    );
    assert_eq!(result.summary(), "2 passed, 0 failed, 0 blocked");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn states_are_set_up_before_and_torn_down_after() {
    let server = provider().await;
    let pact = pact(vec![interaction(
        "get user",
        "GET",
        "/users/42",
        &["user 42 exists"],
        json!({"id": 42}),
    )]);
    let result = verify(&server, pact, true, read_only()).await.unwrap();
    assert_eq!(result.results[0].outcome, Outcome::Passed);
    let hits = server.hits();
    let order: Vec<(String, String)> = hits
        .iter()
        .map(|h| (h.method.clone(), h.uri.clone()))
        .collect();
    assert_eq!(
        order,
        vec![
            ("POST".into(), "/_pact/state".into()),
            ("GET".into(), "/users/42".into()),
            ("POST".into(), "/_pact/state".into()),
        ]
    );
    let expected = |action: &str| json!({"consumer": "web", "state": "user 42 exists", "params": {"id": 42}, "action": action});
    assert_eq!(hits[0].json(), expected("setup"));
    assert_eq!(hits[2].json(), expected("teardown"));
}

/// Regression: a path such as `@host:port/x` was appended to the provider
/// URL as is, which moves the request to `host:port` (here the state-change
/// origin, which the interaction policy allows).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_path_without_a_leading_slash_is_refused_before_anything_is_sent() {
    let server = provider().await;
    let states_origin = server
        .url("")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_owned();
    let path = format!("@{states_origin}/x");
    let pact = pact(vec![
        interaction("hop", "GET", &path, &["user 42 exists"], json!({})),
        interaction("get user", "GET", "/users/42", &[], json!({"id": 42})),
    ]);
    let result = verify(&server, pact, true, read_only()).await.unwrap();
    assert_eq!(
        result.results[0].outcome,
        Outcome::Failed(vec![format!(
            "request path '{path}' does not start with '/'; the interaction is not sent"
        )])
    );
    assert_eq!(result.results[1].outcome, Outcome::Passed);
    let uris: Vec<String> = server.hits().iter().map(|h| h.uri.clone()).collect();
    assert_eq!(
        uris,
        ["/users/42"],
        "no state change and no request for the refused path"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_setup_fails_the_interaction() {
    let server = provider().await;
    let pact = pact(vec![interaction(
        "get user",
        "GET",
        "/users/42",
        &["broken"],
        json!({"id": 42}),
    )]);
    let result = verify(&server, pact, true, read_only()).await.unwrap();
    assert_eq!(
        result.results[0].outcome,
        Outcome::Failed(vec![
            "provider state 'broken': setup returned 500".to_owned()
        ])
    );
    assert_eq!(
        server.hits().len(),
        1,
        "neither the interaction nor a teardown"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stateful_interactions_need_a_state_url_before_anything_is_sent() {
    let server = provider().await;
    let pact = pact(vec![interaction(
        "get user",
        "GET",
        "/users/42",
        &["user 42 exists"],
        json!({}),
    )]);
    let error = verify(&server, pact, false, read_only()).await.unwrap_err();
    assert_eq!(error, STATES_URL_MISSING);
    assert_eq!(
        STATES_URL_MISSING,
        "provider states declared but --provider-states-url not set"
    );
    assert!(server.hits().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mutations_are_blocked_without_the_flags() {
    let server = provider().await;
    let interactions = || {
        vec![
            interaction("create user", "POST", "/users", &[], json!({"id": 1})),
            interaction("delete user", "DELETE", "/users/42", &[], json!({"id": 1})),
        ]
    };
    let result = verify(&server, pact(interactions()), false, read_only())
        .await
        .unwrap();
    let outcomes: Vec<&Outcome> = result.results.iter().map(|r| &r.outcome).collect();
    assert_eq!(
        outcomes,
        vec![
            &Outcome::Blocked("blocked: mutations not allowed".into()),
            &Outcome::Blocked("blocked: destructive not allowed".into()),
        ]
    );
    assert!(server.hits().is_empty(), "a blocked request is never sent");
    let mutations = Permission::from_flags(true, false);
    let result = verify(&server, pact(interactions()), false, mutations)
        .await
        .unwrap();
    assert_eq!(result.summary(), "0 passed, 1 failed, 1 blocked");
}

#[test]
fn generated_files_read_back() {
    let generated = Pact {
        consumer: "web".into(),
        provider: "users".into(),
        version: PactVersion::V3,
        interactions: pact(vec![interaction(
            "get user",
            "GET",
            "/users/42",
            &["s"],
            json!({"id": 42}),
        )])
        .interactions,
    };
    let text = generated.to_json().to_string();
    let back = read("p.json", &text).unwrap().pact;
    assert_eq!(back, generated);
}
