//! Default headers, computed headers, decompression, spilling and binary
//! detection (task 4.10). The memory ceiling is in `body_memory.rs`.

mod support;

use rustman_core::vars::{Resolved, ResolvedBody, ResolvedPart, ResolvedPartSource};
use rustman_http::{BodyOptions, Exchange, HttpEngine, SendOptions, USER_AGENT_VALUE};
use support::TestServer;
use support::request::{echoed_header, file, get, pair};
use support::routes::{COMPRESSED_TEXT, PNG};

async fn send(request: &rustman_core::vars::ResolvedRequest, options: &SendOptions) -> Exchange {
    HttpEngine::new().send(request, options).await.unwrap()
}

fn echo(exchange: &Exchange) -> serde_json::Value {
    serde_json::from_slice(&exchange.body).unwrap()
}

#[tokio::test]
async fn default_headers_are_sent_unless_set() {
    let server = TestServer::http().await;
    let exchange = send(&get(&server.url("/echo")), &SendOptions::default()).await;
    let echo = echo(&exchange);
    assert_eq!(echoed_header(&echo, "user-agent"), [USER_AGENT_VALUE]);
    assert_eq!(echoed_header(&echo, "accept"), ["*/*"]);
    assert!(USER_AGENT_VALUE.starts_with("rustman/"));

    let mut request = get(&server.url("/echo"));
    request.headers = vec![pair("User-Agent", "mine/1"), pair("Accept", "text/csv")];
    let echo = self::echo(&send(&request, &SendOptions::default()).await);
    assert_eq!(echoed_header(&echo, "user-agent"), ["mine/1"]);
    assert_eq!(echoed_header(&echo, "accept"), ["text/csv"]);
}

#[tokio::test]
async fn explicit_content_type_wins_once() {
    let server = TestServer::http().await;
    let mut request = get(&server.url("/echo"));
    request.method = "POST".into();
    request.body = ResolvedBody::Json(serde_json::json!({"a": 1}));
    request.headers = vec![pair("Content-Type", "application/vnd.api+json")];
    let echo = echo(&send(&request, &SendOptions::default()).await);
    assert_eq!(
        echoed_header(&echo, "content-type"),
        ["application/vnd.api+json"]
    );
}

#[tokio::test]
async fn body_types_set_their_content_type() {
    let server = TestServer::http().await;
    let dir = tempfile::tempdir().unwrap();
    let cases = [
        (
            ResolvedBody::Json(serde_json::json!([])),
            "application/json",
        ),
        (
            ResolvedBody::Text(Resolved::literal("hi")),
            "text/plain; charset=utf-8",
        ),
        (
            ResolvedBody::Form(vec![pair("a", "b c")]),
            "application/x-www-form-urlencoded",
        ),
        (
            ResolvedBody::File(file(dir.path(), "b.bin", b"\x00\x01").unwrap()),
            "application/octet-stream",
        ),
    ];
    for (body, content_type) in cases {
        let mut request = get(&server.url("/echo"));
        request.method = "POST".into();
        request.body = body;
        let echo = echo(&send(&request, &SendOptions::default()).await);
        assert_eq!(echoed_header(&echo, "content-type"), [content_type]);
    }
}

#[tokio::test]
async fn multipart_bodies_carry_their_boundary() {
    let server = TestServer::http().await;
    let dir = tempfile::tempdir().unwrap();
    let mut request = get(&server.url("/echo"));
    request.method = "POST".into();
    request.body = ResolvedBody::Multipart(vec![
        ResolvedPart {
            name: "note".into(),
            source: ResolvedPartSource::Text(Resolved::literal("hello")),
            content_type: None,
        },
        ResolvedPart {
            name: "doc".into(),
            source: ResolvedPartSource::File(file(dir.path(), "a.txt", "file text").unwrap()),
            content_type: Some("text/plain".into()),
        },
    ]);
    let echo = echo(&send(&request, &SendOptions::default()).await);
    let content_type = &echoed_header(&echo, "content-type")[0];
    let boundary = content_type
        .strip_prefix("multipart/form-data; boundary=")
        .unwrap();
    let body = echo["body"].as_str().unwrap();
    assert!(body.starts_with(&format!("--{boundary}\r\n")), "{body}");
    assert!(body.contains("name=\"note\"\r\n\r\nhello\r\n"), "{body}");
    assert!(
        body.contains(
            "name=\"doc\"; filename=\"a.txt\"\r\nContent-Type: text/plain\r\n\r\nfile text\r\n"
        ),
        "{body}"
    );
    assert!(body.ends_with(&format!("--{boundary}--\r\n")), "{body}");
}

