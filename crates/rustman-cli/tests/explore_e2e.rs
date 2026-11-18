//! End to end against the `bad-api` fixture (add-openapi-explorer task 5.1
//! and the test-reports redaction requirement, task 4.6): import the
//! document, explore with `--allow-destructive` writing all five report
//! formats at once, render them again with `report render`, export every
//! finding as a regression and run the regressions. The whole flow must
//! finish well within the 5-minute budget, and no output may contain the
//! bearer token of either identity.
//!
//! Every probe family runs (`bola` with `--identity-b`), and each must find
//! the bug `bad-api` plants for it. `GET /admin/users` answers without
//! credentials and lists identity A's token, so the no-credentials auth
//! probe receives a secret it never sent: it must be redacted too.
//!
//! Mutation check (task 4.6): the token reaches responses (`GET /search`
//! echoes the request headers) and requests (the `Authorization` header).
//! In `rustman_explore::explore::exec::evidence`, replacing
//! `excerpt(&exchange.body, redactor)` with the raw body (checked: the test
//! then fails on the first rendering), or
//! `redactor.resolved_header_value(..)` with the raw header value, makes
//! this test fail: the report is the only source every renderer reads, so
//! redaction at build time is what keeps the token out of all five formats.

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::{rustman, write};
use rustman_test_support::bad_api::{BadApi, OPENAPI, TOKEN_A, TOKEN_B};
use serde_json::Value;

/// The five formats, and where `--report` writes each.
const FORMATS: [(&str, &str); 5] = [
    ("json", "out/report.json"),
    ("junit", "out/junit.xml"),
    ("markdown", "out/report.md"),
    ("html", "out/report.html"),
    ("sarif", "out/report.sarif"),
];

/// Every probe family, `bola` included.
const FAMILIES: &str = "conformance,negative-input,auth,undocumented-status,method-not-allowed,\
error-leakage,security-headers,lifecycle,fuzz,bola";

/// The bug each family must find: `(family, rule, operation)`.
const EXPECTED: [(&str, &str, &str); 11] = [
    (
        "conformance",
        "response-schema-violation",
        "GET /users/{id}",
    ),
    (
        "negative-input",
        "server-error-on-invalid-input",
        "POST /users",
    ),
    ("negative-input", "invalid-input-accepted", "POST /users"),
    ("auth", "auth-not-enforced", "GET /admin/users"),
    ("undocumented-status", "undocumented-status", "GET /status"),
    (
        "method-not-allowed",
        "undocumented-method-allowed",
        "PUT /users",
    ),
    ("error-leakage", "error-leakage", "GET /search"),
    (
        "security-headers",
        "missing-content-type-options",
        "GET /users",
    ),
    ("lifecycle", "update-not-reflected", "GET /users/{id}"),
    ("lifecycle", "resource-survives-delete", "GET /users/{id}"),
    (
        "bola",
        "broken-object-level-authorization",
        "GET /orders/{orderId}",
    ),
];

/// The `(rule, operation)` a fuzz probe must find with `--seed 7`.
const FUZZ_EXPECTED: (&str, &str) = ("missing-content-type-options", "POST /users");

/// The fixture's whole flow must take less than this.
const BUDGET: Duration = Duration::from_secs(5 * 60);

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

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn check(output: &std::process::Output, code: i32, step: &str) {
    assert_eq!(
        output.status.code(),
        Some(code),
        "{step}\nstdout:\n{}\nstderr:\n{}",
        text(&output.stdout),
        text(&output.stderr)
    );
    let all = format!("{}{}", text(&output.stdout), text(&output.stderr));
    for token in [TOKEN_A, TOKEN_B] {
        assert!(!all.contains(token), "{step} printed a token:\n{all}");
    }
}

/// The only exploration's id.
fn exploration_id(dir: &Path) -> String {
    let root = dir.join(".rustman").join("explorations");
    let mut ids: Vec<String> = std::fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(ids.len(), 1, "{ids:?}");
    ids.remove(0)
}

