//! `rustman report schema`, `render` and `diff`, and `--report` on `rustman
//! run` (add-openapi-explorer tasks 4.1, 4.4 and 4.5).

mod common;

use std::path::{Path, PathBuf};

use common::{Server, request, rustman, valid_report, workspace, write};

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// A report fixture of `rustman-report`.
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../rustman-report/tests/fixtures")
        .join(name)
}

#[test]
fn report_schema_prints_the_published_schema() {
    let dir = tempfile::tempdir().unwrap();
    let output = rustman(dir.path())
        .args(["report", "schema"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let published = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/schemas/report.schema.json"),
    )
    .unwrap()
    .replace("\r\n", "\n");
    assert_eq!(text(&output.stdout), published);
}

#[test]
fn an_unknown_major_version_exits_2_naming_it() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "it.json",
        r#"{"schema": "rustman.report/v9", "kind": "exploration"}"#,
    );
    for args in [
        &["report", "render", "it.json", "--format", "html"][..],
        &["report", "diff", "it.json", "it.json"][..],
    ] {
        let output = rustman(dir.path()).args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert_eq!(
            text(&output.stderr).trim_end(),
            "error: it.json: unsupported report schema 'rustman.report/v9' (this version reads rustman.report/v1)"
        );
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn render_writes_every_format_of_a_run_report() {
    let dir = tempfile::tempdir().unwrap();
    let source = fixture("run-v1.json");
    let source = source.to_str().unwrap();
    for format in ["json", "junit", "markdown", "html", "sarif"] {
        let out = format!("out/run.{format}");
        let output = rustman(dir.path())
            .args(["report", "render", source, "--format", format, "-o", &out])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{format}: {}",
            text(&output.stderr)
        );
        assert!(output.stdout.is_empty());
        let rendered = std::fs::read_to_string(dir.path().join(&out)).unwrap();
        assert!(rendered.contains("create"), "{format}");
    }
    let bad = rustman(dir.path())
        .args(["report", "render", source, "--format", "xml"])
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(2));
    assert!(text(&bad.stderr).contains("unknown report format 'xml'"));
}

#[test]
fn run_writes_two_report_formats_at_once() {
    let server = Server::http();
    let ws = workspace(&server);
    write(
        ws.path(),
        "a.rman.yaml",
        &request(
            "A",
            "{{base_url}}/status/404",
            "assertions:\n  - status: 200\n",
        ),
    );
    let output = rustman(ws.path())
        .args(["run", "--env", "dev", "--no-history"])
        .args(["--report", "json=out/r.json", "--report", "junit=out/r.xml"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr));
    // Without a --report on stdout, the pretty report still is.
    assert!(text(&output.stdout).contains("FAIL  a"));
    let json = std::fs::read(ws.path().join("out/r.json")).unwrap();
    let report = valid_report(&json);
    assert_eq!(report["requests"][0]["assertions"][0]["actual"], "404");
    let junit = std::fs::read_to_string(ws.path().join("out/r.xml")).unwrap();
    assert!(junit.contains("<failure"), "{junit}");

    let stdout = rustman(ws.path())
        .args([
            "run",
            "--env",
            "dev",
            "--no-history",
            "--report",
            "markdown",
        ])
        .output()
        .unwrap();
    assert!(text(&stdout.stdout).starts_with("# Run report"));
    let two = rustman(ws.path())
        .args([
            "run",
            "--env",
            "dev",
            "--reporter",
            "json",
            "--report",
            "sarif",
        ])
        .output()
        .unwrap();
    assert_eq!(two.status.code(), Some(2));
    assert!(text(&two.stderr).contains("only one reporter can write to stdout, got json, sarif"));
}

#[test]
fn diff_exits_1_on_a_new_finding_at_or_above_fail_on() {
    let dir = tempfile::tempdir().unwrap();
    let old = fixture("diff/old.json");
    let new = fixture("diff/new.json");
    let (old, new) = (old.to_str().unwrap(), new.to_str().unwrap());
    let output = rustman(dir.path())
        .args(["report", "diff", old, new])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let markdown = text(&output.stdout);
    assert!(
        markdown.contains("1 new, 1 fixed, 4 unchanged findings."),
        "{markdown}"
    );
    assert!(markdown.contains(
        "| critical | `broken-object-level-authorization` | `GET /orders/{id}` | f001 |"
    ));

    let json = rustman(dir.path())
        .args(["report", "diff", old, new, "--format", "json"])
        .output()
        .unwrap();
    let diff: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(diff["fixed"][0]["rule"], "server-error-on-invalid-input");
    assert_eq!(diff["coverage"]["operations"]["delta"], 25.0);

    // A fixed finding alone is no regression.
    let back = rustman(dir.path())
        .args(["report", "diff", new, old, "--fail-on", "critical"])
        .output()
        .unwrap();
    assert_eq!(back.status.code(), Some(0), "{}", text(&back.stdout));
    let run = fixture("run-v1.json");
    let mixed = rustman(dir.path())
        .args(["report", "diff", old, run.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(mixed.status.code(), Some(2));
    assert!(text(&mixed.stderr).contains("cannot compare an exploration report with a run report"));
}
