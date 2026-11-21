//! The shared stages (tasks 1.4-1.6): no secret value in any output,
//! deterministic trees, the conflict policy and the JSON report.

mod common;

use std::fs;
use std::path::Path;
use std::sync::Mutex;

use common::{POSTMAN_FIXTURES, Setup, apply, file, plan_fixture, read_tree, tree};
use rustman_core::model::{VarValue, Variables};
use rustman_core::{Environment, Secret};
use rustman_import::OnConflict;
use rustman_import::ir::NoteKind;
use rustman_import::report::Action;

const BILLING: &str = "postman/billing-api.postman_collection.json";

/// Every value the fixtures inject as a secret contains this marker.
const CANARY: &str = "CANARY";

#[test]
fn no_fixture_output_contains_an_injected_secret() {
    for (name, _) in POSTMAN_FIXTURES {
        let plan = plan_fixture(name);
        for f in &plan.files {
            assert!(!f.text.contains(CANARY), "{name}: {}\n{}", f.path, f.text);
        }
        assert!(!plan.report.to_json().contains(CANARY), "{name}");
        assert!(!plan.report.summary_text().contains(CANARY), "{name}");
        assert!(!format!("{plan:?}").contains(CANARY), "{name}");
        assert!(!tree(&plan).contains("eyJhbGciOiJIUzI1NiJ9"), "{name}");
    }
}

#[test]
fn every_injected_secret_is_stored_by_name() {
    let plan = plan_fixture(BILLING);
    let mut values: Vec<&str> = plan
        .secrets
        .iter()
        .map(|m| m.secret.expose().as_str())
        .collect();
    values.sort_unstable();
    assert_eq!(
        values,
        [
            "CANARY-basic-password-5566",
            "CANARY-body-password-2468",
            "CANARY-collection-apikey-4d5e6f",
            "CANARY-header-xapikey-1122",
            "CANARY-login-password-8c9d",
            "CANARY-oauth-client-secret-9900",
            "CANARY-path-token-3690",
            "CANARY-script-only-secret-8024",
            "CANARY-userinfo-pass-1470",
            "CANARY-webhook-secret-0a1b2c",
            "eyJhbGciOiJIUzI1NiJ9.CANARY-literal-jwt.sig",
            "session=CANARY-cookie-session-3344; theme=dark",
            "sk_live_CANARY_collection_bearer_7f3a9c2e",
        ]
    );
    // The OAuth2 access token Postman cached is not carried over at all.
    assert!(!values.iter().any(|v| v.contains("access-token")));
}

#[test]
fn applying_writes_no_secret_into_the_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let store = Mutex::new(Vec::new());
    for (name, _) in POSTMAN_FIXTURES {
        let plan = Setup::new(dir.path()).plan(name);
        apply(&plan, dir.path(), &store);
    }
    for (path, bytes) in read_tree(dir.path()) {
        let text = String::from_utf8(bytes).unwrap();
        assert!(!text.contains(CANARY), "{path}\n{text}");
    }
    assert!(
        store
            .lock()
            .unwrap()
            .iter()
            .any(|(_, _, v)| v.contains(CANARY))
    );
}

#[test]
fn importing_twice_into_empty_roots_is_byte_identical() {
    let store = Mutex::new(Vec::new());
    let trees: Vec<_> = (0..2)
        .map(|_| {
            let dir = tempfile::tempdir().unwrap();
            for (name, _) in POSTMAN_FIXTURES {
                apply(&Setup::new(dir.path()).plan(name), dir.path(), &store);
            }
            read_tree(dir.path())
        })
        .collect();
    assert!(trees[0].len() > 20);
    assert_eq!(trees[0], trees[1]);
}

type Stored = Mutex<Vec<(String, String, String)>>;

/// Import the billing fixture, then edit `billing-api/users/list.rman.yaml`.
fn imported_and_edited(root: &Path, store: &Stored) -> String {
    apply(&Setup::new(root).plan(BILLING), root, store);
    let path = root.join("billing-api/users/list.rman.yaml");
    let edited = fs::read_to_string(&path).unwrap() + "tags: [mine]\n";
    fs::write(&path, &edited).unwrap();
    edited
}

