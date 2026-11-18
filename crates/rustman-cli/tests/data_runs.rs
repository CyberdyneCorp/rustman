//! Data-driven runs (runner parity tasks 2.1-2.6): `rustman run --data`
//! with CSV and JSON rows, a collection's default data file, `--no-data`,
//! the variable precedence of rows, independent iterations, `--bail`
//! across iterations, iteration labels and secret columns. Requests go to
//! the in-process test server; `/timed` hits show what reached it.

mod common;

use std::fs;
use std::path::Path;

use common::{Server, closed_port, request, rustman, unique, valid_report, workspace, write};
use rustman_test_support::routes::timed_hits;
use serde_json::Value;

/// `rustman run <args> --reporter json` in `root`: exit code and report.
fn run_json(root: &Path, args: &[&str]) -> (Option<i32>, Value) {
    let output = rustman(root)
        .arg("run")
        .args(args)
        .args(["--reporter", "json", "--no-history"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.stdout.is_empty(), "no report: {stderr}");
    (output.status.code(), valid_report(&output.stdout))
}

fn requests(report: &Value) -> &Vec<Value> {
    report["requests"].as_array().unwrap()
}

/// `(id, iteration index, status)` of every request of `report`.
fn outcomes(report: &Value) -> Vec<(String, u64, String)> {
    requests(report)
        .iter()
        .map(|r| {
            (
                r["id"].as_str().unwrap().to_owned(),
                r["data_row"]["index"].as_u64().unwrap_or(0),
                r["status"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

/// The `/timed` ids that reached the server with `prefix`, without it.
fn hits(prefix: &str) -> Vec<String> {
    timed_hits(prefix)
        .iter()
        .map(|h| h.id.strip_prefix(prefix).unwrap().to_owned())
        .collect()
}

/// A request named `name` hitting `/timed/1` with id `<prefix><id>`.
fn timed(prefix: &str, name: &str, id: &str, extra: &str) -> String {
    request(
        name,
        &format!("{{{{base_url}}}}/timed/1?id={prefix}{id}"),
        extra,
    )
}

fn header_value(record: &Value, name: &str) -> String {
    record["exchange"]["request"]["headers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["name"] == name)
        .map(|h| h["value"].as_str().unwrap().to_owned())
        .unwrap_or_default()
}

#[test]
fn one_iteration_per_csv_row_in_row_order() {
    let server = Server::http();
    let ws = workspace(&server);
    let prefix = unique(ws.path());
    for (file, name) in [("1-get", "get"), ("2-put", "put")] {
        let id = format!("{name}-{{{{email}}}}");
        write(
            ws.path(),
            &format!("api/users/{file}.rman.yaml"),
            &timed(&prefix, name, &id, ""),
        );
    }
    write(
        ws.path(),
        "users.csv",
        "email,role\nana@example.com,admin\nbo@example.com,user\ncy@example.com,user\n",
    );
    let (code, report) = run_json(
        ws.path(),
        &["api/users", "--env", "dev", "--data", "users.csv"],
    );
    assert_eq!(code, Some(0), "{report:#}");
    assert_eq!(
        hits(&prefix),
        [
            "get-ana@example.com",
            "put-ana@example.com",
            "get-bo@example.com",
            "put-bo@example.com",
            "get-cy@example.com",
            "put-cy@example.com",
        ]
    );
    let records = requests(&report);
    assert_eq!(records.len(), 6);
    assert_eq!(
        records[2]["iteration"], 2,
        "iteration keeps its v0.1 integer"
    );
    assert_eq!(records[2]["data_row"]["index"], 2);
    assert_eq!(records[2]["data_row"]["total"], 3);
    assert_eq!(records[2]["data_row"]["label"], "row 2");
}

#[test]
fn json_rows_resolve_scalars_and_nested_values() {
    let server = Server::http();
    let ws = workspace(&server);
    write(
        ws.path(),
        "case.rman.yaml",
        &request(
            "case",
            "{{base_url}}/echo",
            "headers:\n  - name: X-Case\n    value: '{{id}} {{tags}}'\n",
        ),
    );
    write(
        ws.path(),
        "cases.json",
        r#"[{"id": 1, "tags": ["a"]}, {"id": 2, "tags": []}]"#,
    );
    let (code, report) = run_json(ws.path(), &["--env", "dev", "--data", "cases.json"]);
    assert_eq!(code, Some(0), "{report:#}");
    let records = requests(&report);
    assert_eq!(header_value(&records[0], "X-Case"), r#"1 ["a"]"#);
    assert_eq!(header_value(&records[1], "X-Case"), "2 []");
}

/// A folder whose `_collection.yaml` names `tenants.csv` (4 rows) and sets
/// `tenant: default`, with one request sending `{{tenant}}`.
fn tenants(server: &Server) -> tempfile::TempDir {
    let ws = workspace(server);
    write(
        ws.path(),
        "api/tenants/_collection.yaml",
        "data: tenants.csv\nvariables:\n  tenant: default\n",
    );
    write(
        ws.path(),
        "api/tenants/tenants.csv",
        "tenant\nacme\nglobex\ninitech\numbrella\n",
    );
    write(
        ws.path(),
        "api/tenants/list.rman.yaml",
        &request(
            "list",
            "{{base_url}}/echo",
            "headers:\n  - name: X-Tenant\n    value: '{{tenant}}'\n",
        ),
    );
    ws
}

#[test]
fn a_collection_default_data_file_runs_once_per_row_and_rows_beat_collection_variables() {
    let server = Server::http();
    let ws = tenants(&server);
    let (code, report) = run_json(ws.path(), &["api/tenants", "--env", "dev"]);
    assert_eq!(code, Some(0), "{report:#}");
    let records = requests(&report);
    assert_eq!(records.len(), 4);
    let sent: Vec<String> = records
        .iter()
        .map(|r| header_value(r, "X-Tenant"))
        .collect();
    assert_eq!(sent, ["acme", "globex", "initech", "umbrella"]);
    // `--data` replaces the default.
    write(ws.path(), "one.csv", "tenant\nsolo\n");
    let (_, report) = run_json(
        ws.path(),
        &["api/tenants", "--env", "dev", "--data", "one.csv"],
    );
    assert_eq!(header_value(&requests(&report)[0], "X-Tenant"), "solo");
}

#[test]
fn no_data_runs_the_folder_once_without_row_variables() {
    let server = Server::http();
    let ws = tenants(&server);
    let (code, report) = run_json(ws.path(), &["api/tenants", "--env", "dev", "--no-data"]);
    assert_eq!(code, Some(0), "{report:#}");
    let records = requests(&report);
    assert_eq!(records.len(), 1);
    assert_eq!(header_value(&records[0], "X-Tenant"), "default");
    assert_eq!(records[0]["iteration"], 1);
    assert!(records[0].get("data_row").is_none());
}

#[test]
fn a_collection_data_file_outside_the_workspace_is_refused() {
    let server = Server::http();
    let ws = tenants(&server);
    write(
        ws.path(),
        "api/tenants/_collection.yaml",
        "data: ../../../outside.csv\n",
    );
    rustman(ws.path())
        .args(["run", "api/tenants", "--env", "dev"])
        .assert()
        .code(2)
        .stderr(
            "error: api/tenants/_collection.yaml: data: file path escapes workspace: ../../../outside.csv\n",
        );
}

#[test]
fn a_row_overrides_the_environment() {
    let server = Server::http();
    let ws = workspace(&server);
    let closed = closed_port();
    write(
        ws.path(),
        "environments/dev.yaml",
        &format!("variables:\n  base_url: http://127.0.0.1:{closed}\n"),
    );
    write(
        ws.path(),
        "ping.rman.yaml",
        &request(
            "ping",
            "{{base_url}}/echo",
            "assertions:\n  - status: 200\n",
        ),
    );
    write(
        ws.path(),
        "hosts.csv",
        &format!("base_url\n{}\n", server.url("")),
    );
    let (code, report) = run_json(ws.path(), &["--env", "dev", "--data", "hosts.csv"]);
    assert_eq!(code, Some(0), "{report:#}");
    let url = requests(&report)[0]["exchange"]["request"]["url"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(url, server.url("/echo"));
    assert!(!url.contains(&format!(":{closed}/")), "{url}");
}

#[test]
fn a_capture_overrides_the_row_within_its_iteration() {
    let server = Server::http();
    let ws = workspace(&server);
    write(
        ws.path(),
        "flow/1-login.rman.yaml",
        &request(
            "login",
            "{{base_url}}/token",
            "captures:\n  - var: token\n    from: json\n    path: $.access_token\n",
        ),
    );
    write(
        ws.path(),
        "flow/2-me.rman.yaml",
        &request(
            "me",
            "{{base_url}}/me",
            "auth:\n  type: bearer\n  token: '{{token}}'\nassertions:\n  - status: 200\n",
        ),
    );
    write(ws.path(), "rows.csv", "token\nplaceholder\nplaceholder\n");
    let (code, report) = run_json(ws.path(), &["flow", "--env", "dev", "--data", "rows.csv"]);
    assert_eq!(code, Some(0), "{report:#}");
    assert_eq!(report["summary"]["passed"], 4);
}

#[test]
fn iterations_are_independent() {
    let server = Server::http();
    let ws = workspace(&server);
    write(
        ws.path(),
        "flow/1-use.rman.yaml",
        &request("use", "{{base_url}}/echo?order={{order_id}}", ""),
    );
    write(
        ws.path(),
        "flow/2-create.rman.yaml",
        &request(
            "create",
            "{{base_url}}/token",
            "captures:\n  - var: order_id\n    from: json\n    path: $.user.id\n",
        ),
    );
    write(ws.path(), "rows.csv", "n\n1\n2\n");
    let (code, report) = run_json(ws.path(), &["flow", "--env", "dev", "--data", "rows.csv"]);
    assert_eq!(code, Some(3), "an errored request exits with 3: {report:#}");
    let records = requests(&report);
    for index in [0, 2] {
        assert_eq!(records[index]["status"], "errored", "{report:#}");
        assert_eq!(
            records[index]["error"],
            "unresolved variable(s): order_id",
            "iteration {}",
            index / 2 + 1
        );
    }
    assert_eq!(records[3]["status"], "passed");
}

/// Three rows; `check` fails for row 2 (`/status/500`), then `after` hits
/// `/timed` with the row's number.
fn failing_row(server: &Server) -> (tempfile::TempDir, String) {
    let ws = workspace(server);
    let prefix = unique(ws.path());
    write(
        ws.path(),
        "rows/1-check.rman.yaml",
        &request(
            "check",
            "{{base_url}}/status/{{code}}",
            "assertions:\n  - status: 200\n",
        ),
    );
    write(
        ws.path(),
        "rows/2-after.rman.yaml",
        &timed(&prefix, "after", "after-{{n}}", ""),
    );
    write(ws.path(), "rows.csv", "n,code\n1,200\n2,500\n3,200\n");
    (ws, prefix)
}

#[test]
fn bail_stops_the_whole_data_run() {
    let server = Server::http();
    let (ws, prefix) = failing_row(&server);
    let (code, report) = run_json(
        ws.path(),
        &["rows", "--env", "dev", "--data", "rows.csv", "--bail"],
    );
    assert_eq!(code, Some(1));
    let statuses: Vec<String> = outcomes(&report).into_iter().map(|o| o.2).collect();
    assert_eq!(
        statuses,
        [
            "passed", "passed", "failed", "skipped", "skipped", "skipped"
        ]
    );
    for record in &requests(&report)[3..] {
        assert_eq!(record["skip_reason"], "bail");
    }
    assert_eq!(hits(&prefix), ["after-1"]);
}

#[test]
fn bail_iteration_skips_only_the_rest_of_the_failing_row() {
    let server = Server::http();
    let (ws, prefix) = failing_row(&server);
    let output = rustman(ws.path())
        .args(["run", "rows", "--env", "dev", "--data", "rows.csv"])
        .args(["--bail=iteration", "--no-history"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("iteration 2/3 · row 2\nFAIL  rows/1-check  GET 500  "),
        "{stdout}"
    );
    assert!(
        stdout.contains("SKIP  rows/2-after  skipped (bail)\n\niteration 3/3 · row 3\n"),
        "{stdout}"
    );
    assert_eq!(hits(&prefix), ["after-1", "after-3"]);
}

#[test]
fn junit_names_end_with_the_data_label() {
    let server = Server::http();
    let ws = workspace(&server);
    write(
        ws.path(),
        "get-profile.rman.yaml",
        &request("get-profile", "{{base_url}}/echo?u={{email}}", ""),
    );
    write(
        ws.path(),
        "users.csv",
        "email,role\nana@example.com,admin\nbo@example.com,user\n",
    );
    let junit = ws.path().join("out/junit.xml");
    rustman(ws.path())
        .args(["run", "--env", "dev", "--data", "users.csv"])
        .args(["--data-label", "email", "--no-history", "--reporter"])
        .arg(format!("junit:{}", junit.display()))
        .assert()
        .success();
    let xml = fs::read_to_string(junit).unwrap();
    assert!(
        xml.contains("name=\"get-profile [ana@example.com]\""),
        "{xml}"
    );
    assert!(
        xml.contains("name=\"get-profile [bo@example.com]\""),
        "{xml}"
    );
}

/// What a usage error printed, after checking it exits with 2 and that no
/// request reached the server.
fn refused(ws: &Path, prefix: &str, args: &[&str]) -> String {
    let output = rustman(ws)
        .arg("run")
        .args(args)
        .args(["--env", "dev", "--no-history"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(hits(prefix).is_empty(), "a request was sent");
    String::from_utf8(output.stderr).unwrap()
}

#[test]
fn bad_data_files_and_flag_combinations_are_usage_errors() {
    let server = Server::http();
    let ws = workspace(&server);
    let prefix = unique(ws.path());
    write(ws.path(), "r.rman.yaml", &timed(&prefix, "r", "r", ""));
    write(
        ws.path(),
        "users.csv",
        "email,password\nana@example.com,x\n",
    );
    write(ws.path(), "empty.csv", "email,role\n");
    write(ws.path(), "ragged.csv", "a,b\n1,2\n3,4\n5,6\n7,8,9\n");
    write(ws.path(), "cases.json", r#"{"id": 1}"#);
    let cases: [(&[&str], &str); 6] = [
        (
            &["--data", "users.csv", "--iterations", "5"],
            "--data and --iterations cannot be combined (iterations = row count)",
        ),
        (
            &["--data", "users.csv", "--data-label", "password"],
            "--data-label password: a secret column cannot label iterations",
        ),
        (&["--data", "empty.csv"], "--data empty.csv: no rows"),
        (
            &["--data", "ragged.csv"],
            "--data ragged.csv: row 4 has 3 fields, expected 2",
        ),
        (
            &["--data", "cases.json"],
            "--data cases.json: expected an array of objects",
        ),
        (
            &["--data-label", "email"],
            "--data-label email: there is no data file",
        ),
    ];
    for (args, message) in cases {
        assert_eq!(
            refused(ws.path(), &prefix, args),
            format!("error: {message}\n")
        );
    }
}

#[test]
fn iterations_cannot_override_a_collection_data_file() {
    let server = Server::http();
    let ws = tenants(&server);
    let prefix = unique(ws.path());
    let stderr = refused(ws.path(), &prefix, &["api/tenants", "--iterations", "2"]);
    assert_eq!(
        stderr,
        "error: api/tenants/_collection.yaml data sets the iterations (one per row); pass --no-data to use --iterations\n"
    );
}

/// Every file under `dir`, recursively.
fn files(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(files(&path));
        } else {
            out.push(path);
        }
    }
    out
}

#[test]
fn secret_columns_are_redacted_in_reports_history_and_logs() {
    const PASSWORD: &str = "pw-c4n4ry-Hq83";
    const PIN: &str = "pin-c4n4ry-7731";
    let server = Server::http();
    let ws = workspace(&server);
    write(
        ws.path(),
        "login.rman.yaml",
        &request(
            "login",
            "{{base_url}}/echo",
            "method: POST\nheaders:\n  - name: X-Pin\n    value: '{{pin}}'\nbody:\n  type: text\n  content: 'user={{email}}&password={{password}}'\n",
        ),
    );
    write(
        ws.path(),
        "users.csv",
        &format!("email,password,pin\nana@example.com,{PASSWORD},{PIN}\n"),
    );
    let report = ws.path().join("out/report.json");
    let output = rustman(ws.path())
        .args(["-vv", "run", "--env", "dev", "--data", "users.csv"])
        .args(["--secret-columns", "pin", "--data-label", "email"])
        .args(["--reporter", "pretty", "--reporter"])
        .arg(format!("json:{}", report.display()))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let json = valid_report(&fs::read(&report).unwrap());
    let sent = &requests(&json)[0]["exchange"]["request"];
    assert_eq!(
        sent["body"],
        "user=ana@example.com&password=«redacted:password»"
    );
    assert_eq!(header_value(&requests(&json)[0], "X-Pin"), "«redacted:pin»");
    let history = files(&ws.path().join(".rustman/history"));
    assert!(!history.is_empty(), "the run is recorded in history");
    let mut artifacts: Vec<(String, Vec<u8>)> = vec![
        ("stdout".into(), output.stdout),
        ("stderr".into(), output.stderr),
    ];
    for file in history.iter().chain([&report]) {
        artifacts.push((file.display().to_string(), fs::read(file).unwrap()));
    }
    for (name, bytes) in artifacts {
        let text = String::from_utf8_lossy(&bytes);
        for secret in [PASSWORD, PIN, "c4n4ry"] {
            assert!(!text.contains(secret), "{name} holds {secret}:\n{text}");
        }
    }
}
