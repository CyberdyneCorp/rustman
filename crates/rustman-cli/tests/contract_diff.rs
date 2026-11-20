//! `rustman contract diff` (add-contract-testing tasks 3.1 to 3.3 and 4.2):
//! the spec scenarios for exit codes and strictness, the allowlist, `--base`
//! against a temporary Git repository, `--report` and redaction. Every
//! document is a local file; nothing is sent.

mod common;

use std::path::Path;
use std::process::Output;

use common::{rustman, valid_report, write};

const USERS: &str = "  /users:\n    get:\n      responses:\n        '200': {description: ok}\n";
const DELETE_USER: &str = "  /users/{id}:\n    delete:\n      parameters:\n        \
    - {name: id, in: path, required: true, schema: {type: string}}\n      responses:\n        \
    '204': {description: gone}\n";
const HEALTH: &str = "  /health:\n    get:\n      responses:\n        '200': {description: ok}\n";

fn doc(paths: &str) -> String {
    format!("openapi: 3.1.0\ninfo: {{title: t, version: '1'}}\npaths:\n{paths}")
}

/// A response whose schema is `oneOf` the given alternatives.
fn one_of(alternatives: &str) -> String {
    doc(&format!(
        "  /pets:\n    get:\n      responses:\n        '200':\n          description: ok\n          \
         content:\n            application/json:\n              schema:\n                \
         oneOf: [{alternatives}]\n"
    ))
}

/// `v1.yaml` and `v2.yaml` in a new folder.
fn pair(v1: &str, v2: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "v1.yaml", v1);
    write(dir.path(), "v2.yaml", v2);
    dir
}

/// v2 adds `GET /health` and removes `DELETE /users/{id}`.
fn two_versions() -> tempfile::TempDir {
    pair(
        &doc(&format!("{USERS}{DELETE_USER}")),
        &doc(&format!("{USERS}{HEALTH}")),
    )
}

