//! The messages listed in docs/http.md under "Errors and warnings" (task
//! 4.12): each one is produced by the engine against the in-process test
//! server. A `<...>` in the page marks a variable part; the message must
//! start with the text before it.

mod support;

use rustman_core::model::RequestSettings;
use rustman_core::vars::{Resolved, ResolvedBody};
use rustman_http::{HttpEngine, SendOptions, import_curl, interactive_login_required};
use support::request::{get, pair};
use support::{TestCerts, TestServer, Tls};

/// The lines of the ```text block after the "Errors and warnings" heading.
fn documented_messages() -> Vec<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/http.md");
    let text = std::fs::read_to_string(path).unwrap();
    let (_, section) = text.split_once("## Errors and warnings").unwrap();
    let (_, block) = section.split_once("```text\n").unwrap();
    let (block, _) = block.split_once("```").unwrap();
    block.lines().map(str::to_owned).collect()
}

async fn produced_messages() -> Vec<String> {
    let server = TestServer::http().await;
    let engine = HttpEngine::new();
    let error = |request: rustman_core::vars::ResolvedRequest| {
        let engine = &engine;
        async move {
            let options = SendOptions::new(&Default::default(), &request.settings);
            engine
                .send(&request, &options)
                .await
                .unwrap_err()
                .to_string()
        }
    };

    let mut slow = get(&server.url("/delay/2000"));
    slow.settings = RequestSettings {
        timeout_ms: Some(500),
        ..RequestSettings::default()
    };
    let certs = TestCerts::generate();
    let self_signed = TestServer::https(&certs, Tls::SelfSigned).await;

    let mut stale = get(&server.url("/echo"));
    stale.method = "POST".into();
    stale.headers = vec![pair("Content-Length", "3"), pair("Host", "other.example")];
    stale.body = ResolvedBody::Text(Resolved::literal("0123456789"));
    let warnings = engine
        .send(&stale, &SendOptions::default())
        .await
        .unwrap()
        .warnings;

    let mut bomb_options = SendOptions::default();
    bomb_options.body.max_decoded = 1024;
    let bomb = engine
        .send(&get(&server.url("/zeros/zstd/4096")), &bomb_options)
        .await
        .unwrap_err()
        .to_string();

    let mut messages = vec![
        error(slow).await,
        error(get(&server.url("/loop"))).await,
        error(get(&self_signed.url("/echo"))).await,
        bomb,
        interactive_login_required("<request>").to_string(),
    ];
    messages.extend(warnings);
    messages.extend(import_curl("curl --limit-rate 1k x.io").unwrap().warnings);
    messages
}

#[tokio::test]
async fn documented_messages_match_the_engine() {
    let documented = documented_messages();
    let produced = produced_messages().await;
    assert_eq!(documented.len(), produced.len(), "{produced:#?}");
    for (doc, actual) in documented.iter().zip(&produced) {
        let matches = match doc.split_once('<') {
            Some((fixed, _)) => actual.starts_with(fixed),
            None => doc == actual,
        };
        assert!(matches, "documented `{doc}`, engine says `{actual}`");
    }
}
