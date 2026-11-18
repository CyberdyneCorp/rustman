//! An exploration an agent started and cancelled over MCP (agent-mcp-interface
//! task 4.2) is left `cancelled` for the agent and interrupted on disk, and
//! `rustman explore resume` finishes it from the terminal without sending
//! any probe twice. Requests go only to the in-process bad API.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use common::{rustman, test_data_dir, write};
use rustman_test_support::bad_api::{BadApi, OPENAPI};
use serde_json::{Value, json};

struct Api {
    // Dropped before the runtime, which the server's task runs on.
    server: BadApi,
    _runtime: tokio::runtime::Runtime,
}

impl Api {
    fn start() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let server = runtime.block_on(BadApi::start());
        Self {
            server,
            _runtime: runtime,
        }
    }
}

/// A workspace whose `dev` environment points at `target`, at 2 requests
/// per second so the exploration is still running when it is cancelled.
fn workspace(target: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n\n[safety]\nrate = 2\nconcurrency = 1\n",
    );
    write(dir.path(), "api.yaml", OPENAPI);
    let env = format!("variables:\n  base_url: {target}\n");
    write(dir.path(), "environments/dev.yaml", &env);
    write(dir.path(), ".gitignore", ".rustman/\n");
    dir
}

/// Read stdout frames until the answer to request `id`.
fn answer(stdout: &mut BufReader<ChildStdout>, id: u64) -> Value {
    loop {
        let mut line = String::new();
        assert!(stdout.read_line(&mut line).unwrap() > 0, "stdout closed");
        let frame: Value = serde_json::from_str(&line).unwrap();
        if frame["id"] == id {
            return frame;
        }
    }
}

fn tool_call(id: u64, name: &str, arguments: Value) -> String {
    let frame = json!({
        "jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": {"name": name, "arguments": arguments}
    });
    format!("{frame}\n")
}

fn journal_lines(dir: &Path) -> Vec<Value> {
    std::fs::read_to_string(dir.join("journal.ndjson"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

#[test]
fn a_cancelled_agent_exploration_resumes_from_the_cli() {
    let api = Api::start();
    let target = api.server.url("");
    let ws = workspace(&target);
    let mut child = Command::new(assert_cmd::cargo::cargo_bin("rustman"))
        .args(["mcp", "serve", "--env", "dev"])
        .current_dir(ws.path())
        .env("RUSTMAN_SECRET_STORE", "file")
        .env("RUSTMAN_DATA_DIR", test_data_dir())
        .env_remove("RUSTMAN_ENV")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let initialize = json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "cli-test", "version": "0"}
        }
    });
    stdin
        .write_all(format!("{initialize}\n").as_bytes())
        .unwrap();
    answer(&mut stdout, 1);
    let initialized = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    stdin
        .write_all(format!("{initialized}\n").as_bytes())
        .unwrap();
    let args = json!({
        "spec": "api.yaml", "target": target,
        "probes": ["conformance", "auth", "fuzz"], "fuzz_cases": 8, "seed": 3,
    });
    stdin
        .write_all(tool_call(2, "explore_run", args).as_bytes())
        .unwrap();
    let started = answer(&mut stdout, 2);
    let started = &started["result"]["structuredContent"];
    let job = started["job_id"].as_str().expect("a job id").to_owned();
    let id = started["exploration_id"].as_str().unwrap().to_owned();
    let dir: PathBuf = ws.path().join(".rustman").join("explorations").join(&id);

    let deadline = Instant::now() + Duration::from_secs(60);
    while api.server.hits().len() < 3 {
        assert!(Instant::now() < deadline, "the exploration did not start");
        std::thread::sleep(Duration::from_millis(50));
    }
    stdin
        .write_all(tool_call(3, "explore_cancel", json!({"job_id": job})).as_bytes())
        .unwrap();
    let cancelled = answer(&mut stdout, 3);
    let status = &cancelled["result"]["structuredContent"];
    assert_eq!(status["state"], "cancelled", "{cancelled:#}");
    let done = status["done"].as_u64().unwrap();
    assert!(done < status["total"].as_u64().unwrap(), "{status:#}");
    drop(stdin);
    assert!(child.wait().unwrap().success());

    let shown = rustman(ws.path())
        .args(["explore", "status", &id])
        .output()
        .unwrap();
    let shown = String::from_utf8_lossy(&shown.stdout).into_owned();
    assert!(shown.contains("(interrupted)"), "{shown}");

    let resumed = rustman(ws.path())
        .args(["explore", "resume", &id])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&resumed.stderr);
    // Exit 1: the bad API has findings at the default fail-on level.
    assert_eq!(resumed.status.code(), Some(1), "{stderr}");
    let lines = journal_lines(&dir);
    let sent: usize = lines
        .iter()
        .map(|l| l["sent"].as_array().map_or(0, Vec::len))
        .sum();
    assert_eq!(api.server.hits().len(), sent, "no probe was sent twice");
    let plan: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("plan.json")).unwrap()).unwrap();
    assert_eq!(lines.len(), plan["probes"].as_array().unwrap().len());
    assert!(dir.join("report.json").is_file());
}
