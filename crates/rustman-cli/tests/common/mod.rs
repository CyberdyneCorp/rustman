//! Helpers for the `send` and `run` tests: a workspace pointed at the
//! in-process test server, which runs on its own runtime while the test
//! drives the `rustman` binary. Nothing here leaves the loopback interface.
#![allow(dead_code)]

pub mod viz_stub;

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use rustman_test_support::TestServer;

/// The test server and the runtime it is served from.
pub struct Server {
    // Dropped before the runtime, which the server's task runs on.
    pub server: TestServer,
    _runtime: tokio::runtime::Runtime,
}

impl Server {
    pub fn http() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let server = runtime.block_on(TestServer::http());
        Self {
            server,
            _runtime: runtime,
        }
    }

    pub fn url(&self, path: &str) -> String {
        self.server.url(path)
    }
}

/// The data directory the tests' ad-hoc history goes to, never the user's.
pub fn test_data_dir() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("rustman-test-data-{}", std::process::id()))
}

/// `rustman` in `dir`, with the file secret store, a test data directory
/// and no environment from the caller's shell.
pub fn rustman(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("rustman").expect("rustman binary is built");
    cmd.current_dir(dir)
        .env("RUSTMAN_SECRET_STORE", "file")
        .env("RUSTMAN_DATA_DIR", test_data_dir())
        .env_remove("RUSTMAN_ENV")
        .env_remove("HTTP_PROXY")
        .env_remove("HTTPS_PROXY")
        .env_remove("ALL_PROXY")
        .env_remove("http_proxy")
        .env_remove("https_proxy")
        .env_remove("all_proxy");
    cmd
}

pub fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// A workspace whose `dev` environment sets `base_url` to `server`.
pub fn workspace(server: &Server) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    write(
        dir.path(),
        "environments/dev.yaml",
        &format!("variables:\n  base_url: {}\n", server.url("")),
    );
    write(dir.path(), ".gitignore", ".rustman/\n");
    dir
}

/// A request file named `name` that sends `GET <url>` with `extra` YAML
/// (assertions, tags...) appended.
pub fn request(name: &str, url: &str, extra: &str) -> String {
    format!("version: 1\nkind: http\nname: {name}\nurl: '{url}'\n{extra}")
}

/// A port nothing listens on: connecting to it is refused, and no other
/// test's server can take it.
pub fn closed_port() -> u16 {
    rustman_test_support::ports::closed_port()
}

/// A prefix for `/timed` ids that no other test uses.
pub fn unique(dir: &Path) -> String {
    dir.file_name().unwrap().to_string_lossy().into_owned()
}

/// The validator for the published report schema.
pub fn report_schema() -> jsonschema::Validator {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/schemas/report.schema.json"
    );
    let schema: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    jsonschema::validator_for(&schema).unwrap()
}

/// Parse `text` as a report and check it against the published schema.
pub fn valid_report(text: &[u8]) -> serde_json::Value {
    let report: serde_json::Value = serde_json::from_slice(text).expect("the report is JSON");
    let errors: Vec<String> = report_schema()
        .iter_errors(&report)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:#?}");
    report
}