#[tokio::test]
async fn a_stale_content_length_is_replaced_with_a_warning() {
    let server = TestServer::http().await;
    let mut request = get(&server.url("/echo"));
    request.method = "POST".into();
    request.headers = vec![pair("Content-Length", "3")];
    request.body = ResolvedBody::Text(Resolved::literal("0123456789"));
    let exchange = send(&request, &SendOptions::default()).await;
    let echo = echo(&exchange);
    assert_eq!(echoed_header(&echo, "content-length"), ["10"]);
    assert_eq!(echo["body"], "0123456789");
    assert_eq!(
        exchange.warnings,
        ["ignored stale Content-Length: 3 (sent the computed 10)"]
    );
}

#[tokio::test]
async fn a_matching_content_length_needs_no_warning() {
    let server = TestServer::http().await;
    let mut request = get(&server.url("/echo"));
    request.method = "POST".into();
    request.headers = vec![pair("Content-Length", "2")];
    request.body = ResolvedBody::Text(Resolved::literal("ok"));
    let exchange = send(&request, &SendOptions::default()).await;
    assert_eq!(echoed_header(&echo(&exchange), "content-length"), ["2"]);
    assert!(exchange.warnings.is_empty(), "{:?}", exchange.warnings);
}

#[tokio::test]
async fn host_is_computed_from_the_url() {
    let server = TestServer::http().await;
    let mut request = get(&server.url("/echo"));
    request.headers = vec![pair("Host", "elsewhere.example")];
    let exchange = send(&request, &SendOptions::default()).await;
    let authority = format!("localhost:{}", server.port());
    assert_eq!(
        echoed_header(&echo(&exchange), "host"),
        std::slice::from_ref(&authority)
    );
    assert_eq!(
        exchange.warnings,
        [format!(
            "ignored Host: elsewhere.example (sent the URL's host {authority})"
        )]
    );
}

#[tokio::test]
async fn compressed_bodies_are_decoded() {
    let server = TestServer::http().await;
    for encoding in ["gzip", "br", "zstd", "deflate"] {
        let url = server.url(&format!("/compressed/{encoding}"));
        let exchange = send(&get(&url), &SendOptions::default()).await;
        assert_eq!(&exchange.body[..], COMPRESSED_TEXT.as_bytes(), "{encoding}");
        assert_eq!(exchange.size_bytes, COMPRESSED_TEXT.len() as u64);
        assert!(!exchange.binary);
    }
}

#[tokio::test]
async fn decompress_false_keeps_the_encoded_bytes() {
    let server = TestServer::http().await;
    let mut request = get(&server.url("/compressed/gzip"));
    request.settings.decompress = Some(false);
    let options = SendOptions::new(&Default::default(), &request.settings);
    let exchange = send(&request, &options).await;
    assert_eq!(&exchange.body[..2], &[0x1f, 0x8b]);
    assert!(exchange.binary);
}

#[tokio::test]
async fn a_png_is_binary_and_byte_exact() {
    let server = TestServer::http().await;
    let exchange = send(&get(&server.url("/png")), &SendOptions::default()).await;
    assert!(exchange.binary);
    assert_eq!(&exchange.body[..], PNG);
    assert_eq!(exchange.size_bytes, PNG.len() as u64);
    assert_eq!(exchange.spill, None);
}

#[tokio::test]
async fn text_bodies_are_not_binary() {
    let server = TestServer::http().await;
    let exchange = send(&get(&server.url("/echo")), &SendOptions::default()).await;
    assert!(!exchange.binary);
    let empty = send(&get(&server.url("/status/204")), &SendOptions::default()).await;
    assert!(!empty.binary && empty.body.is_empty() && empty.size_bytes == 0);
}

#[tokio::test]
async fn bodies_over_the_cap_spill_to_disk() {
    let server = TestServer::http().await;
    let dir = tempfile::tempdir().unwrap();
    let options = SendOptions {
        body: BodyOptions {
            max_in_memory: 100_000,
            spill_dir: dir.path().join(".rustman/bodies"),
            ..BodyOptions::default()
        },
        ..SendOptions::default()
    };
    let exchange = send(&get(&server.url("/bytes/300000")), &options).await;
    let spill = exchange.spill.clone().unwrap();
    assert_eq!(
        spill,
        dir.path()
            .join(".rustman/bodies")
            .join(format!("{}.bin", exchange.id))
    );
    assert_eq!(exchange.size_bytes, 300_000);
    assert_eq!(exchange.body.len(), rustman_http::PREVIEW_BYTES);
    let saved = std::fs::read(&spill).unwrap();
    assert_eq!(saved.len(), 300_000);
    assert_eq!(&saved[..exchange.body.len()], &exchange.body[..]);
    assert!(
        saved
            .iter()
            .enumerate()
            .all(|(i, b)| *b == (i % (64 * 1024) % 251) as u8)
    );

    let small = send(&get(&server.url("/bytes/100000")), &options).await;
    assert_eq!((small.spill, small.body.len()), (None, 100_000));
}

