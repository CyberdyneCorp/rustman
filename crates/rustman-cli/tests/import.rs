//! `rustman import` for collections: detection, destination, dry run,
//! reports, exit codes, size limits, and the v0.2 exit criterion (a real
//! Postman collection imports with no secret in any tracked file).

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::{Server, rustman, write};

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../rustman-import/tests/fixtures/postman")
        .join(name)
}

fn fixture(name: &str) -> String {
    fs::read_to_string(fixture_path(name)).unwrap()
}

const BILLING: &str = "billing-api.postman_collection.json";

/// A workspace with a git-ignored `.rustman/` and no environment.
fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    write(dir.path(), ".gitignore", ".rustman/\n");
    dir
}

/// Every file under `root` except the git-ignored `.rustman/`.
fn tracked_files(root: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                if !path.ends_with(".rustman") {
                    dirs.push(path);
                }
            } else {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, fs::read_to_string(&path).unwrap_or_default());
            }
        }
    }
    out
}

/// A minimal Postman v2.1 collection named `name` with `requests` items.
fn collection(name: &str, requests: usize) -> String {
    let items: Vec<String> = (1..=requests)
        .map(|i| format!(r#"{{"name": "R{i}", "request": {{"method": "GET", "url": "{{{{base_url}}}}/r/{i}"}}}}"#))
        .collect();
    format!(
        r#"{{"info": {{"name": "{name}", "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"}}, "item": [{}]}}"#,
        items.join(", ")
    )
}

#[test]
fn auto_detects_a_postman_collection_from_its_content() {
    let ws = workspace();
    write(ws.path(), "export.json", &fixture(BILLING));
    rustman(ws.path())
        .args(["import", "export.json"])
        .assert()
        .success()
        .stdout(predicates::str::starts_with(
            "imported Billing API (postman) into billing-api/\n",
        ));
    // The default destination is the slug of the collection name.
    assert!(
        ws.path()
            .join("billing-api/invoices/list-invoices.rman.yaml")
            .is_file()
    );
}

#[test]
fn unknown_content_exits_2() {
    let ws = workspace();
    write(ws.path(), "data.json", "{\"data\": [1, 2, 3]}");
    rustman(ws.path())
        .args(["import", "data.json"])
        .assert()
        .code(2)
        .stdout("")
        .stderr("error: import: cannot detect format of data.json (expected postman, insomnia, bruno, har, http)\n");
    rustman(ws.path())
        .args(["import", "auto", "data.json"])
        .assert()
        .code(2);
}

#[test]
fn a_bare_import_prints_its_subcommands_and_exits_2() {
    let ws = workspace();
    let output = rustman(ws.path()).arg("import").output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    let text = String::from_utf8_lossy(&output.stderr) + String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("postman-env"), "{text}");
}

#[test]
fn outside_a_workspace_suggests_init() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "c.json", &collection("A", 1));
    rustman(dir.path())
        .args(["import", "postman", "c.json"])
        .assert()
        .code(2)
        .stderr("error: no rustman workspace found (run 'rustman init')\n");
}

#[test]
fn into_must_be_inside_the_workspace() {
    let ws = workspace();
    write(ws.path(), "c.json", &collection("A", 1));
    rustman(ws.path())
        .args(["import", "postman", "c.json", "--into", "../elsewhere"])
        .assert()
        .code(2)
        .stderr("error: import: --into must be inside the workspace\n");
    rustman(ws.path())
        .args(["import", "postman", "c.json", "--into", "apis/a"])
        .assert()
        .success();
    assert!(ws.path().join("apis/a/r1.rman.yaml").is_file());
}

#[test]
fn oversized_sources_are_refused() {
    let ws = workspace();
    write(ws.path(), "c.json", &collection("A", 1));
    rustman(ws.path())
        .args(["import", "c.json", "--max-size", "10"])
        .assert()
        .code(2)
        .stderr("error: import: source exceeds 10 bytes (use --max-size)\n");
    // A sparse file larger than the default limit is refused before reading.
    let big = fs::File::create(ws.path().join("big.har")).unwrap();
    big.set_len(300 << 20).unwrap();
    drop(big);
    rustman(ws.path())
        .args(["import", "big.har"])
        .assert()
        .code(2)
        .stderr("error: import: source exceeds 100 MiB (use --max-size)\n");
}

