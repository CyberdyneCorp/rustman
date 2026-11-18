//! Postman collections and environments (tasks 2.1-2.4), over the fixture
//! corpus in `tests/fixtures/postman`.

mod common;

use common::{POSTMAN_FIXTURES, Setup, file, fixture, plan_fixture, tree};
use rustman_core::model::{KindRegistry, Request};
use rustman_import::ir::{ImportedAuth, Item, NoteKind};
use rustman_import::report::Action;
use rustman_import::{Format, detect};

const BILLING: &str = "postman/billing-api.postman_collection.json";

#[test]
fn every_fixture_is_detected_with_its_format() {
    for (name, format) in POSTMAN_FIXTURES {
        assert_eq!(detect(&fixture(name)), Some(*format), "{name}");
    }
}

#[test]
fn fixture_snapshots() {
    for (name, _) in POSTMAN_FIXTURES {
        let plan = plan_fixture(name);
        let stem = name.rsplit('/').next().unwrap().split('.').next().unwrap();
        let mut settings = insta::Settings::clone_current();
        settings.set_prepend_module_to_snapshot(false);
        settings.bind(|| {
            insta::assert_snapshot!(format!("postman-{stem}"), tree(&plan));
            insta::assert_snapshot!(
                format!("postman-{stem}-summary"),
                plan.report.summary_text()
            );
        });
    }
}

#[test]
fn every_written_request_file_loads() {
    let registry = KindRegistry::default();
    for (name, _) in POSTMAN_FIXTURES {
        for f in plan_fixture(name)
            .files
            .iter()
            .filter(|f| f.path.ends_with(".rman.yaml"))
        {
            let loaded = registry.load(std::path::Path::new(&f.path), &f.text);
            assert!(loaded.is_ok(), "{name}: {}: {loaded:?}\n{}", f.path, f.text);
        }
    }
}

#[test]
fn nested_folders_keep_their_order() {
    let plan = plan_fixture(BILLING);
    assert!(
        file(&plan, "billing-api/users/_collection.yaml")
            .contains("order:\n  - list\n  - create\n  - statement\n  - delete\n"),
        "{}",
        tree(&plan)
    );
    let root = file(&plan, "billing-api/_collection.yaml");
    assert!(
        root.starts_with("# imported-script: prerequest (not executed)\n"),
        "{root}"
    );
    assert!(root.contains("name: Billing API\n"), "{root}");
    assert!(
        root.contains("order:\n  - auth\n  - users\n  - invoices\n  - get-user\n  - get-user-2\n"),
        "{root}"
    );
}

#[test]
fn disabled_entries_are_dropped_and_reported() {
    let plan = plan_fixture(BILLING);
    let list = file(&plan, "billing-api/users/list.rman.yaml");
    assert!(!list.contains("X-Debug"), "{list}");
    assert!(!list.contains("debug"), "{list}");
    let dropped: Vec<(&str, Option<&str>)> = plan
        .report
        .notes
        .iter()
        .filter(|n| n.kind == NoteKind::Dropped)
        .map(|n| (n.message.as_str(), n.location.item.as_deref()))
        .collect();
    assert!(
        dropped.contains(&("disabled header X-Debug dropped", Some("Users / List"))),
        "{dropped:?}"
    );
    assert!(dropped.contains(&(
        "disabled query parameter debug dropped",
        Some("Users / List")
    )));
    assert!(dropped.contains(&("disabled form field otp dropped", Some("Auth / Login"))));
    assert!(dropped.contains(&("disabled variable legacy_host dropped", None)));
}

#[test]
fn colliding_names_get_suffixes() {
    let plan = plan_fixture(BILLING);
    assert!(file(&plan, "billing-api/get-user.rman.yaml").contains("url: '{{base_url}}/me'\n"));
    let second = file(&plan, "billing-api/get-user-2.rman.yaml");
    assert!(second.contains("name: get user!\n"), "{second}");
}

