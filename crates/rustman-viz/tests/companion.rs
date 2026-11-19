//! The `rustman-viz` executable with Raven's headless backend (tasks 3.3,
//! 3.4, 3.5): recorded sessions piped into it, the handshake, the
//! protocol check, the graphics failure exit code, and every layout
//! loading with all its slots. Needs the `raven` feature and the Raven
//! library (RAVEN_LIB_DIR); no window system or GPU.

#![cfg(feature = "raven")]

use std::io::Write;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_rustman-viz");

fn fixture(name: &str) -> String {
    format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

/// Run the companion headless with `input` on stdin; it must exit on its
/// own once stdin ends.
fn session(input: &str) -> Output {
    let mut child = Command::new(BIN)
        .env("RUSTMAN_VIZ_HEADLESS", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("the companion did not exit after stdin ended");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    child.wait_with_output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

const HELLO: &str = r#"{"type":"hello","protocol":"rustman-viz/1","producer":"rustman test"}"#;

#[test]
fn protocol_check_prints_the_protocol() {
    let out = Command::new(BIN).arg("--protocol-check").output().unwrap();
    assert!(out.status.success());
    assert_eq!(text(&out.stdout), "rustman-viz/1\n");
}

#[test]
fn bad_arguments_exit_2() {
    let out = Command::new(BIN).arg("--nope").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn no_graphics_writes_the_error_line_and_exits_3() {
    let out = Command::new(BIN)
        .env("RUSTMAN_VIZ_FAIL_GRAPHICS", "1")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    let line = text(&out.stdout);
    assert!(
        line.starts_with(r#"{"type":"error","code":"vulkan_unavailable""#),
        "{line}"
    );
}

#[test]
fn a_recorded_run_session_is_answered_and_shown() {
    let mut input = format!("{HELLO}\n");
    input.push_str(r#"{"type":"open_view","view":"run_dashboard","title":"api/"}"#);
    input.push('\n');
    input.push_str(r#"{"type":"run_started","started_at":"2026-11-19T10:00:00Z","total":200}"#);
    input.push('\n');
    for i in 0..200 {
        let (outcome, status) = match i {
            0..180 => ("passed", ",\"status\":200"),
            180..195 => ("failed", ",\"status\":422"),
            _ => ("errored", ""),
        };
        input.push_str(&format!(
            r#"{{"type":"exchange_completed","request":"r{}","method":"GET","url":"https://api.test/x","outcome":"{outcome}"{status},"duration_ms":{},"assertions":{{"passed":1,"failed":0}}}}"#,
            i % 4,
            10 + i % 30
        ));
        input.push('\n');
    }
    input.push_str(r#"{"type":"run_finished","state":"completed","summary":{"total":200,"passed":180,"failed":15,"errored":5,"skipped":0,"duration_ms":9000}}"#);
    input.push('\n');
    input.push_str(r#"{"type":"close"}"#);
    input.push('\n');
    let out = session(&input);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    assert!(
        stdout.starts_with(r#"{"type":"hello_ack","protocol":"rustman-viz/1""#),
        "{stdout}"
    );
    assert!(
        !text(&out.stderr).contains("cannot open"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn every_view_opens_with_all_its_slots() {
    let run = fixture("run-report.json");
    let exploration = fixture("exploration-report.json");
    let input = format!(
        "{HELLO}\n\
         {{\"type\":\"load_report\",\"path\":{run:?},\"kind\":\"run\",\"view\":\"run_dashboard\"}}\n\
         {{\"type\":\"load_report\",\"path\":{exploration:?},\"kind\":\"exploration\",\"view\":\"exploration_dashboard\"}}\n\
         {{\"type\":\"load_report\",\"path\":{exploration:?},\"kind\":\"exploration\",\"view\":\"api_graph\"}}\n\
         {{\"type\":\"inspect_response\",\"status\":200,\"body\":{{\"type\":\"inline\",\"text\":\"[{{\\\"id\\\":1}}]\"}}}}\n"
    );
    let out = session(&input);
    let stderr = text(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    // A layout that fails to load, or lacks a slot, is reported here.
    assert!(!stderr.contains("cannot open"), "{stderr}");
    assert!(!stderr.contains("lacks"), "{stderr}");
}

#[test]
fn a_rejected_producer_gets_hello_reject() {
    let out =
        session("{\"type\":\"hello\",\"protocol\":\"rustman-viz/2\",\"producer\":\"future\"}\n");
    assert!(out.status.success());
    assert_eq!(
        text(&out.stdout).trim(),
        r#"{"type":"hello_reject","supported":["rustman-viz/1"]}"#
    );
}

#[test]
fn an_unreadable_report_is_logged_and_the_companion_exits() {
    let input = format!(
        "{HELLO}\n{{\"type\":\"load_report\",\"path\":\"/nope/r.json\",\"kind\":\"run\"}}\n"
    );
    let out = session(&input);
    assert!(out.status.success());
    assert!(text(&out.stderr).contains("cannot read /nope/r.json"));
}
