//! Helpers shared by the rustman-evm integration tests.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use rustman_core::model::RequestSettings;
use rustman_core::vars::{Resolved, ResolvedBody, ResolvedRequest, TlsFiles};
use rustman_evm::JsonrpcTransport;
use rustman_http::{HttpEngine, SendOptions};
use rustman_test_support::jsonrpc::RpcReply;
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

/// Calls to `url` through the JSON-RPC client.
pub fn transport(url: &str) -> JsonrpcTransport {
    JsonrpcTransport::new(
        endpoint(url),
        Arc::new(HttpEngine::new()),
        SendOptions::default(),
    )
}

pub fn result(request: &Value, result: Value) -> RpcReply {
    RpcReply::json(&json!({"jsonrpc": "2.0", "id": request["id"], "result": result}))
}

pub fn error(request: &Value, code: i64, message: &str) -> RpcReply {
    RpcReply::json(&json!({
        "jsonrpc": "2.0", "id": request["id"],
        "error": {"code": code, "message": message}
    }))
}

/// A reverted call, as Geth and Anvil report it: code 3 with the revert
/// data.
pub fn revert(request: &Value, data: &str) -> RpcReply {
    RpcReply::json(&json!({
        "jsonrpc": "2.0", "id": request["id"],
        "error": {"code": 3, "message": "execution reverted", "data": data}
    }))
}

pub fn fixture(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(path)
}

/// `RUSTMAN_REQUIRE_ANVIL=1` (the CI Anvil job) runs the Anvil tests and
/// turns a missing Foundry tool into a failure instead of a skipped test.
pub const REQUIRE_ANVIL: &str = "RUSTMAN_REQUIRE_ANVIL";

/// `RUSTMAN_ANVIL=1` opts a local run into the Anvil tests; without it
/// (or `RUSTMAN_REQUIRE_ANVIL=1`) they skip even when Foundry is installed,
/// so a plain `cargo test` never starts Anvil.
pub const OPT_IN_ANVIL: &str = "RUSTMAN_ANVIL";

/// Whether the Anvil tests run, from the two switches' values.
pub fn anvil_enabled(require: Option<&str>, opt_in: Option<&str>) -> bool {
    require == Some("1") || opt_in == Some("1")
}

/// The Foundry tool `name` (`anvil`, `cast`) on `PATH`, or `None` when the
/// Anvil tests are not enabled, or when it is absent and not required.
pub fn foundry_tool(name: &str) -> Option<PathBuf> {
    let require = std::env::var(REQUIRE_ANVIL).ok();
    let opt_in = std::env::var(OPT_IN_ANVIL).ok();
    if !anvil_enabled(require.as_deref(), opt_in.as_deref()) {
        eprintln!("set {OPT_IN_ANVIL}=1 to run the Anvil tests: skipping");
        return None;
    }
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    let found = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(&file))
            .find(|p| p.is_file())
    });
    if found.is_none() {
        assert!(
            require.as_deref() != Some("1"),
            "{name} is required ({REQUIRE_ANVIL}=1) but not on PATH"
        );
        eprintln!("{name} is not on PATH: skipping");
    }
    found
}
