//! `rustman export postman|http`: collection folders and request files as a
//! Postman v2.1 collection or a `.http` file, with secrets only ever as
//! `{{name}}` placeholders (canary tests for both outputs).

mod common;

use std::fs;

use common::{rustman, write};

const SECRET: &str = "tok-CANARY-export-5150";

/// A workspace whose `dev` environment holds the secret `token`, and an
/// `api` collection whose request uses it and (by mistake) also carries
/// its value in the description.
fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    write(root, ".gitignore", ".rustman/\n");
    write(
        root,
        "environments/dev.yaml",
        "variables:\n  base: https://api.example.com\nsecrets:\n  - token\n",
    );
    write(root, "api/_collection.yaml", "name: API\n");
    write(
        root,
        "api/me.rman.yaml",
        &format!(
            "version: 1\nkind: http\nname: Me\ndescription: was {SECRET}\nurl: '{{{{base}}}}/me'\n\
             auth:\n  type: bearer\n  token: '{{{{token}}}}'\nassertions:\n  - status: 200\n"
        ),
    );
    rustman(root)
        .args(["secret", "set", "token", "--env", "dev"])
        .write_stdin(SECRET)
        .assert()
        .success();
    dir
}

fn export(dir: &std::path::Path, args: &[&str]) -> (String, String) {
    let out = rustman(dir)
        .args(["export"])
        .args(args)
        .args(["--env", "dev"])
        .assert()
        .success()
        .get_output()
        .clone();
    (
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

#[test]
fn http_export_keeps_the_bearer_token_a_placeholder() {
    let ws = workspace();
    let (stdout, stderr) = export(ws.path(), &["http", "api"]);
    assert_eq!(
        stdout,
        "### Me\nGET {{base}}/me\nAuthorization: Bearer {{token}}\n"
    );
    assert_eq!(
        stderr,
        "warning: export: api/me.rman.yaml: 1 assertions not exported (no equivalent)\n"
    );
}

#[test]
fn postman_export_writes_a_file_and_never_a_secret_value() {
    let ws = workspace();
    let (stdout, stderr) = export(ws.path(), &["postman", "api", "-o", "out/api.json"]);
    assert_eq!(stdout, "");
    assert!(stderr.contains("wrote out/api.json"), "{stderr}");
    let text = fs::read_to_string(ws.path().join("out/api.json")).unwrap();
    assert!(!text.contains(SECRET), "{text}");
    assert!(text.contains("was «redacted:token»"), "{text}");
    let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(doc["info"]["name"], "API");
    assert_eq!(
        doc["item"][0]["request"]["auth"]["bearer"][0]["value"],
        "{{token}}"
    );
    // A single request file exports too.
    let (stdout, _) = export(ws.path(), &["postman", "api/me.rman.yaml"]);
    assert!(!stdout.contains(SECRET), "{stdout}");
    assert!(stdout.contains("\"name\": \"Me\""), "{stdout}");
}

#[test]
fn paths_outside_the_workspace_or_missing_exit_2() {
    let ws = workspace();
    let outside = tempfile::tempdir().unwrap();
    rustman(ws.path())
        .args(["export", "http"])
        .arg(outside.path())
        .assert()
        .code(2)
        .stderr(predicates::str::contains("is outside the workspace"));
    rustman(ws.path())
        .args(["export", "postman", "nope"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "export: nope: no such file or folder",
        ));
    rustman(ws.path())
        .args(["export", "postman", "environments"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains(
            "export: environments is not a request file or collection folder of the workspace",
        ));
}
