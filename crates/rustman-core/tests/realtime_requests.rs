//! `kind: ws` and `kind: sse` request files (add-realtime-client tasks 1.2,
//! 1.3): registration, the write/load round trip, and interpolation with
//! secrets kept out of `Debug` output.

use std::fs;
use std::path::Path;

use rustman_core::Workspace;
use rustman_core::format::Located;
use rustman_core::format::write::{request_to_yaml, sse_to_yaml, ws_to_yaml};
use rustman_core::model::{
    CloseStep, Frame, KindRegistry, REQUEST_VERSION, RealtimeLimits, Request, SseRequest, Step,
    WsRequest,
};
use rustman_core::vars::{
    ResolvedBinary, ResolvedFrame, ResolvedRealtime, ResolvedStep, Resolver, SecretLookup,
    resolve_realtime,
};
use serde_json::json;

const FEED: &str = "\
version: 1
kind: ws
name: Ticker
description: Live prices
tags: [realtime]
url: 'wss://{{host}}/feed?token={{api_token}}'
headers:
- name: X-Client
  value: '{{client}}'
auth:
  type: bearer
  token: '{{api_token}}'
subprotocols:
- v2.feed
- v1.feed
steps:
- send:
    json:
      op: subscribe
      channel: '{{channel}}'
      depth: 10
- wait_for:
    json:
      path: $.channel
      equals: '{{channel}}'
    timeout_ms: 2000
- send:
    binary:
      file: fixtures/blob.bin
- sleep: 50
- close:
    code: 1000
    reason: done
limits:
  max_messages: 5
  duration_ms: 1000
tls:
  verify: false
variables:
  client: cli
assertions:
- latency: {total_lt: 2000}
captures:
- var: first
  from: json
  path: $.messages[0]
";

const EVENTS: &str = "\
version: 1
kind: sse
name: Chat
method: POST
url: 'https://{{host}}/v1/chat'
headers:
- name: X-Key
  value: '{{api_token}}'
body:
  type: json
  content:
    stream: true
last_event_id: '41'
reconnect:
  max_attempts: 2
limits:
  max_messages: 3
";

fn load(text: &str) -> Request {
    KindRegistry::default()
        .load(Path::new("r.rman.yaml"), text)
        .unwrap()
}

#[test]
fn ws_and_sse_are_registered_kinds() {
    let names = KindRegistry::default().names();
    assert!(names.contains(&"ws") && names.contains(&"sse"), "{names:?}");
    let err = KindRegistry::default()
        .load(
            Path::new("x.rman.yaml"),
            "version: 1\nkind: stomp\nname: x\n",
        )
        .unwrap_err();
    assert!(err.message.contains("ws, sse"), "{}", err.message);
    assert!(matches!(load(FEED), Request::Ws(_)));
    assert!(matches!(load(EVENTS), Request::Sse(_)));
}

#[test]
fn the_wrong_scheme_is_a_load_error() {
    let err = KindRegistry::default()
        .load(
            Path::new("f.rman.yaml"),
            "version: 1\nkind: ws\nname: f\nurl: https://example.com/feed\n",
        )
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "f.rman.yaml:4: ws: url must use ws:// or wss://"
    );
}

#[test]
fn loaded_files_are_written_back_unchanged_and_rewritten_files_load_equal() {
    for text in [FEED, EVENTS] {
        let loaded = load(text);
        assert_eq!(request_to_yaml(&loaded), text);
        let written = match &loaded {
            Request::Ws(ws) => ws_to_yaml(ws),
            Request::Sse(sse) => sse_to_yaml(sse),
            other => panic!("{other:?}"),
        };
        let again = load(&written);
        let same = match (&loaded, &again) {
            (Request::Ws(a), Request::Ws(b)) => {
                **a == WsRequest {
                    source: a.source.clone(),
                    ..(**b).clone()
                }
            }
            (Request::Sse(a), Request::Sse(b)) => {
                **a == SseRequest {
                    source: a.source.clone(),
                    ..(**b).clone()
                }
            }
            _ => false,
        };
        assert!(same, "{written}");
    }
}

#[test]
fn a_request_built_in_code_is_written_field_by_field() {
    let built = WsRequest {
        version: REQUEST_VERSION,
        kind: "ws".into(),
        name: "Echo".into(),
        description: None,
        tags: None,
        url: Located::new("ws://localhost:9000/".into()),
        headers: None,
        auth: None,
        subprotocols: None,
        steps: Some(vec![
            Located::new(Step::Send(Frame::Text("hi".into()))),
            Located::new(Step::Close(CloseStep::default())),
        ]),
        limits: Some(
            rustman_core::model::Checked::new(RealtimeLimits {
                max_messages: Some(3),
                ..RealtimeLimits::default()
            })
            .unwrap(),
        ),
        tls: None,
        variables: None,
        assertions: None,
        captures: None,
        source: String::new(),
    };
    let text = request_to_yaml(&Request::Ws(Box::new(built.clone())));
    assert_eq!(
        text,
        "version: 1\nkind: ws\nname: Echo\nurl: ws://localhost:9000/\nsteps:\n  - send:\n      text: hi\n  - close: {}\nlimits:\n  max_messages: 3\n"
    );
    let Request::Ws(again) = load(&text) else {
        panic!("a ws request");
    };
    assert_eq!(
        *again,
        WsRequest {
            source: text.clone(),
            ..built
        }
    );
}