fn read(dir: &Path, path: &str) -> String {
    std::fs::read_to_string(dir.join(path)).unwrap_or_else(|e| panic!("{path}: {e}"))
}

#[test]
fn bad_api_end_to_end_in_every_format() {
    let started = Instant::now();
    let api = Api::start();
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path();
    write(
        ws,
        "rustman.toml",
        "[workspace]\nname = \"e2e\"\nversion = 1\n",
    );
    write(ws, "api.yaml", OPENAPI);

    let import = rustman(ws)
        .args(["import", "openapi", "api.yaml"])
        .output()
        .unwrap();
    check(&import, 0, "import");
    write(ws, "environments/dev.yaml", "secrets: [bearerAuth_token]\n");
    let secret = rustman(ws)
        .args(["secret", "set", "bearerAuth_token", "--env", "dev"])
        .write_stdin(TOKEN_A)
        .output()
        .unwrap();
    check(&secret, 0, "secret set");
    write(
        ws,
        "environments/other.yaml",
        "secrets: [bearerAuth_token]\n",
    );
    let secret = rustman(ws)
        .args(["secret", "set", "bearerAuth_token", "--env", "other"])
        .write_stdin(TOKEN_B)
        .output()
        .unwrap();
    check(&secret, 0, "secret set (identity B)");

    let target = api.server.url("");
    let mut args = vec![
        "explore",
        "run",
        "--spec",
        "api.yaml",
        "--target",
        &target,
        "--env",
        "dev",
        "--identity-b",
        "other",
        "--probes",
        FAMILIES,
        "--allow-destructive",
        "--seed",
        "7",
        "--fuzz-cases",
        "4",
        "--rate",
        "1000",
    ];
    let reports: Vec<String> = FORMATS
        .iter()
        .map(|(format, path)| format!("{format}={path}"))
        .collect();
    for report in &reports {
        args.extend(["--report", report.as_str()]);
    }
    let explored = rustman(ws).args(&args).output().unwrap();
    check(&explored, 1, "explore run");

    let id = exploration_id(ws);
    let canonical = read(ws, &format!(".rustman/explorations/{id}/report.json"));
    assert_eq!(read(ws, "out/report.json"), canonical);
    validates_against_the_published_schema(ws, &canonical);
    every_family_finds_its_bug(&canonical);
    renders_the_same_from_the_json_report(ws, &id);
    no_output_contains_the_token(ws, &id);

    let exported = rustman(ws)
        .args([
            "explore",
            "export",
            &id,
            "--findings",
            "all",
            "--into",
            "regressions",
        ])
        .output()
        .unwrap();
    check(&exported, 0, "explore export");
    let var = format!("baseUrl={target}");
    let regressions = rustman(ws)
        .args([
            "run",
            "regressions",
            "--env",
            "dev",
            "--no-history",
            "--var",
            &var,
        ])
        .args(["--report", "junit=out/regressions.xml"])
        .output()
        .unwrap();
    // The API is still broken, so its regressions fail.
    check(&regressions, 1, "run regressions");
    assert!(read(ws, "out/regressions.xml").contains("<failure"));

    let elapsed = started.elapsed();
    assert!(elapsed < BUDGET, "took {elapsed:?}");
}

fn validates_against_the_published_schema(ws: &Path, report: &str) {
    let schema = rustman(ws).args(["report", "schema"]).output().unwrap();
    check(&schema, 0, "report schema");
    let schema: Value = serde_json::from_slice(&schema.stdout).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let report: Value = serde_json::from_str(report).unwrap();
    let errors: Vec<String> = validator
        .iter_errors(&report)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:#?}");
    assert_eq!(report["kind"], "exploration");
    assert!(report["summary"]["findings"]["critical"].as_u64().unwrap() > 0);
}