fn diff(dir: &Path, extra: &[&str]) -> Output {
    rustman(dir)
        .args(["contract", "diff", "v1.yaml", "v2.yaml"])
        .args(extra)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

#[test]
fn two_versions_are_compared_with_pointers_into_both() {
    let dir = two_versions();
    let output = diff(dir.path(), &[]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let out = stdout(&output);
    assert!(
        out.contains(
            "breaking      operation-removed  DELETE /users/{id}: operation removed\n  \
             old: /paths/~1users~1{id}/delete\n  new: /paths\n"
        ),
        "{out}"
    );
    assert!(
        out.contains(
            "non-breaking  operation-added    GET /health: operation added\n  \
             old: /paths\n  new: /paths/~1health/get\n"
        ),
        "{out}"
    );
}

#[test]
fn unknown_differences_fail_only_with_fail_on_unknown() {
    let dir = pair(
        &one_of("{type: string}, {type: integer}"),
        &one_of("{type: string}, {type: integer}, {type: boolean}"),
    );
    let output = diff(dir.path(), &[]);
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
    assert!(stdout(&output).contains("unknown       composition-changed"));
    for flags in [&["--fail-on-unknown"][..], &["--fail-on", "unknown"]] {
        assert_eq!(diff(dir.path(), flags).status.code(), Some(1), "{flags:?}");
    }
}

#[test]
fn fail_on_non_breaking_fails_any_difference() {
    let dir = pair(&doc(USERS), &doc(&format!("{USERS}{HEALTH}")));
    assert_eq!(diff(dir.path(), &[]).status.code(), Some(0));
    let frozen = diff(dir.path(), &["--fail-on", "non-breaking"]);
    assert_eq!(frozen.status.code(), Some(1));
    let bad = diff(dir.path(), &["--fail-on", "everything"]);
    assert_eq!(bad.status.code(), Some(2));
}

#[test]
fn base_and_a_second_document_conflict() {
    let dir = two_versions();
    let output = diff(dir.path(), &["--base", "main"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).contains("cannot be used with"),
        "{}",
        stderr(&output)
    );
}

const ALLOW_REMOVAL: &str = "- rule: operation-removed\n  operation: \"DELETE /users/{id}\"\n  \
    justification: \"Deprecated since 2026-06, no traffic\"\n";

#[test]
fn an_allowlisted_removal_is_reported_and_passes() {
    let dir = two_versions();
    write(dir.path(), "contracts/allowlist.yaml", ALLOW_REMOVAL);
    let output = diff(dir.path(), &[]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let out = stdout(&output);
    assert!(
        out.contains("breaking (allowed)  operation-removed  DELETE /users/{id}"),
        "{out}"
    );
    assert!(out.contains("  allowed: Deprecated since 2026-06, no traffic\n"));
    assert!(out.contains("1 breaking, 1 non-breaking, 0 unknown, 1 allowed"));
    assert!(!stderr(&output).contains("warning"), "{}", stderr(&output));
}

#[test]
fn an_entry_without_justification_fails_with_its_line() {
    let dir = two_versions();
    let text = format!("{ALLOW_REMOVAL}- rule: operation-added\n  operation: GET /health\n");
    write(dir.path(), "contracts/allowlist.yaml", &text);
    let output = diff(dir.path(), &[]);
    assert_eq!(output.status.code(), Some(2));
    let err = stderr(&output);
    assert!(
        err.contains("contracts/allowlist.yaml:4: justification is required"),
        "{err}"
    );
}

#[test]
fn an_expired_entry_warns_and_allows_nothing() {
    let dir = two_versions();
    let text = format!("{ALLOW_REMOVAL}  expires: 2026-01-01\n");
    write(dir.path(), "contracts/allowlist.yaml", &text);
    let output = diff(dir.path(), &[]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output)
            .contains("warning: contracts/allowlist.yaml:1: allowlist entry expired on 2026-01-01"),
        "{}",
        stderr(&output)
    );
    assert!(!stdout(&output).contains("(allowed)"));
}

#[test]
fn an_unused_entry_warns_without_changing_the_exit_code() {
    let dir = pair(&doc(USERS), &doc(&format!("{USERS}{HEALTH}")));
    let text = format!("{ALLOW_REMOVAL}  expires: 2999-01-01\n");
    write(dir.path(), "contracts/allowlist.yaml", &text);
    let output = diff(dir.path(), &[]);
    assert_eq!(output.status.code(), Some(0));
    assert!(
        stderr(&output).contains(
            "warning: contracts/allowlist.yaml:1: unused allowlist entry (operation-removed DELETE /users/{id})"
        ),
        "{}",
        stderr(&output)
    );
}

#[test]
fn an_explicit_allowlist_must_exist() {
    let dir = two_versions();
    let output = diff(dir.path(), &["--allowlist", "missing.yaml"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("missing.yaml: no such allowlist file"));
    write(dir.path(), "ci/allow.yaml", ALLOW_REMOVAL);
    let output = diff(dir.path(), &["--allowlist", "ci/allow.yaml"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
}

fn git(dir: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(["-c", "commit.gpgsign=false", "-c", "core.autocrlf=false"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {}", stderr(&output));
}

const ROOT_DOC: &str = "openapi: 3.1.0\ninfo: {title: t, version: '1'}\npaths:\n  /users/{id}:\n    \
    get:\n      parameters:\n        - {name: id, in: path, required: true, schema: {type: string}}\n      \
    responses:\n        '200':\n          description: ok\n          content:\n            \
    application/json:\n              schema: {$ref: 'schemas/user.yaml#/User'}\n";

/// A repository with two commits; the working tree drops `email` from the
/// referenced schema.
fn repository() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q"]);
    write(root, "api/openapi.yaml", &doc(USERS));
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "one"]);
    write(root, "api/openapi.yaml", ROOT_DOC);
    let user =
        "User:\n  type: object\n  properties:\n    id: {type: string}\n    email: {type: string}\n";
    write(root, "api/schemas/user.yaml", user);
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "two"]);
    write(
        root,
        "api/schemas/user.yaml",
        "User:\n  type: object\n  properties:\n    id: {type: string}\n",
    );
    dir
}

#[test]
fn base_reads_the_old_document_and_its_refs_at_the_ref() {
    let repo = repository();
    let output = rustman(repo.path())
        .args(["contract", "diff", "--base", "HEAD", "api/openapi.yaml"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    let out = stdout(&output);
    assert!(
        out.contains("breaking      response-field-removed  GET /users/{id}: response 200"),
        "{out}"
    );
    assert!(out.contains("$.email: removed"), "{out}");
    // From a subfolder, against the first commit: the operation is new.
    let output = rustman(&repo.path().join("api"))
        .args(["contract", "diff", "--base", "HEAD~1", "openapi.yaml"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(stdout(&output).contains("operation-added    GET /users/{id}"));
}

#[test]
fn base_with_a_missing_path_or_ref_exits_2() {
    let repo = repository();
    write(repo.path(), "openapi.yaml", &doc(USERS));
    let output = rustman(repo.path())
        .args(["contract", "diff", "--base", "HEAD", "openapi.yaml"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).contains("openapi.yaml not found at HEAD"),
        "{}",
        stderr(&output)
    );
    let output = rustman(repo.path())
        .args([
            "contract",
            "diff",
            "--base",
            "no-such-ref",
            "api/openapi.yaml",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let err = stderr(&output);
    assert!(err.contains("unknown git ref 'no-such-ref'"), "{err}");
    assert!(err.contains("git fetch origin main --depth=1"), "{err}");
}

#[test]
fn base_outside_a_repository_exits_2() {
    let dir = two_versions();
    let output = rustman(dir.path())
        .args(["contract", "diff", "--base", "HEAD", "v1.yaml"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("--base needs a git repository"));
}

#[test]
fn reports_are_written_in_every_format() {
    let dir = two_versions();
    let output = diff(
        dir.path(),
        &[
            "--report",
            "markdown=out/contract.md",
            "--report",
            "sarif=out/contract.sarif",
            "--report",
            "junit=out/junit.xml",
            "--report",
            "json=out/contract.json",
        ],
    );
    assert_eq!(output.status.code(), Some(1));
    let read = |name: &str| std::fs::read_to_string(dir.path().join("out").join(name)).unwrap();
    let markdown = read("contract.md");
    assert!(markdown.contains("**Summary:** 1 breaking, 1 non-breaking, 0 unknown."));
    let breaking = markdown.find("## Breaking (1)").unwrap();
    assert!(breaking < markdown.find("## Non-breaking (1)").unwrap());
    assert!(
        markdown.contains(
            "| `operation-removed` (failing) | `DELETE /users/{id}` | operation removed |"
        )
    );
    let sarif: serde_json::Value = serde_json::from_str(&read("contract.sarif")).unwrap();
    let result = &sarif["runs"][0]["results"][0];
    assert_eq!(result["level"], "error");
    let location = &result["locations"][0];
    assert_eq!(
        location["physicalLocation"]["artifactLocation"]["uri"],
        "v2.yaml"
    );
    assert_eq!(
        location["logicalLocations"][0]["fullyQualifiedName"],
        "/paths"
    );
    assert!(read("junit.xml").contains("type=\"operation-removed\""));
    let report = valid_report(read("contract.json").as_bytes());
    assert_eq!(report["kind"], "contract");
    assert_eq!(report["mode"], "diff");
}

#[test]
fn a_report_on_stdout_moves_the_summary_to_stderr() {
    let dir = two_versions();
    let output = diff(dir.path(), &["--report", "json"]);
    assert_eq!(output.status.code(), Some(1));
    valid_report(&output.stdout);
    assert!(stderr(&output).contains("operation-removed"));
    let two = diff(dir.path(), &["--report", "json", "--report", "sarif"]);
    assert_eq!(two.status.code(), Some(2));
}

#[test]
fn secrets_in_documents_never_reach_the_output() {
    const SECRET: &str = "tok-s3cr3t-canary";
    let enum_doc = |values: &str| {
        doc(&format!(
            "  /me:\n    get:\n      responses:\n        '200':\n          description: ok\n          \
             content:\n            application/json:\n              schema:\n                \
             type: object\n                properties:\n                  \
             plan: {{type: string, enum: [{values}]}}\n"
        ))
    };
    let dir = pair(&enum_doc("free"), &enum_doc(&format!("free, {SECRET}")));
    let root = dir.path();
    write(
        root,
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    write(root, "environments/prod.yaml", "secrets: [api_token]\n");
    write(root, ".gitignore", ".rustman/\n");
    let allow =
        format!("- rule: operation-removed\n  operation: GET /{SECRET}\n  justification: x\n");
    write(root, "contracts/allowlist.yaml", &allow);
    rustman(root)
        .args(["secret", "set", "api_token", "--env", "prod"])
        .write_stdin(SECRET)
        .assert()
        .success();
    let mut args = vec!["--env", "prod"];
    let formats = ["json", "markdown", "sarif", "junit", "html"];
    let targets: Vec<String> = formats.iter().map(|f| format!("{f}=out/r.{f}")).collect();
    for target in &targets {
        args.extend(["--report", target.as_str()]);
    }
    let output = diff(root, &args);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let out = stdout(&output);
    assert!(out.contains("«redacted:api_token»"), "{out}");
    let mut texts = vec![out, stderr(&output)];
    for format in formats {
        texts.push(std::fs::read_to_string(root.join(format!("out/r.{format}"))).unwrap());
    }
    for text in &texts {
        assert!(!text.contains(SECRET), "{text}");
    }
    assert!(texts[1].contains("unused allowlist entry"), "{}", texts[1]);
}

#[test]
fn report_render_reads_contract_reports_and_refuses_unknown_kinds() {
    let dir = two_versions();
    let output = diff(dir.path(), &["--report", "json=out/contract.json"]);
    assert_eq!(output.status.code(), Some(1));
    let render = |file: &str| {
        rustman(dir.path())
            .args(["report", "render", file, "--format", "markdown"])
            .output()
            .unwrap()
    };
    let rendered = render("out/contract.json");
    assert_eq!(rendered.status.code(), Some(0), "{}", stderr(&rendered));
    assert!(stdout(&rendered).starts_with("# Contract diff\n"));
    let text = std::fs::read_to_string(dir.path().join("out/contract.json")).unwrap();
    let newer = text.replace("\"kind\": \"contract\"", "\"kind\": \"contract-next\"");
    write(dir.path(), "out/newer.json", &newer);
    let refused = render("out/newer.json");
    assert_eq!(refused.status.code(), Some(2));
    assert!(
        stderr(&refused).contains("unknown report kind 'contract-next'"),
        "{}",
        stderr(&refused)
    );
}
