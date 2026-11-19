//! `rustman contract`: the command group (task 1.1), `rustman contract
//! rules` (task 2.4, spec "Listing rules") and the basic diff output and
//! exit codes. The documents are local fixtures; nothing is sent.

mod common;

use std::path::{Path, PathBuf};

use common::rustman;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/contract")
}

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

#[test]
fn contract_help_lists_its_commands() {
    let dir = tempfile::tempdir().unwrap();
    let output = rustman(dir.path())
        .args(["contract", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = stdout(&output);
    for command in ["diff", "rules", "pact", "verify"] {
        assert!(
            help.lines().any(|l| l.trim_start().starts_with(command)),
            "{command} missing from\n{help}"
        );
    }
}

#[test]
fn rules_json_is_an_array_with_every_rule_and_the_version() {
    let dir = tempfile::tempdir().unwrap();
    let output = rustman(dir.path())
        .args(["contract", "rules", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    let rules = json.as_array().unwrap();
    assert_eq!(rules.len(), rustman_contract::RuleId::ALL.len());
    for rule in rules {
        for key in ["id", "classification", "description", "example"] {
            assert!(rule[key].is_string(), "{key} in {rule}");
        }
        assert_eq!(rule["catalog_version"], rustman_contract::CATALOG_VERSION);
    }
    assert!(
        rules
            .iter()
            .any(|r| r["id"] == "operation-removed" && r["classification"] == "breaking")
    );
}

#[test]
fn rules_prints_a_table_with_the_version() {
    let dir = tempfile::tempdir().unwrap();
    let output = rustman(dir.path())
        .args(["contract", "rules"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let table = stdout(&output);
    assert!(table.starts_with(&format!(
        "rule catalog version {}\n",
        rustman_contract::CATALOG_VERSION
    )));
    assert!(table.contains("composition-changed"));
}

#[test]
fn a_breaking_difference_exits_1() {
    let corpus = fixtures().join("corpus/petstore");
    let output = rustman(&corpus)
        .args(["contract", "diff", "old.yaml", "new.yaml"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let out = stdout(&output);
    assert!(out.contains("breaking      operation-removed"), "{out}");
    assert!(
        out.contains("DELETE /pets/{petId}: operation removed"),
        "{out}"
    );
    assert!(out.contains("3 breaking, 5 non-breaking, 3 unknown (rule catalog version 2)"));
}

#[test]
fn no_breaking_difference_exits_0() {
    let corpus = fixtures().join("corpus/stripe");
    let output = rustman(&corpus)
        .args(["contract", "diff", "old.yaml", "new.yaml"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
    let same = rustman(&corpus)
        .args(["contract", "diff", "old.yaml", "old.yaml"])
        .output()
        .unwrap();
    assert_eq!(same.status.code(), Some(0));
    assert!(stdout(&same).starts_with("no differences\n"));
}

#[test]
fn an_invalid_document_prints_its_diagnostics_and_exits_2() {
    let dir = tempfile::tempdir().unwrap();
    let valid = "openapi: 3.1.0\ninfo: {title: t, version: '1'}\npaths: {}\n";
    let invalid = "openapi: 3.1.0\ninfo: {version: '1'}\npaths: {}\n";
    std::fs::write(dir.path().join("v1.yaml"), valid).unwrap();
    std::fs::write(dir.path().join("v2.yaml"), invalid).unwrap();
    let output = rustman(dir.path())
        .args(["contract", "diff", "v1.yaml", "v2.yaml"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("error: /info/title: required string 'title' is missing"),
        "{stderr}"
    );
}

#[test]
fn pact_and_verify_need_their_arguments() {
    let dir = tempfile::tempdir().unwrap();
    for (args, missing) in [
        (vec!["contract", "pact", "generate"], "--consumer <NAME>"),
        (vec!["contract", "pact", "verify"], "--provider-url <URL>"),
        (vec!["contract", "verify"], "--spec <OPENAPI>"),
    ] {
        let output = rustman(dir.path()).args(&args).output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains(missing), "{stderr}");
    }
}
