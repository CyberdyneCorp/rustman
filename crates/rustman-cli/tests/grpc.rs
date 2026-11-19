//! `rustman grpc list`, `describe`, `call` and `health`, and `kind: grpc`
//! in `rustman run` (add-grpc-client tasks 2.1, 3.5, 6.1 and 6.4) against
//! in-process tonic servers on their own runtime: output formats, exit
//! codes, the descriptor cache, and secrets kept out of every output.

mod common;

use std::fs;
use std::path::Path;

use common::{rustman, valid_report, write};
use rustman_test_support::grpc::{ServingStatus, UNREACHABLE_TARGET};
use rustman_test_support::{CATALOG_SERVICE, GrpcServer, Reflection};
use serde_json::Value;

const SECRET: &str = "s3cr3t-grpc-cli-canary";

/// A server and the runtime it is served from (dropped after it).
struct Served {
    server: GrpcServer,
    runtime: tokio::runtime::Runtime,
}

fn serve(reflection: Reflection) -> Served {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let server = runtime.block_on(GrpcServer::start(reflection));
    Served { server, runtime }
}

/// A workspace whose `dev` environment points `grpc_host` at `target`,
/// with the secret `token` set to [`SECRET`].
fn workspace(target: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n[defaults]\ndefault_environment = \"dev\"\n",
    );
    write(
        dir.path(),
        "environments/dev.yaml",
        &format!("variables:\n  grpc_host: '{target}'\nsecrets: [token]\n"),
    );
    rustman(dir.path())
        .args(["secret", "set", "token", "--env", "dev"])
        .write_stdin(SECRET)
        .assert()
        .success();
    dir
}

/// A `kind: grpc` file calling `method` of the catalog with `rest`.
fn grpc_file(method: &str, rest: &str) -> String {
    format!(
        "version: 1\nkind: grpc\nname: {method}\ntarget: '{{{{grpc_host}}}}'\nmethod: shop.v1.Catalog/{method}\n{rest}"
    )
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Every file under `dir`, read as text.
fn all_text(dir: &Path) -> String {
    let mut out = String::new();
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.push_str(&all_text(&path));
        } else {
            out.push_str(&String::from_utf8_lossy(&fs::read(&path).unwrap()));
        }
    }
    out
}

