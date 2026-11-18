//! The redirect policy (task 4.4).

mod support;

use rustman_core::vars::ResolvedBody;
use rustman_http::{HttpEngine, RedirectHop, RequestError, SendOptions};
use support::TestServer;
use support::request::{echoed_header, get, pair};

fn echo(body: &[u8]) -> serde_json::Value {
    serde_json::from_slice(body).unwrap()
}

#[tokio::test]
async fn a_redirect_chain_is_recorded() {
    let server = TestServer::http().await;
    let first = server.url("/redirect/302?to=/redirect/301?to=/echo");
    let exchange = HttpEngine::new()
        .send(&get(&first), &SendOptions::default())
        .await
        .unwrap();
    assert_eq!(exchange.status, 200);
    assert_eq!(exchange.url, server.url("/echo"));
    assert_eq!(
        exchange.redirects,
        [
            RedirectHop {
                status: 302,
                url: first,
            },
            RedirectHop {
                status: 301,
                url: server.url("/redirect/301?to=/echo"),
            },
        ]
    );
}

fn post_json(url: &str) -> rustman_core::vars::ResolvedRequest {
    let mut request = get(url);
    request.method = "POST".into();
    request.body = ResolvedBody::Json(serde_json::json!({"a": 1}));
    request
}

#[tokio::test]
async fn see_other_style_redirects_switch_to_get_and_drop_the_body() {
    let server = TestServer::http().await;
    for code in [301, 302, 303] {
        let request = post_json(&server.url(&format!("/redirect/{code}?to=/echo")));
        let exchange = HttpEngine::new()
            .send(&request, &SendOptions::default())
            .await
            .unwrap();
        let echo = echo(&exchange.body);
        assert_eq!(echo["method"], "GET", "{code}");
        assert_eq!(echo["body"], "", "{code}");
        assert!(echoed_header(&echo, "content-type").is_empty(), "{code}");
    }
}

#[tokio::test]
async fn temporary_and_permanent_redirects_keep_method_and_body() {
    let server = TestServer::http().await;
    for code in [307, 308] {
        let request = post_json(&server.url(&format!("/redirect/{code}?to=/echo")));
        let exchange = HttpEngine::new()
            .send(&request, &SendOptions::default())
            .await
            .unwrap();
        let echo = echo(&exchange.body);
        assert_eq!(echo["method"], "POST", "{code}");
        assert_eq!(echo["body"], r#"{"a":1}"#, "{code}");
        assert_eq!(echoed_header(&echo, "content-type"), ["application/json"]);
    }
}

#[tokio::test]
async fn authorization_is_stripped_when_the_host_changes() {
    let server = TestServer::http().await;
    // `localhost` and `127.0.0.1` are the same server under two host names.
    let other_host = format!("http://127.0.0.1:{}/echo", server.port());
    let cases = [(other_host.as_str(), false), ("/echo", true)];
    for (to, kept) in cases {
        let mut request = get(&server.url(&format!("/redirect/302?to={to}")));
        request.headers = vec![pair("Authorization", "Bearer t0ken"), pair("X-Keep", "1")];
        let exchange = HttpEngine::new()
            .send(&request, &SendOptions::default())
            .await
            .unwrap();
        let echo = echo(&exchange.body);
        let expected: &[&str] = if kept { &["Bearer t0ken"] } else { &[] };
        assert_eq!(echoed_header(&echo, "authorization"), expected, "{to}");
        assert_eq!(echoed_header(&echo, "x-keep"), ["1"], "{to}");
    }
}

#[tokio::test]
async fn bearer_auth_is_stripped_when_the_host_changes() {
    use rustman_core::model::Auth;
    let server = TestServer::http().await;
    let other_host = format!("http://127.0.0.1:{}/echo", server.port());
    let cases = [(other_host.as_str(), false), ("/echo", true)];
    for (to, kept) in cases {
        let mut request = get(&server.url(&format!("/redirect/307?to={to}")));
        request.auth = Some(Auth::Bearer {
            token: "t0ken".into(),
        });
        let exchange = HttpEngine::new()
            .send(&request, &SendOptions::default())
            .await
            .unwrap();
        let echo = echo(&exchange.body);
        let expected: &[&str] = if kept { &["Bearer t0ken"] } else { &[] };
        assert_eq!(echoed_header(&echo, "authorization"), expected, "{to}");
    }
}

#[tokio::test]
async fn api_keys_and_cookies_are_stripped_when_the_host_changes() {
    use rustman_core::model::{ApiKeyLocation, Auth};
    let server = TestServer::http().await;
    let other_host = format!("http://127.0.0.1:{}/echo?key=k1%26&a=1", server.port());
    let cases = [(other_host.as_str(), false), ("/echo?key=k1%26&a=1", true)];
    for (to, kept) in cases {
        let to_encoded: String = form_urlencoded::byte_serialize(to.as_bytes()).collect();
        let url = server.url(&format!("/redirect/302?to={to_encoded}"));
        for location in [ApiKeyLocation::Header, ApiKeyLocation::Query] {
            let mut request = get(&url);
            request.headers = vec![pair("Cookie", "session=c00kie")];
            request.auth = Some(Auth::ApiKey {
                name: "key".into(),
                value: "k1&".into(),
                location: Some(location),
            });
            let exchange = HttpEngine::new()
                .send(&request, &SendOptions::default())
                .await
                .unwrap();
            let echo = echo(&exchange.body);
            let cookie: &[&str] = if kept { &["session=c00kie"] } else { &[] };
            assert_eq!(echoed_header(&echo, "cookie"), cookie, "{to}");
            let header: &[&str] = match (kept, location) {
                (true, ApiKeyLocation::Header) => &["k1&"],
                _ => &[],
            };
            assert_eq!(echoed_header(&echo, "key"), header, "{to} {location:?}");
            let path = match (kept, location) {
                (false, ApiKeyLocation::Query) => "/echo?a=1",
                _ => "/echo?key=k1%26&a=1",
            };
            assert_eq!(echo["path"], path, "{to} {location:?}");
        }
    }
}

#[tokio::test]
async fn a_redirect_loop_stops_after_ten_hops() {
    let server = TestServer::http().await;
    let err = HttpEngine::new()
        .send(&get(&server.url("/loop")), &SendOptions::default())
        .await
        .unwrap_err();
    assert_eq!(err, RequestError::TooManyRedirects(10));
    assert_eq!(err.to_string(), "too many redirects (10)");
}

#[tokio::test]
async fn max_redirects_is_configurable() {
    let server = TestServer::http().await;
    let options = SendOptions {
        max_redirects: 2,
        ..SendOptions::default()
    };
    let three = "/redirect/302?to=/redirect/302?to=/redirect/302?to=/echo";
    let err = HttpEngine::new()
        .send(&get(&server.url(three)), &options)
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "too many redirects (2)");
    let two = "/redirect/302?to=/redirect/302?to=/echo";
    let exchange = HttpEngine::new()
        .send(&get(&server.url(two)), &options)
        .await
        .unwrap();
    assert_eq!(exchange.redirects.len(), 2);
}