/// Import the billing fixture again, with the workspace's environments
/// and the secrets stored so far.
fn reimport(root: &Path, policy: OnConflict, store: &Stored) -> rustman_import::Plan {
    reimport_into(root, policy, store, Some("local"))
}

fn reimport_into(
    root: &Path,
    policy: OnConflict,
    store: &Stored,
    environment: Option<&str>,
) -> rustman_import::Plan {
    let ws = rustman_core::Workspace::load(root).unwrap();
    let stored = store.lock().unwrap().clone();
    let lookup = move |env: &str, name: &str| {
        stored
            .iter()
            .find(|(e, n, _)| e == env && n == name)
            .map(|(_, n, v)| Secret::new(n, v.clone()))
    };
    let mut setup = Setup::new(root);
    setup.policy = policy;
    setup.environment = environment;
    setup.environments = &ws.environments;
    setup.lookup = &lookup;
    let plan = setup.plan(BILLING);
    apply(&plan, root, store);
    plan
}

fn conflict_for<'p>(plan: &'p rustman_import::Plan, path: &str) -> &'p str {
    let found = plan.report.conflicts.iter().find(|c| c.path == path);
    &found
        .unwrap_or_else(|| panic!("no conflict for {path}: {:?}", plan.report.conflicts))
        .resolution
}

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("rustman.toml"),
        "[workspace]\nname = \"t\"\nversion = 1\n",
    )
    .unwrap();
    dir
}

#[test]
fn skip_keeps_an_edited_file() {
    let dir = workspace();
    let store = Mutex::new(Vec::new());
    let edited = imported_and_edited(dir.path(), &store);
    let plan = reimport(dir.path(), OnConflict::Skip, &store);
    let path = "billing-api/users/list.rman.yaml";
    assert_eq!(fs::read_to_string(dir.path().join(path)).unwrap(), edited);
    assert_eq!(conflict_for(&plan, path), "skipped (exists)");
    assert!(
        plan.report
            .summary_text()
            .contains(&format!("conflict: {path} skipped (exists)\n"))
    );
    assert_eq!(plan.report.summary.requests, 0);
    // Same values, same names: nothing new to store, no duplicate secret.
    assert!(plan.secrets.is_empty());
    let env = plan
        .files
        .iter()
        .find(|f| f.path == "environments/local.yaml")
        .unwrap();
    assert_eq!(env.action, Action::Skip);
}

#[test]
fn overwrite_replaces_an_edited_file() {
    let dir = workspace();
    let store = Mutex::new(Vec::new());
    imported_and_edited(dir.path(), &store);
    let plan = reimport(dir.path(), OnConflict::Overwrite, &store);
    let path = "billing-api/users/list.rman.yaml";
    let text = fs::read_to_string(dir.path().join(path)).unwrap();
    assert!(!text.contains("tags"), "{text}");
    assert_eq!(conflict_for(&plan, path), "overwritten");
    // Unchanged files are not rewritten.
    let create = plan
        .files
        .iter()
        .find(|f| f.path == "billing-api/users/create.rman.yaml")
        .unwrap();
    assert_eq!(create.action, Action::Unchanged);
}

#[test]
fn rename_reports_unchanged_files_as_unchanged_not_as_conflicts() {
    let dir = workspace();
    let store = Mutex::new(Vec::new());
    imported_and_edited(dir.path(), &store);
    let plan = reimport(dir.path(), OnConflict::Rename, &store);
    let create = "billing-api/users/create.rman.yaml";
    let entry = plan.files.iter().find(|f| f.path == create).unwrap();
    assert_eq!(entry.action, Action::Unchanged);
    assert!(
        plan.report.conflicts.iter().all(|c| c.path != create),
        "{:?}",
        plan.report.conflicts
    );
    let summary = plan.report.summary_text();
    assert!(
        !summary.contains(&format!("conflict: {create}")),
        "{summary}"
    );
    let file_skips = summary
        .lines()
        .filter(|l| l.starts_with("conflict: billing-api/") && l.ends_with("skipped (exists)"));
    assert_eq!(file_skips.count(), 0, "{summary}");
    assert!(summary.contains(" files already up to date\n"), "{summary}");
    assert!(
        !dir.path()
            .join("billing-api/users/create-2.rman.yaml")
            .exists()
    );
}

