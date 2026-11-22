//! A local Anvil for the signing tests (add-evm-toolkit tasks 5.3-5.7),
//! with the Counter fixture's runtime code installed by `anvil_setCode`.
//! Only started with `RUSTMAN_ANVIL=1` or `RUSTMAN_REQUIRE_ANVIL=1` (the CI
//! Anvil job, where a missing `anvil` fails the tests).
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustman_core::vars::{Resolved, ResolvedBody, ResolvedRequest, TlsFiles};
use rustman_evm::JsonrpcTransport;
use rustman_evm::fork::{Anvil, AnvilOptions};
use rustman_evm::rpc::call;
use rustman_http::{HttpEngine, SendOptions};
use serde_json::{Value, json};

/// The password of the test keystores.
pub const PASSWORD: &str = "rustman-test-password";

pub fn signing_fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../rustman-evm-signing/tests/fixtures")
}

/// `anvil` on `PATH` when the Anvil tests are enabled.
pub fn anvil_program() -> Option<PathBuf> {
    let require = std::env::var("RUSTMAN_REQUIRE_ANVIL").ok();
    let opt_in = std::env::var("RUSTMAN_ANVIL").ok();
    if require.as_deref() != Some("1") && opt_in.as_deref() != Some("1") {
        eprintln!("set RUSTMAN_ANVIL=1 to run the Anvil tests: skipping");
        return None;
    }
    let file = if cfg!(windows) { "anvil.exe" } else { "anvil" };
    let found = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(file))
            .find(|p| p.is_file())
    });
    assert!(
        found.is_some() || require.as_deref() != Some("1"),
        "anvil is required (RUSTMAN_REQUIRE_ANVIL=1) but not on PATH"
    );
    found
}

/// A running Anvil with the counter installed, and a runtime to talk to it.
pub struct Node {
    runtime: tokio::runtime::Runtime,
    anvil: Option<Anvil>,
    rpc: JsonrpcTransport,
}

impl Node {
    /// Anvil on a free port with the Counter at `counter`.
    pub fn start(program: &Path, counter: &str) -> Self {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let mut options = AnvilOptions::new(program.to_owned());
        options.port = 0;
        let anvil = runtime.block_on(Anvil::start(&options)).unwrap();
        let endpoint = ResolvedRequest {
            method: "POST".to_owned(),
            url: Resolved::literal(anvil.url()),
            query: Vec::new(),
            headers: Vec::new(),
            body: ResolvedBody::None,
            auth: None,
            settings: Default::default(),
            tls: TlsFiles::default(),
            secrets: Vec::new(),
        };
        let rpc = JsonrpcTransport::new(
            endpoint,
            Arc::new(HttpEngine::new()),
            SendOptions::default(),
        );
        let node = Self {
            runtime,
            anvil: Some(anvil),
            rpc,
        };
        let code = std::fs::read_to_string(signing_fixtures().join("counter/Counter.runtime.hex"))
            .unwrap();
        node.call("anvil_setCode", json!([counter, code.trim()]));
        node
    }

    pub fn url(&self) -> String {
        self.anvil.as_ref().unwrap().url()
    }

    pub fn call(&self, method: &str, params: Value) -> Value {
        self.runtime
            .block_on(call(&self.rpc, method, params))
            .unwrap()
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        if let Some(anvil) = self.anvil.take() {
            self.runtime.block_on(anvil.stop());
        }
    }
}