#[test]
fn graphql_bodies_become_graphql_files() {
    let plan = plan_fixture(BILLING);
    let text = file(&plan, "billing-api/payments-graphql.rman.yaml");
    assert!(text.contains("kind: graphql\n"), "{text}");
    assert!(
        text.contains("endpoint: '{{base_url}}/graphql'\n"),
        "{text}"
    );
    assert!(
        text.contains("document: |-\n  query Payments($first: Int) {\n"),
        "{text}"
    );
    assert!(text.contains("variables:\n  first: 10\n"), "{text}");
}

#[test]
fn unsupported_auth_is_written_as_none_and_reported_with_the_item_path() {
    let plan = plan_fixture(BILLING);
    let legacy = file(&plan, "billing-api/invoices/legacy-export.rman.yaml");
    // `none`, not absent: an absent auth would inherit the folder's
    // `X-Billing-Key: {{api_key}}`, which Postman never sent with hawk.
    assert!(legacy.contains("auth:\n  type: none\n"), "{legacy}");
    let note = plan
        .report
        .notes
        .iter()
        .find(|n| n.message == "hawk auth unsupported")
        .expect("hawk is reported");
    assert_eq!(note.kind, NoteKind::Unsupported);
    assert_eq!(
        note.location.item.as_deref(),
        Some("Invoices / Legacy export")
    );
    assert!(plan.report.summary_text().contains(
        "unsupported: hawk auth unsupported (billing-api.postman_collection.json > Invoices / Legacy export)\n"
    ));
}

#[test]
fn scripts_are_kept_as_comments_with_suggested_entries() {
    let plan = plan_fixture(BILLING);
    let login = file(&plan, "billing-api/auth/login.rman.yaml");
    assert!(
        login.starts_with(
            "# imported-script: prerequest (not executed)\n\
             # pm.environment.set('apiSecret', '«redacted:apisecret»');\n\
             # imported-script: test (not executed)\n"
        ),
        "{login}"
    );
    assert!(
        login.contains(
            "# pm.environment.set(\"refresh\", pm.response.headers.get(\"X-Refresh\"));\n"
        )
    );
    assert!(login.contains("assertions:\n  - status: 200\n"), "{login}");
    assert!(
        login.contains("captures:\n  - var: token\n    from: json\n    path: $.data.token\n"),
        "{login}"
    );
    let list = file(&plan, "billing-api/users/list.rman.yaml");
    assert!(
        list.contains("assertions:\n  - status: 200\n  - header:\n      name: X-Total-Count\n      exists: true\n"),
        "{list}"
    );
    let scripts: Vec<&str> = plan
        .report
        .notes
        .iter()
        .filter(|n| n.kind == NoteKind::Script)
        .map(|n| n.message.as_str())
        .collect();
    assert_eq!(
        scripts,
        [
            "script not executed: prerequest",
            "script not executed: prerequest",
            "script not executed: test",
            "script not executed: test"
        ]
    );
}

#[test]
fn body_modes_map_to_body_types() {
    let plan = plan_fixture("postman/body-modes.postman_collection.json");
    let body = |slug: &str| file(&plan, &format!("body-modes/{slug}.rman.yaml")).to_owned();
    assert!(body("raw-json").contains("body:\n  type: json\n  content: '{\"a\": 1}'\n"));
    assert!(body("raw-json-by-header").contains("type: json\n"));
    assert!(body("raw-xml").contains("body:\n  type: text\n  content: <a>1</a>\n"));
    assert!(body("urlencoded").contains("type: form\n  fields:\n    - name: a\n"));
    let multipart = body("formdata");
    assert!(multipart.contains("type: multipart\n"), "{multipart}");
    assert!(multipart.contains(
        "    - name: doc\n      file: files/doc.pdf\n      content_type: application/pdf\n"
    ));
    assert!(!multipart.contains("empty"), "{multipart}");
    assert!(body("file").contains("body:\n  type: file\n  path: payloads/body.bin\n"));
    let graphql = body("graphql-nested-variables");
    assert!(
        graphql.starts_with("# graphql-variables: {\"input\": {\"name\": \"x\"}}\n"),
        "{graphql}"
    );
    assert!(!body("unknown-mode").contains("body:"));
    let messages: Vec<&str> = plan
        .report
        .notes
        .iter()
        .map(|n| n.message.as_str())
        .collect();
    assert!(
        messages.contains(&"body mode 'binary-stream' unsupported"),
        "{messages:?}"
    );
    assert!(
        messages.contains(&"form part empty has no file; dropped"),
        "{messages:?}"
    );
}

