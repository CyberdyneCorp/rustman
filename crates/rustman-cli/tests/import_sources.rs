//! `rustman import` for Insomnia, Bruno, HAR and `.http` sources: format
//! subcommands, folder sources, HAR filters, and no secret in any tracked
//! file or output (canaries).

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use common::{rustman, write};

fn fixture_path(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../rustman-import/tests/fixtures")
        .join(rel)
}

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

/// Run `rustman import <args>` in `ws`; returns stdout and stderr after
/// checking the exit code is 0.
fn import(ws: &Path, args: &[&str]) -> String {
    let output = rustman(ws).arg("import").args(args).output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    format!(
        "{}{}",
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap()
    )
}

/// No `CANARY` marker in the output, the JSON report or any tracked file.
fn assert_no_canary(ws: &Path, output: &str) {
    assert!(!output.contains("CANARY"), "{output}");
    for (path, text) in tracked_files(ws) {
        assert!(!text.contains("CANARY"), "{path}:\n{text}");
    }
}

#[test]
fn insomnia_exports_import_with_each_subcommand_or_detection() {
    for (args, name) in [
        (vec!["insomnia"], "insomnia/shop-v4.json"),
        (vec![], "insomnia/shop-v5.yaml"),
    ] {
        let ws = workspace();
        let src = fixture_path(name);
        let mut all = args.clone();
        all.push(src.to_str().unwrap());
        all.extend(["--report", "out/import.json"]);
        let output = import(ws.path(), &all);
        assert!(
            output.starts_with("imported Shop API (insomnia) into shop-api/\n"),
            "{output}"
        );
        assert!(
            output.contains("unsupported template tag: response"),
            "{output}"
        );
        let prod = fs::read_to_string(ws.path().join("environments/prod.yaml")).unwrap();
        assert!(prod.contains("https://shop.example.com"), "{prod}");
        assert_no_canary(ws.path(), &output);
    }
}

#[test]
fn nested_insomnia_environment_secrets_are_stored_not_written() {
    let ws = workspace();
    let export = r#"{"_type": "export", "__export_format": 4, "resources": [
        {"_id": "wrk_1", "_type": "workspace", "name": "Nested"},
        {"_id": "req_1", "_type": "request", "parentId": "wrk_1", "name": "Me",
         "method": "GET", "url": "{{ _.host }}/me",
         "headers": [{"name": "Authorization", "value": "Bearer {{ _.auth.token }}"}]},
        {"_id": "env_base", "_type": "environment", "parentId": "wrk_1", "name": "Base",
         "data": {"host": "https://api.example.com"}},
        {"_id": "env_prod", "_type": "environment", "parentId": "env_base", "name": "prod",
         "data": {"auth": {"token": "CANARY50", "deep": {"password": "CANARY51"}}}}
    ]}"#;
    write(ws.path(), "src/ins.json", export);
    let output = import(ws.path(), &["insomnia", "src/ins.json"]);
    let request = fs::read_to_string(ws.path().join("nested/me.rman.yaml")).unwrap();
    assert!(request.contains("Bearer {{auth.token}}"), "{request}");
    fs::remove_dir_all(ws.path().join("src")).unwrap();
    assert_no_canary(ws.path(), &output);
    rustman(ws.path())
        .args(["secret", "list", "--env", "prod"])
        .assert()
        .success()
        .stdout("prod/auth.token  set\nprod/auth.deep.password  set\n");
}

#[test]
fn a_bruno_folder_is_detected_and_its_secrets_are_stored() {
    let ws = workspace();
    let src = fixture_path("bruno/shop");
    let output = import(ws.path(), &[src.to_str().unwrap()]);
    assert!(
        output.starts_with("imported Shop (Bruno) (bruno) into shop-bruno/\n"),
        "{output}"
    );
    let health = fs::read_to_string(ws.path().join("shop-bruno/health.rman.yaml")).unwrap();
    assert!(health.contains("  - status: 200\n"), "{health}");
    assert_no_canary(ws.path(), &output);
    rustman(ws.path())
        .args(["secret", "list", "--env", "staging"])
        .assert()
        .success()
        .stdout("staging/api_password  set\n");
}

#[test]
fn a_malformed_bru_file_exits_2_with_its_line() {
    let ws = workspace();
    let src = fixture_path("bruno/malformed");
    rustman(ws.path())
        .args(["import", "bruno", src.to_str().unwrap()])
        .assert()
        .code(2)
        .stderr(predicates::str::ends_with(
            "broken.bru: line 10: block `headers` is not closed\n",
        ));
    assert!(!ws.path().join("broken").exists());
}

#[test]
fn har_filters_repeat_and_static_assets_are_counted() {
    let ws = workspace();
    let src = fixture_path("har/shop-capture.har");
    let src = src.to_str().unwrap();
    let output = import(ws.path(), &["har", src, "--dry-run"]);
    assert!(output.contains("dropped: 30;"), "{output}");
    let output = import(
        ws.path(),
        &[
            "har",
            src,
            "--host",
            "api.example.com",
            "--method",
            "GET",
            "--method",
            "DELETE",
        ],
    );
    assert!(output.contains("written: 4 requests"), "{output}");
    let files = tracked_files(ws.path());
    assert!(
        files.keys().all(|k| !k.contains("cdn-example-com")),
        "{files:?}"
    );
    assert_no_canary(ws.path(), &output);
}

#[test]
fn har_filters_belong_to_the_har_subcommand() {
    let ws = workspace();
    let src = fixture_path("har/shop-capture.har");
    rustman(ws.path())
        .args(["import", src.to_str().unwrap(), "--host", "x"])
        .assert()
        .code(2);
}

#[test]
fn http_files_import_with_private_environment_secrets() {
    let ws = workspace();
    let src = fixture_path("http/jetbrains/api.http");
    let output = import(ws.path(), &["http", src.to_str().unwrap()]);
    assert!(ws.path().join("api/create-item.rman.yaml").is_file());
    assert_no_canary(ws.path(), &output);
    rustman(ws.path())
        .args(["secret", "list", "--env", "dev"])
        .assert()
        .success()
        .stdout("dev/token  set\n");
}

#[test]
fn an_http_folder_reads_vscode_environments() {
    let ws = workspace();
    let src = fixture_path("http/rest-client");
    let output = import(ws.path(), &[src.to_str().unwrap()]);
    assert!(
        output.starts_with("imported rest-client (http) into rest-client/\n"),
        "{output}"
    );
    let local = fs::read_to_string(ws.path().join("environments/local.yaml")).unwrap();
    assert!(local.contains("secrets:\n  - password\n"), "{local}");
    assert_no_canary(ws.path(), &output);
}