#[tokio::test]
async fn follow_redirects_false_returns_the_redirect() {
    let server = TestServer::http().await;
    let options = SendOptions {
        follow_redirects: false,
        ..SendOptions::default()
    };
    let exchange = HttpEngine::new()
        .send(&get(&server.url("/redirect/302?to=/echo")), &options)
        .await
        .unwrap();
    assert_eq!(exchange.status, 302);
    assert_eq!(exchange.headers["location"], "/echo");
    assert!(exchange.redirects.is_empty());
}

#[tokio::test]
async fn the_total_timeout_covers_every_hop() {
    let server = TestServer::http().await;
    let options = SendOptions {
        timeout: std::time::Duration::from_millis(1000),
        ..SendOptions::default()
    };
    let err = HttpEngine::new()
        .send(&get(&server.url("/redirect/302?to=/delay/5000")), &options)
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "timeout after 1000 ms (response)");
}

/// A `POST` of a form with a secret in `X-Api-Key` and in the body.
fn secret_post(url: &str) -> rustman_core::vars::ResolvedRequest {
    let mut request = get(url);
    request.method = "POST".into();
    request.headers = vec![pair("X-Api-Key", "k-CANARY-41"), pair("X-Keep", "1")];
    request.body = ResolvedBody::Form(vec![
        pair("grant_type", "client_credentials"),
        pair("client_secret", "k-CANARY-41"),
    ]);
    request.secrets = vec![rustman_core::Secret::new("tok", "k-CANARY-41".to_owned())];
    request
}

#[tokio::test]
async fn secret_headers_and_bodies_stay_behind_on_a_cross_origin_redirect() {
    let server = TestServer::http().await;
    let other_host = format!("http://127.0.0.1:{}/echo", server.port());
    for code in [307, 308] {
        let request = secret_post(&server.url(&format!("/redirect/{code}?to={other_host}")));
        let exchange = HttpEngine::new()
            .send(&request, &SendOptions::default())
            .await
            .unwrap();
        let echo = echo(&exchange.body);
        assert_eq!(echo["method"], "POST", "{code}");
        assert_eq!(echo["body"], "", "{code}");
        assert!(echoed_header(&echo, "x-api-key").is_empty(), "{code}");
        assert_eq!(echoed_header(&echo, "x-keep"), ["1"], "{code}");
        let origin = format!("http://127.0.0.1:{}", server.port());
        assert_eq!(
            exchange.warnings,
            [format!(
                "cross-origin redirect to {origin}: dropped header x-api-key, the body, \
                 which carry a secret"
            )]
        );
        let shown = format!("{:?}", exchange.warnings);
        assert!(!shown.contains("CANARY"), "{shown}");
    }
}

#[tokio::test]
async fn secret_headers_and_bodies_follow_a_same_origin_redirect() {
    let server = TestServer::http().await;
    let request = secret_post(&server.url("/redirect/307?to=/echo"));
    let exchange = HttpEngine::new()
        .send(&request, &SendOptions::default())
        .await
        .unwrap();
    let echo = echo(&exchange.body);
    assert_eq!(echoed_header(&echo, "x-api-key"), ["k-CANARY-41"]);
    assert_eq!(
        echo["body"],
        "grant_type=client_credentials&client_secret=k-CANARY-41"
    );
    assert!(exchange.warnings.is_empty(), "{:?}", exchange.warnings);
}