fn spilling_to(dir: &std::path::Path, max_in_memory: u64) -> SendOptions {
    SendOptions {
        body: BodyOptions {
            max_in_memory,
            spill_dir: dir.join(".rustman/bodies"),
            ..BodyOptions::default()
        },
        ..SendOptions::default()
    }
}

fn spilled_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    match std::fs::read_dir(dir.join(".rustman/bodies")) {
        Ok(entries) => entries.map(|e| e.unwrap().path()).collect(),
        Err(_) => Vec::new(),
    }
}

async fn send_err(url: &str, options: &SendOptions) -> String {
    let err = HttpEngine::new().send(&get(url), options).await;
    err.unwrap_err().to_string()
}

#[tokio::test]
async fn an_endless_body_stops_at_the_download_limit() {
    let server = TestServer::http().await;
    let dir = tempfile::tempdir().unwrap();
    let mut options = spilling_to(dir.path(), 64 * 1024);
    options.body.max_download = 2 * 1024 * 1024;
    assert_eq!(
        send_err(&server.url("/endless"), &options).await,
        "network: the response body is larger than 2097152 bytes"
    );
    assert_eq!(spilled_files(dir.path()), Vec::<std::path::PathBuf>::new());
}

#[tokio::test]
async fn the_download_limit_applies_to_bodies_left_encoded() {
    let server = TestServer::http().await;
    let dir = tempfile::tempdir().unwrap();
    let mut options = spilling_to(dir.path(), 1024);
    options.body.max_download = 300_000;
    let exact = send(&get(&server.url("/bytes/300000")), &options).await;
    assert_eq!(exact.size_bytes, 300_000);
    assert_eq!(
        send_err(&server.url("/bytes/300001"), &options).await,
        "network: the response body is larger than 300000 bytes"
    );
    options.body.decompress = false;
    options.body.max_download = 10;
    assert_eq!(
        send_err(&server.url("/compressed/gzip"), &options).await,
        "network: the response body is larger than 10 bytes"
    );
}

#[tokio::test]
async fn a_decompression_bomb_stops_at_the_decoded_limit() {
    let server = TestServer::http().await;
    let dir = tempfile::tempdir().unwrap();
    let mut options = spilling_to(dir.path(), 1024 * 1024);
    options.body.max_decoded = 4 * 1024 * 1024;
    for encoding in ["br", "zstd"] {
        let url = server.url(&format!("/zeros/{encoding}/{}", 64 * 1024 * 1024));
        let err = HttpEngine::new()
            .send(&get(&url), &options)
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "network: the decompressed response body is larger than 4194304 bytes",
            "{encoding}"
        );
    }
    assert_eq!(spilled_files(dir.path()), Vec::<std::path::PathBuf>::new());
    let url = server.url(&format!("/zeros/br/{}", 3 * 1024 * 1024));
    let exchange = send(&get(&url), &options).await;
    assert_eq!(exchange.size_bytes, 3 * 1024 * 1024);
}

#[tokio::test]
async fn an_unfinished_body_leaves_no_spill_file() {
    let server = TestServer::http().await;
    let dir = tempfile::tempdir().unwrap();
    let mut options = spilling_to(dir.path(), 64 * 1024);
    // Warm the pool so the 500 ms budget measures the body, not DNS and
    // connect on a loaded machine.
    let engine = HttpEngine::new();
    engine
        .send(&get(&server.url("/status/200")), &options)
        .await
        .unwrap();
    options.timeout = std::time::Duration::from_millis(500);
    let err = engine
        .send(&get(&server.url("/stall-after/1048576")), &options)
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "timeout after 500 ms (response)");
    assert_eq!(spilled_files(dir.path()), Vec::<std::path::PathBuf>::new());
}

#[tokio::test]
async fn only_the_final_hop_keeps_a_spill_file() {
    let server = TestServer::http().await;
    let dir = tempfile::tempdir().unwrap();
    let options = spilling_to(dir.path(), 1024);
    let url = server.url("/redirect/302?pad=100000&to=/bytes/5000");
    let exchange = send(&get(&url), &options).await;
    assert_eq!(exchange.redirects.len(), 1);
    let spill = exchange.spill.clone().unwrap();
    assert_eq!(spilled_files(dir.path()), [spill]);
}
