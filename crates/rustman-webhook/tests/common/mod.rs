//! A listener on a free loopback port and a raw TCP client, so tests send
//! exactly the bytes they mean (including malformed ones).

#![allow(dead_code)]

use std::net::SocketAddr;
use std::time::Duration;

use rustman_webhook::{
    CaptureStore, Event, Limits, Listener, ListenerConfig, ListenerHandle, Mocks,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::broadcast;

/// Generous bound for anything that should happen "soon".
pub const SOON: Duration = Duration::from_secs(10);

pub struct Running {
    pub handle: ListenerHandle,
    pub store: CaptureStore,
    pub mocks: Mocks,
    pub events: broadcast::Receiver<Event>,
}

impl Running {
    pub fn addr(&self) -> SocketAddr {
        self.handle.local_addr()
    }

    /// The next event matching `want`, skipping others.
    pub async fn next_event(&mut self, want: impl Fn(&Event) -> bool) -> Event {
        let wait = async {
            loop {
                match self.events.recv().await {
                    Ok(event) if want(&event) => return event,
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => panic!("event channel closed"),
                }
            }
        };
        tokio::time::timeout(SOON, wait)
            .await
            .expect("no such event in time")
    }
}

pub fn config(limits: Limits) -> ListenerConfig {
    ListenerConfig {
        port: 0,
        limits,
        ..ListenerConfig::default()
    }
}

pub async fn start_with(limits: Limits, mocks: Mocks) -> Running {
    let store = CaptureStore::default();
    let events = store.subscribe();
    let listener = Listener::bind(&config(limits), store.clone(), mocks.clone())
        .await
        .unwrap();
    Running {
        handle: listener.spawn(),
        store,
        mocks,
        events,
    }
}

pub async fn start() -> Running {
    start_with(Limits::default(), Mocks::empty()).await
}

/// Send `request` and read the reply until the server closes.
pub async fn raw(addr: SocketAddr, request: &[u8]) -> String {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request).await.unwrap();
    read_to_close(&mut stream).await
}

/// Everything the server sends until it closes (a reset ends it too).
pub async fn read_to_close(stream: &mut TcpStream) -> String {
    let mut out = Vec::new();
    let mut chunk = [0u8; 8192];
    let read = async {
        while let Ok(n) = stream.read(&mut chunk).await {
            if n == 0 {
                break;
            }
            out.extend_from_slice(&chunk[..n]);
        }
    };
    tokio::time::timeout(SOON, read)
        .await
        .expect("server did not close");
    String::from_utf8_lossy(&out).into_owned()
}

/// The status code of a raw HTTP/1 response.
pub fn status(response: &str) -> u16 {
    response
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("not a response: {response:?}"))
}

/// The body of a raw HTTP/1 response with a `Content-Length`.
pub fn body(response: &str) -> &str {
    response.split_once("\r\n\r\n").map_or("", |(_, b)| b)
}

/// The value of header `name` in a raw response.
pub fn header<'a>(response: &'a str, name: &str) -> Option<&'a str> {
    let head = response.split_once("\r\n\r\n").map_or(response, |(h, _)| h);
    head.lines().skip(1).find_map(|line| {
        let (n, v) = line.split_once(':')?;
        n.eq_ignore_ascii_case(name).then(|| v.trim())
    })
}