#[test]
fn rename_writes_next_to_an_edited_file() {
    let dir = workspace();
    let store = Mutex::new(Vec::new());
    let edited = imported_and_edited(dir.path(), &store);
    let plan = reimport(dir.path(), OnConflict::Rename, &store);
    let path = "billing-api/users/list.rman.yaml";
    assert_eq!(fs::read_to_string(dir.path().join(path)).unwrap(), edited);
    assert_eq!(
        conflict_for(&plan, path),
        "renamed to billing-api/users/list-2.rman.yaml"
    );
    assert!(
        dir.path()
            .join("billing-api/users/list-2.rman.yaml")
            .exists()
    );
    // The renamed request runs where the source puts it: after the
    // existing entries, in the folder's order.
    let order = "billing-api/users/_collection.yaml";
    assert_eq!(conflict_for(&plan, order), "order extended with list-2");
    let text = fs::read_to_string(dir.path().join(order)).unwrap();
    assert!(
        text.contains("order:\n  - list\n  - create\n  - statement\n  - delete\n  - list-2\n"),
        "{text}"
    );
    // The workspace still loads, with both files.
    let ws = rustman_core::Workspace::load(dir.path()).unwrap();
    assert_eq!(ws.find_by_name("List").len(), 2);
}

#[test]
fn a_failed_write_stores_no_secret_and_leaves_no_file() {
    let dir = workspace();
    let plan = Setup::new(dir.path()).plan(BILLING);
    // A file where a folder of the plan must go makes a late write fail.
    fs::create_dir(dir.path().join("billing-api")).unwrap();
    fs::write(dir.path().join("billing-api/users"), "in the way").unwrap();
    let before = read_tree(dir.path());
    let store = Mutex::new(Vec::new());
    let save = |batch: &[(&str, &Secret<String>)]| {
        for (env, secret) in batch {
            store.lock().unwrap().push((
                (*env).to_owned(),
                secret.name().to_owned(),
                String::new(),
            ));
        }
        Ok(())
    };
    assert!(rustman_import::apply(&plan, dir.path(), &save).is_err());
    assert!(store.lock().unwrap().is_empty());
    assert_eq!(read_tree(dir.path()), before);
}

#[test]
fn a_failed_secret_store_leaves_no_file() {
    let dir = workspace();
    let plan = Setup::new(dir.path()).plan(BILLING);
    let before = read_tree(dir.path());
    let fail =
        |_: &[(&str, &Secret<String>)]| Err(rustman_core::SecretError::Keychain("locked".into()));
    assert!(rustman_import::apply(&plan, dir.path(), &fail).is_err());
    assert_eq!(read_tree(dir.path()), before);
    assert!(!dir.path().join("billing-api").exists());
}

#[test]
fn reimport_reuses_secret_names_with_the_same_values() {
    let dir = workspace();
    let store = Mutex::new(Vec::new());
    apply(&Setup::new(dir.path()).plan(BILLING), dir.path(), &store);
    let before = read_tree(dir.path());
    let plan = reimport(dir.path(), OnConflict::Skip, &store);
    assert!(
        plan.files.iter().all(|f| !f.action.writes()),
        "{}",
        tree(&plan)
    );
    assert!(plan.secrets.is_empty());
    assert_eq!(read_tree(dir.path()), before);
}

#[test]
fn reimport_without_an_active_environment_reports_nothing_new() {
    let dir = workspace();
    let store = Mutex::new(Vec::new());
    reimport_into(dir.path(), OnConflict::Skip, &store, None);
    let stored = store.lock().unwrap().len();
    let plan = reimport_into(dir.path(), OnConflict::Skip, &store, None);
    assert_eq!(store.lock().unwrap().len(), stored);
    assert!(plan.secrets.is_empty());
    let summary = plan.report.summary_text();
    assert!(!summary.contains("new environment"), "{summary}");
    assert!(
        summary.contains("written: 0 requests, 0 collections, 0 environments;"),
        "{summary}"
    );
    assert!(summary.contains("secrets moved: 0;"), "{summary}");
}

