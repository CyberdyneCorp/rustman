//! The AMQP frame size against a fake broker on loopback: a server
//! announcing a huge `frame_max` in Connection.Tune gets the client's
//! cap back in Tune-Ok, so `lapin` never sizes its buffers from it.

mod common;

use std::time::Duration;

use rustman_messaging::amqp::FRAME_MAX;
use rustman_messaging::{RunOptions, run};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const FRAME_END: u8 = 0xCE;

/// A method frame on channel 0.
fn method_frame(class: u16, method: u16, args: &[u8]) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend(class.to_be_bytes());
    payload.extend(method.to_be_bytes());
    payload.extend(args);
    let mut frame = vec![1, 0, 0];
    frame.extend(u32::try_from(payload.len()).unwrap().to_be_bytes());
    frame.extend(payload);
    frame.push(FRAME_END);
    frame
}

fn long_string(text: &str) -> Vec<u8> {
    let mut out = u32::try_from(text.len()).unwrap().to_be_bytes().to_vec();
    out.extend(text.as_bytes());
    out
}

/// The class, method and arguments of the next frame.
async fn read_method(socket: &mut TcpStream) -> (u16, u16, Vec<u8>) {
    let mut header = [0u8; 7];
    socket.read_exact(&mut header).await.unwrap();
    let size = u32::from_be_bytes(header[3..7].try_into().unwrap()) as usize;
    let mut payload = vec![0u8; size + 1];
    socket.read_exact(&mut payload).await.unwrap();
    assert_eq!(payload.pop(), Some(FRAME_END));
    let class = u16::from_be_bytes([payload[0], payload[1]]);
    let method = u16::from_be_bytes([payload[2], payload[3]]);
    (class, method, payload[4..].to_vec())
}

/// Answer one client up to its Tune-Ok, announcing `frame_max`; the
/// `frame_max` the client sent back.
async fn fake_broker(listener: TcpListener, frame_max: u32) -> u32 {
    let (mut socket, _) = listener.accept().await.unwrap();
    let mut protocol = [0u8; 8];
    socket.read_exact(&mut protocol).await.unwrap();
    assert_eq!(&protocol, b"AMQP\x00\x00\x09\x01");
    let mut start = vec![0, 9];
    start.extend(0u32.to_be_bytes());
    start.extend(long_string("PLAIN AMQPLAIN"));
    start.extend(long_string("en_US"));
    socket
        .write_all(&method_frame(10, 10, &start))
        .await
        .unwrap();
    assert_eq!(read_method(&mut socket).await.0, 10, "start-ok");
    let mut tune = Vec::new();
    tune.extend(0u16.to_be_bytes());
    tune.extend(frame_max.to_be_bytes());
    tune.extend(0u16.to_be_bytes());
    socket
        .write_all(&method_frame(10, 30, &tune))
        .await
        .unwrap();
    let (class, method, args) = read_method(&mut socket).await;
    assert_eq!((class, method), (10, 31), "tune-ok");
    u32::from_be_bytes(args[2..6].try_into().unwrap())
}

async fn negotiated(announced: u32, query: &str) -> u32 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(fake_broker(listener, announced));
    let text = format!(
        "version: 1\nkind: amqp\nname: a\noperation: consume\nbroker: 'amqp://127.0.0.1:{port}/%2f{query}'\nexchange: x\n"
    );
    let resolved = common::resolve(&text, "");
    let mut options = RunOptions::headless(&resolved);
    options.connect_timeout = Duration::from_secs(2);
    let _ = run(&resolved, options).await;
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_huge_server_frame_max_is_clamped_to_the_client_cap() {
    // Regression: with no client frame_max, lapin took the server's (16 MiB
    // here, u32::MAX for 0) and allocated 32 frames of it per buffer.
    assert_eq!(negotiated(16 << 20, "").await, FRAME_MAX);
    assert_eq!(negotiated(0, "").await, FRAME_MAX);
    assert_eq!(negotiated(16 << 20, "?frame_max=0").await, FRAME_MAX);
    assert_eq!(negotiated(16 << 20, "?frame_max=8192").await, 8192);
    assert_eq!(negotiated(10_000, "").await, 10_000);
}
