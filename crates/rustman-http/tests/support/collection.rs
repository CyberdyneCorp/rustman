//! Running the requests of a collection the way `rustman run` does, for
//! the assertion and capture tests.

use std::fs;
use std::path::Path;

use rustman_core::assertions::{self, AssertionResult, Context};
use rustman_core::captures::{self, CaptureReport, PersistedState, Persistence, RunCaptures};
use rustman_core::model::Request;
use rustman_core::vars::{Resolver, resolve_request};
use rustman_core::{Redactor, SecretStore, Workspace};
use rustman_http::{HttpEngine, SendOptions};

use super::TestServer;

/// What one request of a run produced.
pub struct Ran {
    pub name: String,
    pub assertions: Vec<AssertionResult>,
    pub captures: CaptureReport,
}

impl Ran {
    pub fn passed(&self) -> bool {
        self.assertions.iter().all(AssertionResult::passed) && !self.captures.failed()
    }
}

/// Run, in order, the requests whose file is under `folder`, as `rustman run`
/// does: resolve with earlier captures and persisted state, send, assert,
/// then capture.
pub async fn run(root: &Path, folder: &str, env: &str) -> Vec<Ran> {
    let ws = Workspace::load(root).unwrap();
    let environment = ws.active_environment(Some(env), None).unwrap();
    let store = SecretStore::with_keychain(root, None);
    let state = PersistedState::load(root, Some(env)).unwrap();
    let engine = HttpEngine::new();
    let mut scope = RunCaptures::new();
    let mut ran = Vec::new();
    for request in ws.requests() {
        if !request.entry.path.starts_with(folder) {
            continue;
        }
        let Request::Http(http) = request.request() else {
            unreachable!()
        };
        let mut resolver = Resolver::for_request(&ws.settings, &request, environment, &store);
        resolver.push(&scope);
        resolver.push(state.source());
        let resolved = resolve_request(&request, &resolver).unwrap();
        let options = SendOptions::for_request(&ws.settings, root, &resolved).unwrap();
        let exchange = engine.send(&resolved, &options).await.unwrap();
        let list = http.assertions.as_deref().unwrap_or_default();
        let view = exchange.view(true).unwrap();
        let redactor = Redactor::for_request(&resolved);
        let ctx = Context {
            resolver: &resolver,
            request: Some(&request),
            redactor: &redactor,
        };
        let results = assertions::evaluate(list, &view, &ctx);
        drop(resolver);
        let list = http.captures.as_deref().unwrap_or_default();
        let persistence = Persistence {
            root,
            environment,
            secrets: &store,
            redactor: &redactor,
        };
        let report = captures::apply(list, &view, &mut scope, Some(&persistence));
        ran.push(Ran {
            name: http.name.clone(),
            assertions: results,
            captures: report,
        });
    }
    ran
}

pub fn write(root: &Path, files: &[(&str, &str)]) {
    for (path, text) in files {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
}

pub fn workspace(server: &TestServer, secrets: &[&str]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let secrets = if secrets.is_empty() {
        String::new()
    } else {
        format!("secrets: [{}]\n", secrets.join(", "))
    };
    let env = format!("variables:\n  base_url: {}\n{secrets}", server.url(""));
    write(
        dir.path(),
        &[
            ("rustman.toml", "[workspace]\nname = \"t\"\nversion = 1\n"),
            ("environments/dev.yaml", &env),
            (".gitignore", ".rustman/\n"),
        ],
    );
    dir
}
