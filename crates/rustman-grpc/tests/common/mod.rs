//! A workspace fixture for the gRPC integration tests.
#![allow(dead_code)]

use std::fs;
use std::path::Path;

use std::time::Duration;

use prost_reflect::DescriptorPool;
use rustman_core::Workspace;
use rustman_core::vars::{NoSecrets, ResolveError, ResolvedGrpc, Resolver, resolve_grpc};
use rustman_grpc::{
    CallOptions, GrpcError, GrpcExchange, PreparedCall, Remote, Schema, SchemaOrigin, StreamBounds,
};
use rustman_http::SendOptions;
use rustman_test_support::grpc::{descriptor_set, protos_dir};

/// The test schema, as a descriptor set.
pub fn schema() -> Schema {
    let pool = DescriptorPool::decode(descriptor_set().as_slice()).unwrap();
    Schema::new(pool, SchemaOrigin::DescriptorSet)
}

/// A `kind: grpc` request file named `r` for `target` and `method`, with
/// `rest` (YAML lines) appended.
pub fn request_file(target: &str, method: &str, rest: &str) -> String {
    format!("version: 1\nkind: grpc\nname: r\ntarget: '{target}'\nmethod: {method}\n{rest}")
}

pub struct Fixture {
    pub dir: tempfile::TempDir,
}

impl Fixture {
    pub fn new() -> Self {
        let fixture = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        fixture.write("rustman.toml", "[workspace]\nname = \"t\"\nversion = 1\n");
        fixture
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    pub fn write(&self, rel: &str, text: &str) {
        let path = self.root().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// Copy the test protos to `<rel>/shop/v1/*.proto`.
    pub fn copy_protos(&self, rel: &str) {
        for name in ["catalog.proto", "common.proto"] {
            let text = fs::read_to_string(protos_dir().join("shop/v1").join(name)).unwrap();
            self.write(&format!("{rel}/shop/v1/{name}"), &text);
        }
    }

    pub fn resolve(&self, name: &str) -> Result<ResolvedGrpc, ResolveError> {
        let ws = Workspace::load(self.root()).unwrap();
        let request = &ws.find_by_name(name)[0];
        let env = ws.active_environment(None, None).unwrap();
        let resolver = Resolver::for_request(&ws.settings, request, env, &NoSecrets);
        resolve_grpc(request, &resolver)
    }
}

impl Fixture {
    /// The transport options of request `name` (TLS files, defaults).
    pub fn send_options(&self, resolved: &ResolvedGrpc) -> SendOptions {
        let ws = Workspace::load(self.root()).unwrap();
        SendOptions::for_request(&ws.settings, self.root(), &resolved.request).unwrap()
    }

    /// Write `text` as `r.rman.yaml`, then prepare it against the test
    /// schema.
    pub fn prepare(&self, text: &str) -> Result<(PreparedCall, CallOptions), GrpcError> {
        self.write("r.rman.yaml", text);
        let resolved = self.resolve("r").unwrap();
        let http = self.send_options(&resolved);
        let remote = Remote::for_request(&resolved)?.with_tls_settings(http.tls.clone());
        let call = PreparedCall::new(&resolved, &schema(), remote)?;
        let deadline = rustman_grpc::effective_deadline(resolved.deadline_ms, None);
        let options = CallOptions {
            deadline,
            bounds: Some(StreamBounds::of(&resolved.stream)),
            http,
        };
        Ok((call, options))
    }

    /// Prepare and send `text`, headless (stream bounds on).
    pub async fn send(&self, text: &str) -> Result<GrpcExchange, GrpcError> {
        let (call, options) = self.prepare(text)?;
        call.send(&options).await
    }
}

/// Wait up to 5 s for `check` to hold.
pub async fn eventually(check: impl Fn() -> bool) -> bool {
    for _ in 0..500 {
        if check() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    check()
}