#[test]
fn auth_types_map_or_are_reported() {
    let plan = plan_fixture("postman/auth-types.postman_collection.json");
    let auth = |slug: &str| file(&plan, &format!("auth-types/{slug}.rman.yaml")).to_owned();
    assert!(auth("no-auth").contains("auth:\n  type: none\n"));
    assert!(
        auth("basic").contains("auth:\n  type: basic\n  username: u\n  password: '{{password}}'\n")
    );
    assert!(auth("bearer").contains("auth:\n  type: bearer\n  token: '{{token}}'\n"));
    assert!(
        auth("api-key-in-query")
            .contains("type: api_key\n  name: api_key\n  value: '{{api_key}}'\n  in: query\n")
    );
    let cc = auth("oauth2-client-credentials");
    assert!(cc.contains("grant: client_credentials\n  token_url: https://auth/token\n  client_id: cid\n  client_secret: '{{client_secret}}'\n  scopes:\n    - a\n    - b\n"), "{cc}");
    let code = auth("oauth2-authorization-code");
    assert!(code.contains("grant: authorization_code\n  token_url: https://auth/token\n  auth_url: https://auth/authorize\n"), "{code}");
    let unsupported: Vec<&str> = plan
        .report
        .notes
        .iter()
        .filter(|n| n.kind == NoteKind::Unsupported)
        .map(|n| n.message.as_str())
        .collect();
    assert_eq!(
        unsupported,
        [
            "oauth2 auth with grant 'implicit' unsupported",
            "hawk auth unsupported",
            "digest auth unsupported",
            "awsv4 auth unsupported",
            "ntlm auth unsupported",
        ]
    );
}

#[test]
fn version_2_0_exports_are_read() {
    let text = fixture("postman/legacy-v20.postman_collection.json");
    let collection = Format::Postman.parse(&text, "legacy.json").unwrap();
    let Item::Request(status) = &collection.root.items[0] else {
        panic!("expected a request");
    };
    assert_eq!(
        (status.method.as_str(), status.url.as_str()),
        ("GET", "https://legacy.example.com/status")
    );
    assert_eq!(status.query[0].name, "verbose");
    let Item::Request(search) = &collection.root.items[1] else {
        panic!("expected a request");
    };
    assert_eq!(search.method, "POST");
    assert_eq!(search.headers.len(), 2);
    assert!(
        matches!(&search.auth, Some(ImportedAuth::Bearer { token }) if token.starts_with("CANARY"))
    );
    let Item::Request(built) = &collection.root.items[2] else {
        panic!("expected a request");
    };
    assert_eq!(built.url, "https://api.legacy.example.com:8443/v2/items");
    assert!(matches!(
        collection.root.auth,
        Some(ImportedAuth::Basic { .. })
    ));
}

#[test]
fn path_variables_become_placeholders_with_request_variables() {
    let plan = plan_fixture(BILLING);
    let delete = file(&plan, "billing-api/users/delete.rman.yaml");
    assert!(
        delete.contains("url: '{{base_url}}/users/{{id}}'\n"),
        "{delete}"
    );
    assert!(delete.contains("variables:\n  id: '42'\n"), "{delete}");
}