/// When the stored values cannot be read back, the moved secrets get new
/// names in the existing environment, which is updated, not new.
#[test]
fn reimport_with_an_unreadable_store_never_claims_a_new_environment() {
    let dir = workspace();
    let store = Mutex::new(Vec::new());
    reimport_into(dir.path(), OnConflict::Skip, &store, None);
    let ws = rustman_core::Workspace::load(dir.path()).unwrap();
    let mut setup = Setup::new(dir.path());
    setup.environment = None;
    setup.environments = &ws.environments;
    let plan = setup.plan(BILLING);
    let summary = plan.report.summary_text();
    assert!(!summary.contains("new environment"), "{summary}");
    let env = plan
        .files
        .iter()
        .find(|f| f.path == "environments/billing-api.yaml")
        .unwrap();
    assert_eq!(env.action, Action::Update);
}

#[test]
fn a_different_existing_secret_is_never_replaced() {
    let envs = [Environment {
        name: "local".into(),
        variables: Variables::new(vec![("base".into(), VarValue::String("x".into()))]),
        secrets: vec!["token".into()],
        production: false,
        trace: Default::default(),
        chain_id: None,
    }];
    let lookup = |_: &str, n: &str| Some(Secret::new(n, "other".to_owned()));
    let dir = tempfile::tempdir().unwrap();
    let mut setup = Setup::new(dir.path());
    setup.environments = &envs;
    setup.lookup = &lookup;
    let plan = setup.plan(BILLING);
    assert!(file(&plan, "billing-api/_collection.yaml").contains("token: '{{token_2}}'"));
    let env = plan
        .files
        .iter()
        .find(|f| f.path == "environments/local.yaml")
        .unwrap();
    assert_eq!(env.action, Action::Update);
    assert!(
        env.text
            .starts_with("variables:\n  base: x\nsecrets:\n  - token\n  - api_key\n"),
        "{}",
        env.text
    );
}

#[test]
fn without_an_active_environment_secrets_go_to_a_new_one_named_after_the_collection() {
    let dir = tempfile::tempdir().unwrap();
    let mut setup = Setup::new(dir.path());
    setup.environment = None;
    let plan = setup.plan(BILLING);
    assert!(plan.secrets.iter().all(|m| m.environment == "billing-api"));
    assert!(file(&plan, "environments/billing-api.yaml").contains("secrets:\n"));
    let info = plan
        .report
        .notes
        .iter()
        .find(|n| n.kind == NoteKind::Info)
        .unwrap();
    assert_eq!(
        info.message,
        "secrets stored in new environment 'billing-api' (no active environment)"
    );
    assert!(
        plan.report
            .summary_text()
            .contains("secrets stored in new environment 'billing-api'")
    );
}

#[test]
fn the_json_report_validates_and_matches_the_summary() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/schemas/import-report.schema.json");
    let schema: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    for (name, _) in POSTMAN_FIXTURES {
        let plan = plan_fixture(name);
        let report: serde_json::Value = serde_json::from_str(&plan.report.to_json()).unwrap();
        let errors: Vec<String> = validator
            .iter_errors(&report)
            .map(|e| e.to_string())
            .collect();
        assert!(errors.is_empty(), "{name}: {errors:#?}");
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
        assert!(plan.report.summary_text().contains(&line), "{name}: {line}");
        let files = report["files"].as_array().unwrap();
        let requests = files
            .iter()
            .filter(|f| {
                f["kind"] == "request" && f["action"] != "skip" && f["action"] != "unchanged"
            })
            .count();
        assert_eq!(s["requests"], requests);
        assert_eq!(
            s["secrets_moved"],
            report["secrets"].as_array().unwrap().len()
        );
    }
}

#[test]
fn the_report_lists_secret_names_and_locations_only() {
    let plan = plan_fixture("postman/staging.postman_environment.json");
    let summary = plan.report.summary_text();
    assert!(summary.contains(
        "secret: api_key -> environment 'staging' (staging.postman_environment.json > Staging)\n"
    ));
    assert!(summary.contains("secret: admin_password -> environment 'staging'"));
    let json = plan.report.to_json();
    for secret in &plan.secrets {
        assert!(!summary.contains(secret.secret.expose().as_str()));
        assert!(!json.contains(secret.secret.expose().as_str()));
    }
}
