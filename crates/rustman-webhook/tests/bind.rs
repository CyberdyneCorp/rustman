//! Binding (spec: Listener lifecycle and bind address).

mod common;

use rustman_webhook::{CaptureStore, Listener, ListenerConfig, Mocks};

#[tokio::test]
async fn port_zero_picks_a_free_port_and_the_banner_shows_it() {
    let listener = Listener::bind(
        &common::config(Default::default()),
        CaptureStore::default(),
        Mocks::empty(),
    )
    .await
    .unwrap();
    let port = listener.local_addr().port();
    assert_ne!(port, 0);
    assert_eq!(
        listener.banner(),
        format!("Listening on http://127.0.0.1:{port}")
    );
}

#[tokio::test]
async fn a_port_in_use_is_reported() {
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port();
    let config = ListenerConfig {
        port,
        ..ListenerConfig::default()
    };
    let err = Listener::bind(&config, CaptureStore::default(), Mocks::empty())
        .await
        .err()
        .unwrap();
    assert_eq!(
        err.to_string(),
        format!("webhook: cannot bind 127.0.0.1:{port}: address in use")
    );
}

#[tokio::test]
async fn stopping_releases_the_port() {
    let store = CaptureStore::default();
    let listener = Listener::bind(&common::config(Default::default()), store, Mocks::empty())
        .await
        .unwrap();
    let addr = listener.local_addr();
    listener.spawn().stop().await;
    let config = ListenerConfig {
        port: addr.port(),
        ..ListenerConfig::default()
    };
    assert!(
        Listener::bind(&config, CaptureStore::default(), Mocks::empty())
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn finishing_answers_the_request_in_flight_then_releases_the_port() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("mocks.yaml");
    std::fs::write(
        &file,
        "mocks:\n- {path: /slow, status: 202, delay_ms: 300, body: late}\n",
    )
    .unwrap();
    let mut l = common::start_with(Default::default(), Mocks::from_file(&file).unwrap()).await;
    let addr = l.addr();
    let client = tokio::spawn(common::raw(
        addr,
        b"POST /slow HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n",
    ));
    l.next_event(|e| matches!(e, rustman_webhook::Event::Captured(_)))
        .await;
    l.handle.finish(std::time::Duration::from_secs(30)).await;
    let response = client.await.unwrap();
    assert_eq!(common::status(&response), 202);
    assert_eq!(common::body(&response), "late");
    assert!(tokio::net::TcpStream::connect(addr).await.is_err());
}
