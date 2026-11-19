//! Task 1.4: a graceful stop lets the request in flight finish, then
//! refuses new connections.

mod common;

use std::time::Duration;

use common::{read_to_close, start, status};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

#[tokio::test]
async fn a_delayed_in_flight_request_completes_before_the_stop() {
    let mock = start().await;
    let addr = mock.local_addr();
    let mut stream = TcpStream::connect(addr).await.unwrap();
    // The head now, the body only after the stop began: the request is in
    // flight while the server drains.
    let head = "POST /users HTTP/1.1\r\nHost: x\r\nContent-Length: 2\r\n\r\n";
    stream.write_all(head.as_bytes()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let finish = tokio::spawn(mock.finish(Duration::from_secs(5)));
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !finish.is_finished(),
        "the stop did not wait for the request"
    );
    stream.write_all(b"{}").await.unwrap();
    let response = read_to_close(&mut stream).await;
    assert_eq!(status(&response), 201, "{response}");
    tokio::time::timeout(Duration::from_secs(10), finish)
        .await
        .expect("the stop did not end")
        .unwrap();
    assert!(TcpStream::connect(addr).await.is_err());
}

#[tokio::test]
async fn the_drain_ends_after_the_grace_period() {
    let mock = start().await;
    let mut stream = TcpStream::connect(mock.local_addr()).await.unwrap();
    let head = "POST /users HTTP/1.1\r\nHost: x\r\nContent-Length: 2\r\n\r\n";
    stream.write_all(head.as_bytes()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let finished = tokio::time::timeout(
        Duration::from_secs(10),
        mock.finish(Duration::from_millis(200)),
    )
    .await;
    assert!(finished.is_ok(), "the stop waited past its grace");
}
