//! Helpers shared by the JSON-RPC integration tests.

#![allow(dead_code)]

use std::time::Duration;

use rustman_core::model::{RequestSettings, RetrySettings};
use rustman_core::vars::{
    Resolved, ResolvedBody, ResolvedCall, ResolvedJsonrpc, ResolvedRequest, TlsFiles,
};
use rustman_http::SendOptions;
use serde_json::{Value, json};

/// A request to `url` with no headers, auth or settings.
pub fn endpoint(url: &str) -> ResolvedRequest {
    ResolvedRequest {
        method: "POST".to_owned(),
        url: Resolved::literal(url),
        query: Vec::new(),
        headers: Vec::new(),
        body: ResolvedBody::None,
        auth: None,
        settings: RequestSettings::default(),
        tls: TlsFiles::default(),
        secrets: Vec::new(),
    }
}

/// `calls` to `url`.
pub fn target(url: &str, calls: Vec<ResolvedCall>, batch: bool) -> ResolvedJsonrpc {
    ResolvedJsonrpc::new(endpoint(url), calls, batch)
}

pub fn call(method: &str) -> ResolvedCall {
    ResolvedCall::new(method, None)
}

/// An idempotent request retried `attempts` times, 10 ms apart at first.
pub fn idempotent(mut target: ResolvedJsonrpc, attempts: u32) -> ResolvedJsonrpc {
    target.idempotent = true;
    target.retry = RetrySettings {
        attempts: Some(attempts),
        backoff_ms: Some(10),
    };
    target
}

/// Options with a total timeout of `ms`.
pub fn timeout_ms(ms: u64) -> SendOptions {
    SendOptions {
        timeout: Duration::from_millis(ms),
        ..SendOptions::default()
    }
}

pub fn result(id: Value, value: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": value})
}

pub fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// A local port nothing listens on: bound, then released.
pub fn closed_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}