/// Spec "v1alpha fallback": services are listed from a server with
/// v1alpha reflection only, and the answer is cached.
#[test]
fn list_falls_back_to_v1alpha_and_caches_the_schema() {
    let served = serve(Reflection::V1Alpha);
    let dir = workspace(&served.server.target());
    let out = rustman(dir.path())
        .args(["grpc", "list", "{{grpc_host}}"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    assert!(stdout.contains("service shop.v1.Catalog\n"), "{stdout}");
    assert!(
        stdout.contains(
            "  ListProducts (server streaming): shop.v1.ListProductsRequest -> shop.v1.Product\n"
        ),
        "{stdout}"
    );
    let cache = dir.path().join(".rustman/grpc/descriptors");
    assert_eq!(fs::read_dir(&cache).unwrap().count(), 2, "set and sidecar");

    let out = rustman(dir.path())
        .args(["grpc", "list", "{{grpc_host}}", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let services: Vec<Value> = text(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let catalog = services
        .iter()
        .find(|s| s["name"] == CATALOG_SERVICE)
        .unwrap();
    assert_eq!(catalog["methods"][3]["call_type"], "bidi");
    assert!(
        text(&out.stderr).contains("may be stale"),
        "the second list comes from the cache"
    );
    let out = rustman(dir.path())
        .args(["grpc", "list", "{{grpc_host}}", "--refresh"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(!text(&out.stderr).contains("may be stale"));
}

/// Spec "Describe a method".
#[test]
fn describe_shows_call_type_types_and_template() {
    let served = serve(Reflection::Both);
    let target = served.server.target();
    let dir = tempfile::tempdir().unwrap();
    let out = rustman(dir.path())
        .args(["grpc", "describe", &target, "shop.v1.Catalog/ListProducts"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    for line in [
        "shop.v1.Catalog/ListProducts",
        "  call type: server streaming",
        "  request:   shop.v1.ListProductsRequest",
        "  response:  shop.v1.Product",
        "\"pageSize\": 0",
    ] {
        assert!(stdout.contains(line), "{line}\n{stdout}");
    }
    let out = rustman(dir.path())
        .args(["grpc", "describe", &target, "shop.v1.Nope", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out.stderr).contains("grpc: unknown symbol 'shop.v1.Nope'"));
}

/// Spec "Ad-hoc call with JSON output".
#[test]
fn an_ad_hoc_stream_prints_json_lines_then_the_status() {
    let served = serve(Reflection::V1);
    let target = served.server.target();
    let dir = tempfile::tempdir().unwrap();
    let out = rustman(dir.path())
        .args(["grpc", "call", "--target", &target])
        .args(["--method", "shop.v1.Catalog/ListProducts", "--data", "{}"])
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let lines: Vec<Value> = text(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 4, "{lines:?}");
    let ids: Vec<&str> = lines[..3]
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["p-1", "p-2", "p-3"]);
    assert_eq!(lines[3]["status"]["name"], "OK");
    assert_eq!(lines[3]["status"]["code"], 0);
}

/// A request file with a secret in its metadata: the server gets it, no
/// output or history file shows it.
#[test]
fn a_request_file_call_keeps_its_secret_out_of_every_output() {
    let served = serve(Reflection::V1);
    let dir = workspace(&served.server.target());
    let file = grpc_file(
        "GetProduct",
        "message: {id: p-1}\nmetadata:\n  - {name: authorization, value: 'Bearer {{token}}'}\n  - {name: x-echo, value: '{{token}}'}\n",
    );
    write(dir.path(), "grpc/get.rman.yaml", &file);
    let out = rustman(dir.path())
        .args(["grpc", "call", "grpc/get.rman.yaml"])
        .output()
        .unwrap();
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert!(stdout.contains("\"id\": \"p-1\""), "{stdout}");
    assert!(stderr.contains("status: OK (0)"), "{stderr}");
    let received = &served.server.calls()[0];
    assert_eq!(received.text("x-echo").as_deref(), Some(SECRET));
    assert!(!stdout.contains(SECRET) && !stderr.contains(SECRET));
    // The file secret store of the tests keeps the secret under
    // `.rustman/`; history and the descriptor cache must not.
    let rustman_dir = dir.path().join(".rustman");
    let stored = all_text(&rustman_dir.join("history")) + &all_text(&rustman_dir.join("grpc"));
    assert!(stored.contains("Bearer «redacted:token»"), "{stored}");
    assert!(!stored.contains(SECRET), "{stored}");
}

/// Spec "Status assertion failure" through `rustman grpc call` (exit 1),
/// an unknown field (exit 2) and an unreachable server (exit 3).
#[test]
fn call_exit_codes_follow_the_runner() {
    let served = serve(Reflection::V1);
    let target = served.server.target();
    let dir = tempfile::tempdir().unwrap();
    let call = |data: &str, target: &str| {
        rustman(dir.path())
            .args(["grpc", "call", "--target", target])
            .args(["--method", "shop.v1.Catalog/GetProduct", "--data", data])
            .output()
            .unwrap()
    };
    let out = call(r#"{"id":"missing"}"#, &target);
    assert_eq!(out.status.code(), Some(1));
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains("fail: grpc_status: expected OK, got NOT_FOUND (5)"),
        "{stderr}"
    );
    let out = call(r#"{"idd":"p-1"}"#, &target);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        text(&out.stderr).contains("error: grpc: unknown field 'idd' in shop.v1.GetProductRequest")
    );
    let out = call(r#"{"id":"p-1"}"#, UNREACHABLE_TARGET);
    assert_eq!(out.status.code(), Some(3), "{}", text(&out.stderr));
}

/// Spec "No reflection": exit code 2 and the reflection error.
#[test]
fn a_call_without_reflection_or_schema_exits_2() {
    let served = serve(Reflection::None);
    let target = served.server.target();
    let dir = tempfile::tempdir().unwrap();
    let out = rustman(dir.path())
        .args(["grpc", "call", "--target", &target])
        .args(["--method", "shop.v1.Catalog/GetProduct", "--data", "{}"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let expected = format!(
        "error: grpc: server reflection unavailable at {target}; set schema.protos or schema.descriptor_set"
    );
    assert!(
        text(&out.stderr).contains(&expected),
        "{}",
        text(&out.stderr)
    );
}

/// Spec "Not serving", and the health exit codes: 0 for SERVING, 1 for
/// NOT_SERVING and UNKNOWN, 3 for an error (NOT_FOUND, UNIMPLEMENTED, an
/// unreachable server). SERVICE_UNKNOWN only comes from `Watch`; its code
/// is covered by the unit tests of `rustman_grpc::health`.
#[test]
fn health_exit_codes() {
    let served = serve(Reflection::V1);
    let target = served.server.target();
    let dir = tempfile::tempdir().unwrap();
    let health = |target: &str, service: &str| {
        rustman(dir.path())
            .args(["grpc", "health", target, "--service", service])
            .output()
            .unwrap()
    };
    let out = health(&target, CATALOG_SERVICE);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(text(&out.stdout), "SERVING\n");
    served
        .runtime
        .block_on(served.server.set_not_serving(CATALOG_SERVICE));
    let out = health(&target, CATALOG_SERVICE);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(text(&out.stdout), "NOT_SERVING\n");
    let out = rustman(dir.path())
        .args([
            "grpc",
            "health",
            &target,
            "--service",
            CATALOG_SERVICE,
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(text(&out.stdout), "{\"status\":\"NOT_SERVING\"}\n");
    let out = health(&target, "shop.v1.Unknown");
    assert_eq!(out.status.code(), Some(3));
    assert!(text(&out.stderr).contains("error: grpc: health check failed: NOT_FOUND (5)"));
    served.runtime.block_on(
        served
            .server
            .set_health(CATALOG_SERVICE, ServingStatus::Unknown),
    );
    let out = health(&target, CATALOG_SERVICE);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(text(&out.stdout), "UNKNOWN\n");
    assert_eq!(health(UNREACHABLE_TARGET, "").status.code(), Some(3));
    // A server without the health service answers UNIMPLEMENTED: exit 3.
    let bare = served
        .runtime
        .block_on(GrpcServer::start_without_health(Reflection::V1));
    let out = health(&bare.target(), "");
    assert_eq!(out.status.code(), Some(3), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("UNIMPLEMENTED (12)"),
        "{}",
        text(&out.stderr)
    );
    drop(bare);
}

/// Spec "Deadline exceeded": `rustman run` exits 3, and the JSON report
/// records the gRPC exchange.
#[test]
fn an_expired_deadline_makes_run_exit_3() {
    let served = serve(Reflection::V1);
    let dir = workspace(&served.server.target());
    let slow = grpc_file("GetProduct", "message: {id: slow}\ndeadline_ms: 200\n");
    write(dir.path(), "grpc/slow.rman.yaml", &slow);
    let out = rustman(dir.path())
        .args(["run", "grpc", "--reporter", "json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3), "{}", text(&out.stderr));
    let report = valid_report(&out.stdout);
    let record = &report["requests"][0];
    assert_eq!(record["status"], "errored");
    assert_eq!(record["error"], "grpc: DEADLINE_EXCEEDED (4)");
    assert_eq!(record["grpc"]["protocol"], "grpc");
    assert_eq!(record["grpc"]["status"]["name"], "DEADLINE_EXCEEDED");
}

/// Spec "Status assertion failure" in `rustman run` (exit 1), and a
/// passing stream assertion.
#[test]
fn run_evaluates_grpc_assertions() {
    let served = serve(Reflection::V1);
    let dir = workspace(&served.server.target());
    let list = grpc_file(
        "ListProducts",
        "message: {}\nassertions:\n  - json: {path: $.messages, length: 3}\n",
    );
    write(dir.path(), "grpc/a-list.rman.yaml", &list);
    let out = rustman(dir.path()).args(["run", "grpc"]).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stdout));
    let missing = grpc_file(
        "GetProduct",
        "message: {id: missing}\nassertions:\n  - grpc_status: OK\n",
    );
    write(dir.path(), "grpc/b-missing.rman.yaml", &missing);
    let out = rustman(dir.path()).args(["run", "grpc"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stdout = text(&out.stdout);
    assert!(
        stdout.contains("grpc_status: expected OK, got NOT_FOUND (5)"),
        "{stdout}"
    );
}

/// Task 6.4: `rustman grpc --help` lists every subcommand.
#[test]
fn grpc_help_lists_every_subcommand() {
    let dir = tempfile::tempdir().unwrap();
    let out = rustman(dir.path())
        .args(["grpc", "--help"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let stdout = text(&out.stdout);
    for command in ["list", "describe", "call", "health"] {
        assert!(
            stdout.contains(&format!("\n  {command} ")),
            "{command}\n{stdout}"
        );
    }
}