#[test]
fn dry_run_prints_the_plan_and_changes_nothing() {
    let ws = workspace();
    write(ws.path(), "c.json", &collection("Twelve", 12));
    let before = tracked_files(ws.path());
    let output = rustman(ws.path())
        .args(["import", "postman", "c.json", "--dry-run"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).unwrap();
    for i in 1..=12 {
        assert!(
            stdout.contains(&format!("  create twelve/r{i}.rman.yaml\n")),
            "{stdout}"
        );
    }
    assert!(
        stdout.contains("dry run: nothing was written\n"),
        "{stdout}"
    );
    assert_eq!(tracked_files(ws.path()), before);
    assert!(!ws.path().join(".rustman").exists());
}

#[test]
fn the_json_report_matches_the_summary() {
    let ws = workspace();
    write(ws.path(), "c.json", &fixture(BILLING));
    let output = rustman(ws.path())
        .args(["import", "c.json", "--report", "out/import.json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let report: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(ws.path().join("out/import.json")).unwrap())
            .unwrap();
    assert_eq!(report["schema"], "rustman.import-report/v1");
    let s = &report["summary"];
    let line = format!(
        "written: {} requests, {} collections, {} environments; skipped: {}; dropped: {}; \
         secrets moved: {}; unsupported: {}; unmapped: {}\n",
        s["requests"],
        s["collections"],
        s["environments"],
        s["skipped"],
        s["dropped"],
        s["secrets_moved"],
        s["unsupported"],
        s["unmapped"],
    );
    assert!(stdout.contains(&line), "{stdout}");
    assert_eq!(s["requests"], 12);
}

#[test]
fn script_migration_is_summarized_and_reported_as_markdown() {
    let ws = workspace();
    write(
        ws.path(),
        "c.json",
        &fixture("migration.postman_collection.json"),
    );
    let output = rustman(ws.path())
        .args([
            "import",
            "c.json",
            "--dry-run",
            "--migration-report",
            "m/report.md",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("scripts: 31/40 statements converted (77%)\n"),
        "{stdout}"
    );
    let report = fs::read_to_string(ws.path().join("m/report.md")).unwrap();
    assert!(report.contains("\n## Checkout\n"), "{report}");
    assert!(
        report.contains("no declarative equivalent; consider a plugin"),
        "{report}"
    );
    assert!(!report.contains("sk_live_CANARY_mig_77aa"), "{report}");
    assert!(!stdout.contains("sk_live_CANARY_mig_77aa"), "{stdout}");
}

#[test]
fn partial_conversions_exit_0_unless_strict() {
    let ws = workspace();
    write(ws.path(), "c.json", &fixture(BILLING));
    rustman(ws.path())
        .args(["import", "c.json", "--dry-run"])
        .assert()
        .code(0)
        .stdout(predicates::str::contains(
            "unsupported: hawk auth unsupported",
        ));
    rustman(ws.path())
        .args(["import", "c.json", "--dry-run", "--fail-on-unsupported"])
        .assert()
        .code(1);
    write(ws.path(), "clean.json", &collection("Clean", 2));
    rustman(ws.path())
        .args(["import", "clean.json", "--fail-on-unsupported"])
        .assert()
        .code(0);
}

#[test]
fn reimport_skips_edited_files_unless_told_otherwise() {
    let ws = workspace();
    write(ws.path(), "c.json", &collection("Billing API", 2));
    rustman(ws.path())
        .args(["import", "c.json"])
        .assert()
        .success();
    let file = ws.path().join("billing-api/r1.rman.yaml");
    let edited = fs::read_to_string(&file).unwrap() + "tags: [mine]\n";
    fs::write(&file, &edited).unwrap();
    rustman(ws.path())
        .args(["import", "c.json"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "conflict: billing-api/r1.rman.yaml skipped (exists)\n",
        ));
    assert_eq!(fs::read_to_string(&file).unwrap(), edited);
    rustman(ws.path())
        .args(["import", "c.json", "--on-conflict", "overwrite"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "conflict: billing-api/r1.rman.yaml overwritten\n",
        ));
    assert!(!fs::read_to_string(&file).unwrap().contains("tags"));
}

#[test]
fn postman_environments_store_secret_values_outside_the_workspace() {
    let ws = workspace();
    write(
        ws.path(),
        "staging.json",
        &fixture("staging.postman_environment.json"),
    );
    rustman(ws.path())
        .args(["import", "postman-env", "staging.json"])
        .assert()
        .success();
    let env = fs::read_to_string(ws.path().join("environments/staging.yaml")).unwrap();
    assert!(
        env.contains("secrets:\n  - api_key\n  - admin_password\n"),
        "{env}"
    );
    assert!(!env.contains("CANARY"), "{env}");
    rustman(ws.path())
        .args(["secret", "list", "--env", "staging"])
        .assert()
        .success()
        .stdout("staging/api_key  set\nstaging/admin_password  set\n");
}

/// v0.2 exit criterion: the realistic billing collection, with secrets in
/// variables, auth and headers, imports with no secret in any tracked file
/// or output, and the moved secrets still authenticate the requests.
#[test]
fn a_real_postman_collection_imports_without_leaking_a_secret() {
    let server = Server::http();
    let ws = workspace();
    let token = rustman_test_support::routes::TOKEN;
    // The collection's bearer token becomes the one the test server accepts.
    let text = fixture(BILLING).replace("sk_live_CANARY_collection_bearer_7f3a9c2e", token);
    let mut doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    doc["variable"][0]["value"] = server.url("").trim_end_matches('/').into();
    write(
        ws.path(),
        "billing.postman_collection.json",
        &doc.to_string(),
    );
    let output = rustman(ws.path())
        .args([
            "import",
            "billing.postman_collection.json",
            "--report",
            "import.json",
            "--migration-report",
            "migration.md",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    fs::remove_file(ws.path().join("billing.postman_collection.json")).unwrap();
    let migration = fs::read_to_string(ws.path().join("migration.md")).unwrap();
    assert!(
        migration.starts_with("# Script migration report\n"),
        "{migration}"
    );
    // Exporting the imported collection back is another output to check.
    let exported = rustman(ws.path())
        .args(["export", "postman", "billing-api", "--env", "billing-api"])
        .output()
        .unwrap();
    assert_eq!(exported.status.code(), Some(0), "{exported:?}");
    let outputs = [
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
        String::from_utf8(exported.stdout).unwrap(),
        String::from_utf8(exported.stderr).unwrap(),
    ];
    let mut secrets: Vec<String> = Vec::new();
    collect_canaries(&doc, &mut secrets);
    assert!(secrets.len() >= 14, "{secrets:?}");
    // Encoded forms of the credentials, and the marker alone: a partial or
    // encoded leak shows as well as a whole one.
    for canary in secrets.clone() {
        secrets.push(STANDARD.encode(format!("uploader:{canary}")));
        secrets.push(STANDARD.encode(&canary));
    }
    secrets.push("CANARY".to_owned());
    secrets.push(token.to_owned());
    for (path, text) in tracked_files(ws.path()) {
        for secret in &secrets {
            assert!(
                !text.contains(secret.as_str()),
                "{secret} in {path}:\n{text}"
            );
        }
    }
    for text in &outputs {
        for secret in &secrets {
            assert!(
                !text.contains(secret.as_str()),
                "{secret} in output:\n{text}"
            );
        }
    }
    // The secrets live in the secret store, and resolve when sending.
    rustman(ws.path())
        .args(["send", "Get User", "--env", "billing-api", "--no-history"])
        .assert()
        .code(0)
        .stdout(predicates::str::starts_with("HTTP/1.1 200"));
}

/// Every string in `v` that contains the fixtures' secret marker.
fn collect_canaries(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::String(s) if s.contains("CANARY") => {
            let start = s.find("CANARY").unwrap();
            let token: String = s[start..]
                .chars()
                .take_while(|c| !c.is_whitespace() && *c != ';')
                .collect();
            out.push(token);
        }
        serde_json::Value::Array(items) => items.iter().for_each(|i| collect_canaries(i, out)),
        serde_json::Value::Object(map) => map.values().for_each(|i| collect_canaries(i, out)),
        _ => {}
    }
}
