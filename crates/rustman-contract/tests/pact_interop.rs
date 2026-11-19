//! Interop with the official Pact verifier (add-contract-testing task 6.3):
//! a Pact file we generate is verified both by `rustman contract pact
//! verify`'s verifier and by `pact_verifier_cli`, against the same local
//! fixture provider, and the verdicts must match per interaction.
//!
//! The official verifier is not built here: the `pact-interop` CI job
//! downloads a pinned release (checksum verified) and names it in
//! `PACT_VERIFIER_CLI`. Without that variable the test does nothing.

use std::fs;
use std::path::Path;
use std::process::Command;

use rustman_contract::pact::{Generator, Pact, PactVersion, Verifier};
use rustman_contract::sender::{GovernedSenders, Targets};
use rustman_core::safety::{Permission, SafetySettings};
use rustman_core::vars::NoSecrets;
use rustman_core::{Redactor, Workspace};
use rustman_test_support::{GraphqlHttpServer, RecordedRequest, Reply};

const VERIFIER: &str = "PACT_VERIFIER_CLI";

async fn provider() -> GraphqlHttpServer {
    GraphqlHttpServer::start(|req: &RecordedRequest| {
        let json = "application/json";
        match req.uri.as_str() {
            "/users/42" => Reply::raw(200, json, r#"{"id":42,"name":"Ana","tags":["a","b"]}"#),
            "/users/7" => Reply::raw(200, json, r#"{"id":"42","name":"Bo","tags":[]}"#),
            "/health" => Reply::raw(503, json, "{}"),
            _ => Reply::raw(404, json, "{}"),
        }
    })
    .await
}

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// Requests whose assertions exercise every generated matcher; some pass
/// against the provider and some fail.
fn pact(root: &Path, base: &str, version: PactVersion) -> Pact {
    write(
        root,
        "rustman.toml",
        &format!("[workspace]\nname = \"t\"\nversion = 1\n[variables]\nbase = \"{base}\"\n"),
    );
    let requests = [
        (
            "user-equals",
            "/users/42",
            "- status: 200\n  - json: {path: \"$.id\", equals: 42}\n",
        ),
        (
            "user-as-text",
            "/users/7",
            "- status: 200\n  - json: {path: \"$.id\", equals: 42}\n",
        ),
        (
            "user-types",
            "/users/42",
            "- status: 2xx\n  - json: {path: \"$.id\", type: integer}\n  - json: {path: \"$.name\", type: string}\n",
        ),
        (
            "user-type-mismatch",
            "/users/7",
            "- status: 200\n  - json: {path: \"$.id\", type: integer}\n",
        ),
        (
            "user-regex",
            "/users/42",
            "- status: 200\n  - json: {path: \"$.name\", matches: \"^A\"}\n",
        ),
        (
            "user-regex-mismatch",
            "/users/7",
            "- status: 200\n  - json: {path: \"$.name\", matches: \"^A\"}\n",
        ),
        (
            "user-tags-length",
            "/users/42",
            "- status: 200\n  - json: {path: \"$.tags\", length: 2}\n",
        ),
        (
            "user-tags-too-short",
            "/users/7",
            "- status: 200\n  - json: {path: \"$.tags\", length: 2}\n",
        ),
        ("health", "/health", "- status: 200\n"),
    ];
    for (name, path, assertions) in requests {
        write(
            root,
            &format!("{name}.rman.yaml"),
            &format!(
                "version: 1\nkind: http\nname: {name}\nurl: \"{{{{base}}}}{path}\"\nassertions:\n  {assertions}"
            ),
        );
    }
    let ws = Workspace::load(root).unwrap_or_else(|e| panic!("{e}"));
    let generator = Generator {
        settings: &ws.settings,
        environment: None,
        secrets: &NoSecrets,
        redactor: &Redactor::new(),
        version,
        recorded_status: &|_: &str| None,
    };
    generator
        .generate("web", "users", &ws.requests())
        .unwrap()
        .pact
}

/// Each interaction's description and whether it passed, for our verifier.
async fn ours(pact: &Pact, base: &str) -> Vec<(String, bool)> {
    let targets = Targets {
        provider_url: base,
        states_url: None,
        permission: Permission::from_flags(false, false),
        production: None,
        environment: Default::default(),
    };
    let sender = GovernedSenders::new(&SafetySettings::default(), targets).unwrap();
    let verifier = Verifier {
        sender: &sender,
        provider_url: base,
        states_url: None,
    };
    let verification = verifier
        .verify(&[("p.json".into(), pact.clone())])
        .await
        .unwrap();
    verification
        .results
        .iter()
        .map(|r| (r.description.clone(), r.outcome.status() == "passed"))
        .collect()
}

/// The same, for the official verifier: one run per interaction.
fn official(binary: &str, file: &Path, port: u16, pact: &Pact) -> Vec<(String, bool)> {
    pact.interactions
        .iter()
        .map(|interaction| {
            let filter = format!("^{}$", regex::escape(&interaction.description));
            let output = Command::new(binary)
                .arg("--file")
                .arg(file)
                .args(["--hostname", "127.0.0.1", "--port", &port.to_string()])
                .args(["--filter-description", &filter])
                .output()
                .expect("pact_verifier_cli runs");
            (interaction.description.clone(), output.status.success())
        })
        .collect()
}

/// Our verdicts, which the official verifier must reproduce.
const EXPECTED: [(&str, bool); 9] = [
    ("health", false),
    ("user-as-text", false),
    ("user-equals", true),
    ("user-regex", true),
    ("user-regex-mismatch", false),
    ("user-tags-length", true),
    ("user-tags-too-short", false),
    ("user-type-mismatch", false),
    ("user-types", true),
];

/// Interactions left out of the comparison with the official verifier,
/// because the Pact implementations disagree on them (docs/contract.md,
/// "Interop with the official verifier").
///
/// `user-tags-too-short`: a `length` assertion becomes an empty example
/// array with a `type` matcher whose `min` and `max` are the length. The
/// specification compares the actual array's length with `min`/`max`
/// (V3 and V4 matcher tables); `pact_matching` applies no list matcher
/// when the example array is empty, so it passes any array there.
const NOT_COMPARED: [&str; 1] = ["user-tags-too-short"];

fn comparable(verdicts: &[(String, bool)]) -> Vec<(String, bool)> {
    verdicts
        .iter()
        .filter(|(description, _)| !NOT_COMPARED.contains(&description.as_str()))
        .cloned()
        .collect()
}

/// Our verdicts and, with `binary`, the official ones, for one version.
async fn verdicts(
    version: PactVersion,
    base: &str,
    port: u16,
    binary: Option<String>,
) -> (Vec<(String, bool)>, Option<Vec<(String, bool)>>) {
    let dir = tempfile::tempdir().unwrap();
    let pact = pact(dir.path(), base, version);
    let file = dir.path().join("web-users.json");
    fs::write(&file, pact.to_json().to_string()).unwrap();
    let mut ours = ours(&pact, base).await;
    ours.sort();
    let Some(binary) = binary else {
        return (ours, None);
    };
    let mut theirs = tokio::task::spawn_blocking(move || official(&binary, &file, port, &pact))
        .await
        .unwrap();
    theirs.sort();
    (ours, Some(theirs))
}

/// Every version is verified before anything is asserted, so a CI run
/// reports the disagreements of V3 and V4 together.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_official_verifier_agrees_with_ours() {
    let binary = std::env::var(VERIFIER).ok();
    if binary.is_none() {
        eprintln!("{VERIFIER} is not set: skipping the official verifier");
    }
    let server = provider().await;
    let base = server.url("");
    let port: u16 = base.rsplit(':').next().unwrap().parse().unwrap();
    let expected: Vec<(String, bool)> = EXPECTED
        .iter()
        .map(|(d, p)| ((*d).to_owned(), *p))
        .collect();
    let mut failures = Vec::new();
    for version in [PactVersion::V3, PactVersion::V4] {
        let (ours, theirs) = verdicts(version, &base, port, binary.clone()).await;
        if ours != expected {
            failures.push(format!(
                "Pact {version:?}: ours {ours:?}, expected {expected:?}"
            ));
        }
        let Some(theirs) = theirs else { continue };
        let (ours, theirs) = (comparable(&ours), comparable(&theirs));
        if ours != theirs {
            failures.push(format!(
                "Pact {version:?}: ours {ours:?}, pact_verifier_cli {theirs:?}"
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