struct Store;

impl SecretLookup for Store {
    fn secret(&self, env: &str, name: &str) -> Result<Option<String>, String> {
        Ok((env == "prod" && name == "api_token").then(|| "s3cr3t-ws".to_owned()))
    }
}

fn workspace(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::write(
        root.join("rustman.toml"),
        "[workspace]\nname = \"t\"\nversion = 1\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("environments")).unwrap();
    fs::write(
        root.join("environments/prod.yaml"),
        "variables:\n  host: feed.test\n  channel: ticker\nsecrets: [api_token]\n",
    )
    .unwrap();
    for (path, text) in files {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    dir
}

#[test]
fn urls_headers_and_steps_are_interpolated_and_secrets_stay_hidden() {
    let dir = workspace(&[("feed.rman.yaml", FEED), ("fixtures/blob.bin", "\x01\x02")]);
    let ws = Workspace::load(dir.path()).unwrap();
    let request = &ws.find_by_name("Ticker")[0];
    let env = ws.active_environment(Some("prod"), None).unwrap();
    let resolver = Resolver::for_request(&ws.settings, request, env, &Store);
    let ResolvedRealtime::Ws(resolved) = resolve_realtime(request, &resolver).unwrap() else {
        panic!("a ws session");
    };
    assert_eq!(
        resolved.http.url.text,
        "wss://feed.test/feed?token=s3cr3t-ws"
    );
    assert_eq!(resolved.http.method, "GET");
    assert_eq!(resolved.http.headers[0].value.text, "cli");
    assert_eq!(resolved.subprotocols, ["v2.feed", "v1.feed"]);
    assert_eq!(resolved.http.settings.verify_tls, Some(false));
    let ResolvedStep::Send(ResolvedFrame::Json(subscribe)) = &resolved.steps[0] else {
        panic!("a json frame");
    };
    assert_eq!(
        subscribe,
        &json!({"op": "subscribe", "channel": "ticker", "depth": 10})
    );
    let ResolvedStep::WaitFor(wait) = &resolved.steps[1] else {
        panic!("wait_for");
    };
    assert_eq!(wait.describe(), "json $.channel equals \"ticker\"");
    let ResolvedStep::Send(ResolvedFrame::Binary(ResolvedBinary::File(file))) = &resolved.steps[2]
    else {
        panic!("a file frame");
    };
    assert!(file.path.ends_with("fixtures/blob.bin"), "{file:?}");
    assert_eq!(resolved.limits.effective_max_messages(), 5);
    let shown = format!("{resolved:?}");
    assert!(!shown.contains("s3cr3t-ws"), "{shown}");
    assert!(shown.contains("«redacted:api_token»"), "{shown}");
}

#[test]
fn sse_requests_resolve_with_their_body_and_accept_header() {
    let dir = workspace(&[("chat.rman.yaml", EVENTS)]);
    let ws = Workspace::load(dir.path()).unwrap();
    let request = &ws.find_by_name("Chat")[0];
    let env = ws.active_environment(Some("prod"), None).unwrap();
    let resolver = Resolver::for_request(&ws.settings, request, env, &Store);
    let ResolvedRealtime::Sse(resolved) = resolve_realtime(request, &resolver).unwrap() else {
        panic!("an sse session");
    };
    assert_eq!(resolved.http.method, "POST");
    assert_eq!(resolved.http.url.text, "https://feed.test/v1/chat");
    let accept = resolved
        .http
        .headers
        .iter()
        .find(|h| h.name.text == "Accept")
        .unwrap();
    assert_eq!(accept.value.text, "text/event-stream");
    assert_eq!(resolved.last_event_id.as_deref(), Some("41"));
    assert_eq!(resolved.reconnect.effective_max_attempts(), 2);
    assert!(!format!("{resolved:?}").contains("s3cr3t-ws"));
}

#[test]
fn a_binary_file_outside_the_workspace_is_refused() {
    let text = "version: 1\nkind: ws\nname: Leak\nurl: ws://h/\nsteps:\n- send: {binary: {file: '{{p}}'}}\nvariables:\n  p: ../../etc/passwd\n";
    let dir = workspace(&[("leak.rman.yaml", text)]);
    let ws = Workspace::load(dir.path()).unwrap();
    let request = &ws.find_by_name("Leak")[0];
    let resolver = Resolver::for_request(&ws.settings, request, None, &Store);
    let err = resolve_realtime(request, &resolver).unwrap_err();
    assert!(err.to_string().contains("escapes workspace"), "{err}");
}