#[test]
fn the_staging_environment_stores_api_key_as_a_secret() {
    let plan = plan_fixture("postman/staging.postman_environment.json");
    assert_eq!(plan.report.destination, None);
    let env = file(&plan, "environments/staging.yaml");
    assert_eq!(
        env,
        "variables:\n  base_url: https://staging.billing.example.com/v1\n  \
         request_signing_salt: a8F3k2L9q7W1z5X0c4V6b8N2m1\nsecrets:\n  - api_key\n  - admin_password\n"
    );
    let stored: Vec<(&str, &str)> = plan
        .secrets
        .iter()
        .map(|m| (m.environment.as_str(), m.secret.name()))
        .collect();
    assert_eq!(
        stored,
        [("staging", "api_key"), ("staging", "admin_password")]
    );
    assert_eq!(plan.secrets[0].secret.expose(), "CANARY-env-apikey-2468");
    // The long value that stayed a plain variable is flagged for review.
    assert!(
        plan.report
            .notes
            .iter()
            .any(|n| n.kind == NoteKind::Review && n.message.contains("request_signing_salt"))
    );
}

#[test]
fn the_written_environment_and_requests_load_as_a_workspace() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("rustman.toml"),
        "[workspace]\nname = \"t\"\nversion = 1\n",
    )
    .unwrap();
    let store = std::sync::Mutex::new(Vec::new());
    for name in [BILLING, "postman/staging.postman_environment.json"] {
        let plan = Setup::new(dir.path()).plan(name);
        common::apply(&plan, dir.path(), &store);
    }
    let ws = rustman_core::Workspace::load(dir.path()).unwrap();
    let names: Vec<String> = ws
        .requests()
        .iter()
        .map(|r| r.request().name().to_owned())
        .collect();
    assert!(
        names.contains(&"Payments (GraphQL)".to_owned()),
        "{names:?}"
    );
    assert_eq!(names.len(), 12);
    let local = ws.environments.iter().find(|e| e.name == "local").unwrap();
    for secret in &local.secrets {
        assert!(
            store
                .lock()
                .unwrap()
                .iter()
                .any(|(env, name, _)| env == "local" && name == secret)
        );
    }
    let staging = ws
        .environments
        .iter()
        .find(|e| e.name == "staging")
        .unwrap();
    assert_eq!(staging.secrets, ["api_key", "admin_password"]);
    let Request::Http(list) = ws
        .requests()
        .into_iter()
        .find(|r| r.request().name() == "List")
        .unwrap()
        .request()
        .clone()
    else {
        panic!("http");
    };
    assert_eq!(list.effective_method(), "GET");
    assert!(replan_skips_every_collection_file(dir.path()));
}

/// Re-planning the same source over the written files skips every file.
fn replan_skips_every_collection_file(root: &std::path::Path) -> bool {
    let plan = Setup::new(root).plan(BILLING);
    plan.files
        .iter()
        .filter(|f| f.path.starts_with("billing-api/"))
        .all(|f| f.action == Action::Unchanged)
}

#[test]
fn generated_paths_beyond_the_limit_fail_with_a_short_message() {
    let mut item =
        serde_json::json!({"name": "leaf", "request": {"method": "GET", "url": "https://x.test"}});
    for depth in 0..40 {
        item = serde_json::json!({"name": format!("{depth:02}{}", "f".repeat(62)), "item": [item]});
    }
    let collection = serde_json::json!({
        "info": {"name": "Deep", "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"},
        "item": [item]
    });
    let parsed = rustman_import::Format::Postman
        .parse(&collection.to_string(), "deep.json")
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let options = rustman_import::PlanOptions {
        root: dir.path(),
        source: "deep.json".into(),
        format: rustman_import::Format::Postman,
        destination: None,
        on_conflict: rustman_import::OnConflict::Skip,
        environment: None,
        existing: rustman_import::secrets::Existing::none(),
        dry_run: true,
    };
    let err = rustman_import::plan(parsed, &options)
        .unwrap_err()
        .to_string();
    assert!(
        err.starts_with("import: deep.json: generated path too long ("),
        "{err}"
    );
    assert!(err.len() < 300, "{} bytes: {err}", err.len());
}
