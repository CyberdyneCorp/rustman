//! `rustman explore resume` and Ctrl+C (task 3.8): a resume sends only the
//! probes the journal does not hold, refuses a changed document, and (on
//! Unix) SIGINT stops a run with exit code 130 that `resume` then finishes.

mod common;

use std::path::{Path, PathBuf};

use common::{rustman, write};
use rustman_test_support::bad_api::{BadApi, OPENAPI, TOKEN_A};
use serde_json::Value;

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

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    write(dir.path(), "api.yaml", OPENAPI);
    write(
        dir.path(),
        "environments/dev.yaml",
        "secrets: [bearerAuth_token]\n",
    );
    rustman(dir.path())
        .args(["secret", "set", "bearerAuth_token", "--env", "dev"])
        .write_stdin(TOKEN_A)
        .assert()
        .success();
    dir
}

fn run_args(target: &str, rate: &str) -> Vec<String> {
    [
        "explore",
        "run",
        "--spec",
        "api.yaml",
        "--target",
        target,
        "--env",
        "dev",
        "--seed",
        "3",
        "--probes",
        "conformance,auth,fuzz",
        "--fuzz-cases",
        "8",
        "--rate",
        rate,
    ]
    .map(str::to_owned)
    .to_vec()
}

fn exploration(dir: &Path) -> PathBuf {
    let root = dir.join(".rustman").join("explorations");
    let found: Vec<PathBuf> = std::fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(found.len(), 1);
    found[0].clone()
}

fn id_of(dir: &Path) -> String {
    exploration(dir)
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

fn journal(dir: &Path) -> Vec<Value> {
    std::fs::read_to_string(exploration(dir).join("journal.ndjson"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// Requests the journal lines record.
fn sent(lines: &[Value]) -> usize {
    lines
        .iter()
        .map(|l| l["sent"].as_array().map_or(0, Vec::len))
        .sum()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn resume(dir: &Path, id: &str) -> std::process::Output {
    rustman(dir)
        .args(["explore", "resume", id])
        .output()
        .unwrap()
}

#[test]
fn resume_sends_only_the_probes_not_in_the_journal() {
    let api = Api::start();
    let ws = workspace();
    let target = api.server.url("");
    let first = rustman(ws.path())
        .args(run_args(&target, "1000"))
        .output()
        .unwrap();
    assert_eq!(first.status.code(), Some(1), "{}", text(&first.stderr));
    let dir = exploration(ws.path());
    let id = id_of(ws.path());
    let lines = journal(ws.path());
    let total = lines.len();
    assert!(total > 10, "{total}");
    let (kept, dropped) = lines.split_at(10);
    let text_lines: Vec<String> = kept.iter().map(Value::to_string).collect();
    std::fs::write(dir.join("journal.ndjson"), text_lines.join("\n") + "\n").unwrap();
    let state_path = dir.join("state.json");
    let mut state: Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
    state["state"] = "interrupted".into();
    state["phase"] = "execute".into();
    std::fs::write(&state_path, state.to_string()).unwrap();
    std::fs::remove_file(dir.join("report.json")).unwrap();
    let before = api.server.hits().len();

    std::fs::write(
        ws.path().join("api.yaml"),
        format!("{OPENAPI}\n# changed\n"),
    )
    .unwrap();
    let changed = resume(ws.path(), &id);
    assert_eq!(changed.status.code(), Some(2));
    assert!(
        text(&changed.stderr).contains("changed since exploration"),
        "{}",
        text(&changed.stderr)
    );
    assert_eq!(api.server.hits().len(), before, "nothing sent");

    std::fs::write(ws.path().join("api.yaml"), OPENAPI).unwrap();
    let resumed = resume(ws.path(), &id);
    assert_eq!(resumed.status.code(), Some(1), "{}", text(&resumed.stderr));
    assert_eq!(api.server.hits().len() - before, sent(dropped));
    let report: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("report.json")).unwrap()).unwrap();
    assert_eq!(report["summary"]["probes"]["total"], total);
    assert_eq!(journal(ws.path()).len(), total);

    let again = resume(ws.path(), &id);
    assert_eq!(again.status.code(), Some(2), "a completed exploration");
    let unknown = resume(ws.path(), "exp-20000101-000000");
    assert_eq!(unknown.status.code(), Some(2));
}

#[cfg(unix)]
#[test]
fn ctrl_c_exits_130_and_resume_finishes_the_exploration() {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let api = Api::start();
    let ws = workspace();
    let target = api.server.url("");
    let mut child = Command::new(assert_cmd::cargo::cargo_bin("rustman"))
        .args(run_args(&target, "20"))
        .current_dir(ws.path())
        .env("RUSTMAN_SECRET_STORE", "file")
        .env("RUSTMAN_DATA_DIR", common::test_data_dir())
        .env_remove("RUSTMAN_ENV")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let explorations = ws.path().join(".rustman").join("explorations");
    while !explorations.is_dir() || journal(ws.path()).len() < 5 {
        assert!(Instant::now() < deadline, "the run did not start");
        assert!(child.try_wait().unwrap().is_none(), "the run ended early");
        std::thread::sleep(Duration::from_millis(50));
    }
    let id = id_of(ws.path());
    let status = rustman(ws.path())
        .args(["explore", "status", &id])
        .output()
        .unwrap();
    let shown = text(&status.stdout);
    assert!(
        shown.contains("(running)") && shown.contains("phase: execute"),
        "{shown}"
    );

    let killed = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let output = child.wait_with_output().unwrap();
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(130), "{stderr}");
    assert!(
        stderr.contains(&format!("rustman explore resume {id}")),
        "{stderr}"
    );
    let done = journal(ws.path()).len();
    let status = rustman(ws.path())
        .args(["explore", "status", &id])
        .output()
        .unwrap();
    let shown = text(&status.stdout);
    assert!(
        shown.contains("(interrupted)") && shown.contains("phase: execute"),
        "{shown}"
    );
    assert!(shown.contains(&format!("probes: {done}/")), "{shown}");

    let resumed = rustman(ws.path())
        .args(["explore", "resume", &id, "--workspace", "."])
        .output()
        .unwrap();
    assert_eq!(resumed.status.code(), Some(1), "{}", text(&resumed.stderr));
    let lines = journal(ws.path());
    assert_eq!(
        api.server.hits().len(),
        sent(&lines),
        "no probe was sent twice"
    );
    let plan: Value = serde_json::from_str(
        &std::fs::read_to_string(exploration(ws.path()).join("plan.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(lines.len(), plan["probes"].as_array().unwrap().len());
    assert!(lines.len() > done);
}
