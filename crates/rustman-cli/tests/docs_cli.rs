//! The `rustman send` examples in docs/cli.md, run against the test server:
//! the ambiguous-name error, the default output, the failed-assertion line
//! and the ad-hoc requests whose URL starts with `{{base_url}}`; and the
//! `rustman run` examples, with the `_collection.yaml` that runs a folder's
//! requests concurrently.

mod common;

use std::fs;
use std::path::Path;

use common::{Server, request, rustman, unique, workspace, write};
use rustman_test_support::routes::timed_hits;

fn doc() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/cli.md");
    fs::read_to_string(path).unwrap()
}

/// The ```text block of the doc that contains `marker`, as lines.
fn block_with(doc: &str, marker: &str) -> Vec<String> {
    let mut lines = doc.lines();
    while let Some(line) = lines.next() {
        if line.starts_with("```") && line.len() > 3 {
            let body: Vec<String> = lines
                .by_ref()
                .take_while(|l| *l != "```")
                .map(str::to_owned)
                .collect();
            if body.iter().any(|l| l.contains(marker)) {
                return body;
            }
        }
    }
    panic!("no block with {marker:?} in docs/cli.md");
}

/// Whether every line of `expected` appears in `actual`, in order.
fn in_order(expected: &[String], actual: &str) -> bool {
    let mut rest = actual.lines();
    expected.iter().all(|want| rest.any(|got| got == want))
}

#[test]
fn the_ambiguous_name_error_is_the_documented_one() {
    let server = Server::http();
    let ws = workspace(&server);
    let list = request("List", "{{base_url}}/echo", "");
    write(ws.path(), "a/list.rman.yaml", &list);
    write(ws.path(), "b/list.rman.yaml", &list);
    let documented = block_with(&doc(), "is ambiguous");
    let output = rustman(ws.path()).args(["send", "List"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8(output.stderr).unwrap().trim_end(),
        documented.join("\n")
    );
}

#[test]
fn send_prints_the_documented_status_headers_and_body() {
    let server = Server::http();
    let ws = workspace(&server);
    write(
        ws.path(),
        "users/get-user.rman.yaml",
        &request("get-user", "{{base_url}}/users/42", ""),
    );
    let documented = block_with(&doc(), "HTTP/1.1 200");
    let output = rustman(ws.path())
        .args(["send", "get-user", "--env", "dev", "--no-history"])
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(in_order(&documented, &stdout), "{documented:#?}\n{stdout}");
}

#[test]
fn a_failed_assertion_prints_the_documented_line() {
    let server = Server::http();
    let ws = workspace(&server);
    write(
        ws.path(),
        "missing.rman.yaml",
        &request(
            "missing",
            "{{base_url}}/status/404",
            "assertions:\n  - status: 200\n",
        ),
    );
    assert!(doc().contains("(`fail: status: expected 200, got 404`)"));
    let output = rustman(ws.path())
        .args(["send", "missing", "--env", "dev", "--no-history"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("fail: status: expected 200, got 404\n"),
        "{stderr}"
    );
}

#[test]
fn the_templated_ad_hoc_examples_run() {
    let server = Server::http();
    let ws = workspace(&server);
    let documented = block_with(&doc(), "rustman send '{{base_url}}/users/42'");
    write(
        ws.path(),
        "environments/dev.yaml",
        &format!(
            "variables:\n  base_url: {}\nsecrets: [api_token]\n",
            server.url("")
        ),
    );
    rustman(ws.path())
        .args(["secret", "set", "api_token", "--env", "dev"])
        .write_stdin("t0k")
        .assert()
        .success();
    for line in &documented {
        let args = shell_words::split(line).unwrap();
        assert_eq!(args[0], "rustman");
        let output = rustman(ws.path())
            .args(&args[1..])
            .arg("--no-history")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(output.status.code(), Some(0), "{line}\n{output:?}");
        assert!(stdout.starts_with("HTTP/1.1 "), "{line}\n{stdout}");
    }
}

/// The fenced block of the doc whose first line contains `marker`.
fn fenced_block(doc: &str, marker: &str) -> Vec<String> {
    let block = block_with(doc, marker);
    assert!(block[0].contains(marker), "{block:#?}");
    block
}

#[test]
fn the_run_examples_run() {
    let server = Server::http();
    let ws = workspace(&server);
    let env = format!("variables:\n  base_url: {}\n", server.url(""));
    for name in ["staging", "ci"] {
        write(ws.path(), &format!("environments/{name}.yaml"), &env);
    }
    let smoke = request("list", "{{base_url}}/users/42", "tags: [smoke]\n");
    write(ws.path(), "api/users/list.rman.yaml", &smoke);
    let other = request("other", "{{base_url}}/echo", "");
    write(ws.path(), "api/other.rman.yaml", &other);
    let examples = fenced_block(&doc(), "rustman run api/users");
    assert_eq!(examples.len(), 3, "{examples:#?}");
    let mut executed = Vec::new();
    for line in &examples {
        let args = shell_words::split(line).unwrap();
        assert_eq!(args[0], "rustman");
        let output = rustman(ws.path())
            .args(&args[1..])
            .arg("--no-history")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0), "{line}\n{output:?}");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        executed.push(stdout.matches("PASS ").count());
    }
    // api/users (1), tag:smoke (1), everything (2).
    assert_eq!(executed, [1, 1, 2]);
    let report = fs::read(ws.path().join("out/report.json")).unwrap();
    let report: serde_json::Value = serde_json::from_slice(&report).unwrap();
    assert_eq!(report["summary"]["passed"], 2);
}

#[test]
fn the_parallel_collection_example_runs_requests_concurrently() {
    let server = Server::http();
    let ws = workspace(&server);
    let collection = fenced_block(&doc(), "# load/_collection.yaml");
    write(ws.path(), "load/_collection.yaml", &collection.join("\n"));
    let prefix = format!("{}-doc-par-", unique(ws.path()));
    for i in 1..=2 {
        let url = format!("{{{{base_url}}}}/timed/1500?id={prefix}{i}");
        write(
            ws.path(),
            &format!("load/{i}.rman.yaml"),
            &request(&format!("load-{i}"), &url, ""),
        );
    }
    rustman(ws.path())
        .args([
            "run",
            "load",
            "--env",
            "dev",
            "--parallel",
            "2",
            "--no-history",
        ])
        .assert()
        .success();
    let hits = timed_hits(&prefix);
    assert_eq!(hits.len(), 2);
    // Sequential requests would start after the other ended.
    let (a, b) = (&hits[0], &hits[1]);
    assert!(b.start < a.end.unwrap() && a.start < b.end.unwrap());
}