fn every_family_finds_its_bug(report: &str) {
    let report: Value = serde_json::from_str(report).unwrap();
    let probes = report["probes"].as_array().unwrap();
    let findings = report["findings"].as_array().unwrap();
    // `undocumented-status`, `error-leakage` and `security-headers` are
    // checks on every response, with no probes of their own.
    let detectors = ["undocumented-status", "error-leakage", "security-headers"];
    for family in FAMILIES.split(',').filter(|f| !detectors.contains(f)) {
        let ran = probes
            .iter()
            .filter(|p| p["family"] == family)
            .any(|p| p["outcome"] == "finding" || p["outcome"] == "passed");
        assert!(ran, "no {family} probe ran");
    }
    for (family, rule, operation) in EXPECTED {
        let found = findings
            .iter()
            .any(|f| f["rule"] == rule && f["operation"] == operation);
        assert!(found, "{family}: no {rule} on {operation}");
    }
    // Fuzzing is seeded, so the cases (and what they find) are fixed:
    // with `--seed 7`, a fuzz probe of `POST /users` reports the missing
    // header. The finding must name that fuzz probe, not only other ones.
    let (rule, operation) = FUZZ_EXPECTED;
    let fuzz_probes: Vec<&Value> = probes
        .iter()
        .filter(|p| p["family"] == "fuzz" && p["operation"] == operation)
        .map(|p| &p["id"])
        .collect();
    let found = findings.iter().any(|f| {
        f["rule"] == rule
            && f["operation"] == operation
            && f["probes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|id| fuzz_probes.contains(&id))
    });
    assert!(found, "fuzz: no {rule} on {operation} from a fuzz probe");
}

/// The SARIF of a real exploration is valid SARIF 2.1.0.
fn sarif_validates(ws: &Path) {
    let schema = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../rustman-report/tests/fixtures/sarif-schema-2.1.0.json");
    let schema: Value = serde_json::from_str(&std::fs::read_to_string(schema).unwrap()).unwrap();
    let validator = jsonschema::options()
        .with_draft(jsonschema::Draft::Draft4)
        .build(&schema)
        .unwrap();
    let sarif: Value = serde_json::from_str(&read(ws, "out/report.sarif")).unwrap();
    let errors: Vec<String> = validator
        .iter_errors(&sarif)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:#?}");
}

/// `report render` of the canonical report gives what `--report` wrote.
fn renders_the_same_from_the_json_report(ws: &Path, id: &str) {
    let source = format!(".rustman/explorations/{id}/report.json");
    for (format, path) in FORMATS {
        let rendered = rustman(ws)
            .args(["report", "render", &source, "--format", format])
            .output()
            .unwrap();
        check(&rendered, 0, &format!("report render --format {format}"));
        assert_eq!(text(&rendered.stdout), read(ws, path), "{format}");
    }
    sarif_validates(ws);
    let sarif: Value = serde_json::from_str(&read(ws, "out/report.sarif")).unwrap();
    let location = &sarif["runs"][0]["results"][0]["locations"][0];
    assert_eq!(
        location["physicalLocation"]["artifactLocation"]["uri"],
        "api.yaml"
    );
    assert!(
        location["physicalLocation"]["region"]["startLine"]
            .as_u64()
            .unwrap()
            > 1
    );
}

fn no_output_contains_the_token(ws: &Path, id: &str) {
    let mut outputs: Vec<String> = FORMATS.iter().map(|(_, p)| read(ws, p)).collect();
    let dir = format!(".rustman/explorations/{id}");
    for file in ["report.json", "journal.ndjson", "state.json", "plan.json"] {
        if ws.join(&dir).join(file).exists() {
            outputs.push(read(ws, &format!("{dir}/{file}")));
        }
    }
    for (i, output) in outputs.iter().enumerate() {
        for token in [TOKEN_A, TOKEN_B] {
            assert!(!output.contains(token), "output {i} leaks a token");
        }
    }
    for (format, path) in FORMATS {
        if format == "junit" || format == "sarif" {
            continue;
        }
        assert!(
            read(ws, path).contains("«redacted:bearerAuth_token»"),
            "{format} shows where the token was"
        );
    }
}
