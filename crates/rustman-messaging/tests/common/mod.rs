//! A temporary workspace holding one messaging request, resolved with an
//! environment whose `kafka_pw` secret comes from a test store. The
//! in-process MQTT broker is `rustman_test_support::mqtt_broker`.

#![allow(dead_code, reason = "each test binary uses a subset")]

use std::fs;
use std::path::Path;

use rustman_core::Workspace;
use rustman_core::vars::{ResolvedMessaging, Resolver, SecretLookup, resolve_messaging};

/// The value of the `kafka_pw` secret in the `dev` environment.
pub const KAFKA_PW: &str = "kafka-canary-pw-5f3a";

struct Store(String);

impl SecretLookup for Store {
    fn secret(&self, env: &str, name: &str) -> Result<Option<String>, String> {
        Ok((env == "dev" && name == "kafka_pw").then(|| self.0.clone()))
    }
}

/// A workspace that lives as long as the fixture, so files the request
/// refers to (TLS material, payloads, documents) are still there when it
/// runs.
pub struct Fixture {
    pub dir: tempfile::TempDir,
}

impl Fixture {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("rustman.toml"),
            "[workspace]\nname = \"t\"\nversion = 1\n",
        )
        .unwrap();
        fs::create_dir_all(dir.path().join("environments")).unwrap();
        Self { dir }
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    /// Write `text` at `path`, relative to the workspace root.
    pub fn write(&self, path: &str, text: &str) {
        let full = self.root().join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, text).unwrap();
    }

    /// Resolve the request file `text` (saved as `r.rman.yaml`) with
    /// `variables` (YAML mapping entries) and `kafka_pw` = `password`.
    pub fn resolve(&self, text: &str, variables: &str, password: &str) -> ResolvedMessaging {
        self.write(
            "environments/dev.yaml",
            &format!("variables:\n  unused: x\n{variables}secrets: [kafka_pw]\n"),
        );
        self.write("r.rman.yaml", text);
        let ws = Workspace::load(self.root()).unwrap();
        let requests = ws.requests();
        let request = requests.iter().find(|r| r.entry.id() == "r").unwrap();
        let env = ws.active_environment(Some("dev"), None).unwrap();
        let store = Store(password.to_owned());
        let resolver = Resolver::for_request(&ws.settings, request, env, &store);
        resolve_messaging(request, &resolver).unwrap()
    }
}

/// Resolve the request file `text` with `variables` (YAML mapping
/// entries) and `kafka_pw` = `password`.
pub fn resolve_with(text: &str, variables: &str, password: &str) -> ResolvedMessaging {
    Fixture::new().resolve(text, variables, password)
}

pub fn resolve(text: &str, variables: &str) -> ResolvedMessaging {
    resolve_with(text, variables, KAFKA_PW)
}
