//! The spec scenarios of the Insomnia, Bruno, HAR and HTTP importers
//! (tasks 3.1-4.2).

mod common;

use common::{Setup, file, fixture_path, plan_source, tree};
use rustman_import::ir::{ImportedBody, Item, NoteKind};
use rustman_import::{Format, HarFilter, ImportError, Plan, SourceOptions, load};

fn note_messages(plan: &Plan, kind: NoteKind) -> Vec<String> {
    plan.report
        .notes
        .iter()
        .filter(|n| n.kind == kind)
        .map(|n| n.message.clone())
        .collect()
}

fn request_files(plan: &Plan) -> Vec<&str> {
    plan.files
        .iter()
        .filter(|f| f.path.ends_with(".rman.yaml"))
        .map(|f| f.path.as_str())
        .collect()
}

// --- Insomnia ------------------------------------------------------------

#[test]
fn insomnia_sub_environments_are_flattened() {
    for name in ["insomnia/shop-v4.json", "insomnia/shop-v5.yaml"] {
        let plan = plan_source(name);
        let prod = file(&plan, "environments/prod.yaml");
        assert!(
            prod.contains("base_url: https://shop.example.com\n"),
            "{name}: {prod}"
        );
        assert!(!prod.contains("localhost"), "{name}: {prod}");
    }
}

#[test]
fn insomnia_underscore_variables_are_translated() {
    for name in ["insomnia/shop-v4.json", "insomnia/shop-v5.yaml"] {
        let plan = plan_source(name);
        let list = file(&plan, "shop-api/users/list-users.rman.yaml");
        assert!(
            list.contains("url: '{{base_url}}/users'\n"),
            "{name}: {list}"
        );
    }
}

#[test]
fn insomnia_template_tags_are_reported_with_the_request_name() {
    for name in ["insomnia/shop-v4.json", "insomnia/shop-v5.yaml"] {
        let plan = plan_source(name);
        let note = plan
            .report
            .notes
            .iter()
            .find(|n| n.message == "unsupported template tag: response")
            .unwrap_or_else(|| panic!("{name}: {:?}", plan.report.notes));
        assert_eq!(note.kind, NoteKind::Unsupported);
        assert_eq!(note.location.item.as_deref(), Some("Users / Get user"));
    }
}

#[test]
fn insomnia_groups_order_graphql_and_secrets() {
    let plan = plan_source("insomnia/shop-v4.json");
    assert!(
        file(&plan, "shop-api/users/_collection.yaml")
            .contains("order:\n  - create-user\n  - list-users\n  - get-user\n")
    );
    let graphql = file(&plan, "shop-api/search-products.rman.yaml");
    assert!(graphql.contains("kind: graphql\n"), "{graphql}");
    assert!(graphql.contains("variables:\n  q: lamp\n"), "{graphql}");
    let create = file(&plan, "shop-api/users/create-user.rman.yaml");
    assert!(create.contains("token: '{{token}}'"), "{create}");
    assert!(create.contains("{{$uuid}}"), "{create}");
    assert!(note_messages(&plan, NoteKind::Unsupported).contains(&"hawk auth unsupported".into()));
}

#[test]
fn insomnia_unsupported_auth_disables_inherited_auth() {
    // `Get user` uses hawk inside a folder of the workspace.
    let plan = plan_source("insomnia/shop-v4.json");
    let get = file(&plan, "shop-api/users/get-user.rman.yaml");
    assert!(get.contains("auth:\n  type: none\n"), "{get}");
}

/// Every workspace file outside `.rustman/` after applying `plan`, and the
/// stored secret values.
fn applied(name: &str) -> (Vec<(String, String)>, Vec<String>) {
    let dir = tempfile::tempdir().unwrap();
    let store = std::sync::Mutex::new(Vec::new());
    let plan = Setup::new(dir.path()).plan_source(name, &SourceOptions::default());
    common::apply(&plan, dir.path(), &store);
    let files = common::read_tree(dir.path())
        .into_iter()
        .filter(|(path, _)| !path.starts_with(".rustman/"))
        .map(|(path, bytes)| (path, String::from_utf8(bytes).unwrap()))
        .collect();
    let stored = store.lock().unwrap().iter().map(|s| s.2.clone()).collect();
    (files, stored)
}

#[test]
fn insomnia_private_environment_values_are_never_written_to_the_workspace() {
    for (name, canary) in [
        ("insomnia/shop-v4.json", "CANARY-insomnia-private-plain-v4"),
        ("insomnia/shop-v5.yaml", "CANARY-insomnia-private-plain-v5"),
    ] {
        let (files, stored) = applied(name);
        for (path, text) in &files {
            assert!(!text.contains(canary), "{name}: {path}\n{text}");
        }
        assert!(stored.iter().any(|s| s == canary), "{name}: {stored:?}");
        let private = &files
            .iter()
            .find(|(p, _)| p == "environments/private.yaml")
            .unwrap_or_else(|| panic!("{name}: no private environment"))
            .1;
        assert!(private.contains("\n  - plain\n"), "{name}: {private}");
        assert!(!private.contains("plain:"), "{name}: {private}");
    }
}

#[test]
fn bruno_names_listed_as_secret_are_never_written_with_their_value() {
    let canary = "CANARY-bruno-listed-secret-77";
    let (files, stored) = applied("bruno/shop");
    for (path, text) in &files {
        assert!(!text.contains(canary), "{path}\n{text}");
    }
    assert!(stored.iter().any(|s| s == canary), "{stored:?}");
    let local = &files
        .iter()
        .find(|(p, _)| p == "environments/local.yaml")
        .unwrap()
        .1;
    assert!(local.contains("  - session_ref\n"), "{local}");
}

#[test]
fn an_insomnia_export_that_is_neither_version_is_invalid() {
    let err = Format::Insomnia
        .parse(
            "{\"_type\": \"export\", \"__export_format\": 3}",
            "old.json",
        )
        .unwrap_err();
    assert!(matches!(err, ImportError::Invalid { .. }), "{err}");
    let err = Format::Insomnia
        .parse("type: something/else\n", "x.yaml")
        .unwrap_err();
    assert!(err.to_string().contains("not an Insomnia"), "{err}");
}

// --- Bruno ---------------------------------------------------------------

#[test]
fn bruno_status_assertion_maps_to_a_status_check() {
    let plan = plan_source("bruno/shop");
    let health = file(&plan, "shop-bruno/health.rman.yaml");
    assert!(
        health.contains("assertions:\n  - status: 200\n"),
        "{health}"
    );
}

#[test]
fn bruno_seq_orders_requests_and_folders() {
    let plan = plan_source("bruno/shop");
    assert!(file(&plan, "shop-bruno/_collection.yaml").contains("order:\n  - health\n  - users\n"));
    assert!(
        file(&plan, "shop-bruno/users/_collection.yaml")
            .contains("order:\n  - create-user\n  - list-users\n  - upload-avatar\n")
    );
}

/// A copy of the Bruno `shop` fixture in a temporary folder.
fn bruno_copy() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let src = fixture_path("bruno/shop");
    let mut stack = vec![src.clone()];
    while let Some(from) = stack.pop() {
        let to = dir.path().join(from.strip_prefix(&src).unwrap());
        std::fs::create_dir_all(&to).unwrap();
        for entry in std::fs::read_dir(&from).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                std::fs::copy(&path, to.join(path.file_name().unwrap())).unwrap();
            }
        }
    }
    dir
}

fn load_bruno(
    dir: &std::path::Path,
) -> Result<rustman_import::ir::ImportedCollection, ImportError> {
    load(Some(Format::Bruno), dir, "shop", &SourceOptions::default()).map(|(_, c)| c)
}

fn count_requests(folder: &rustman_import::ir::Folder) -> usize {
    folder
        .items
        .iter()
        .map(|item| match item {
            Item::Request(_) => 1,
            Item::Folder(f) => count_requests(f),
        })
        .sum()
}

#[cfg(unix)]
#[test]
fn bruno_symlinked_folders_are_not_followed() {
    let dir = bruno_copy();
    let expected = count_requests(&load_bruno(dir.path()).unwrap().root);
    let users = dir.path().join("users");
    std::os::unix::fs::symlink("..", users.join("loop1")).unwrap();
    std::os::unix::fs::symlink("..", users.join("loop2")).unwrap();
    let collection = load_bruno(dir.path()).unwrap();
    assert_eq!(count_requests(&collection.root), expected);
    let dropped: Vec<&str> = collection
        .notes
        .iter()
        .filter(|n| n.kind == NoteKind::Dropped && n.message.starts_with("symlinked"))
        .map(|n| n.message.as_str())
        .collect();
    assert_eq!(
        dropped,
        [
            "symlinked folder shop/users/loop1 skipped",
            "symlinked folder shop/users/loop2 skipped"
        ]
    );
}

#[test]
fn bruno_folders_nested_too_deep_are_an_error() {
    let dir = bruno_copy();
    let mut deep = dir.path().to_path_buf();
    for _ in 0..=rustman_import::bruno::MAX_DEPTH {
        deep.push("d");
    }
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(
        deep.join("x.bru"),
        "meta {\n  name: x\n}\n\nget {\n  url: http://x\n}\n",
    )
    .unwrap();
    let err = load_bruno(dir.path()).unwrap_err().to_string();
    assert!(
        err.contains("folders nested deeper than 32 levels"),
        "{err}"
    );
}

#[test]
fn bruno_unsupported_auth_disables_the_collection_bearer() {
    let dir = bruno_copy();
    let list = dir.path().join("users/list-users.bru");
    let text = std::fs::read_to_string(&list).unwrap();
    std::fs::write(&list, text.replace("auth: inherit", "auth: awsv4")).unwrap();
    let collection = load_bruno(dir.path()).unwrap();
    let root = tempfile::tempdir().unwrap();
    let plan = Setup::new(root.path()).plan_collection(Format::Bruno, collection, "shop");
    let written = file(&plan, "shop-bruno/users/list-users.rman.yaml");
    assert!(written.contains("auth:\n  type: none\n"), "{written}");
    assert!(note_messages(&plan, NoteKind::Unsupported).contains(&"awsv4 auth unsupported".into()));
}

#[test]
fn bruno_environments_keep_secret_names_without_values() {
    let plan = plan_source("bruno/shop");
    let local = file(&plan, "environments/local.yaml");
    assert!(
        local.contains("base_url: http://localhost:3000\n"),
        "{local}"
    );
    assert!(local.contains("  - token\n"), "{local}");
    let staging = file(&plan, "environments/staging.yaml");
    assert!(
        staging.contains("secrets:\n  - api_password\n"),
        "{staging}"
    );
}

#[test]
fn a_malformed_bru_file_fails_with_its_line_number() {
    let err = load(
        None,
        &fixture_path("bruno/malformed"),
        "malformed",
        &SourceOptions::default(),
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "import: malformed/broken.bru: line 10: block `headers` is not closed"
    );
}

#[test]
fn a_single_bru_file_imports_as_one_request() {
    let text = std::fs::read_to_string(fixture_path("bruno/shop/health.bru")).unwrap();
    assert_eq!(rustman_import::detect(&text), Some(Format::Bruno));
    let collection = Format::Bruno.parse(&text, "health.bru").unwrap();
    assert_eq!(collection.name, "Health");
    assert!(matches!(&collection.root.items[..], [Item::Request(_)]));
}

// --- HAR -----------------------------------------------------------------

const HAR: &str = "har/shop-capture.har";

fn plan_har(filter: HarFilter) -> Plan {
    let dir = tempfile::tempdir().unwrap();
    let options = SourceOptions {
        har: filter,
        ..SourceOptions::default()
    };
    Setup::new(dir.path()).plan_source(HAR, &options)
}

#[test]
fn har_static_assets_are_dropped_and_counted() {
    let plan = plan_source(HAR);
    let files = request_files(&plan);
    assert!(files.len() <= 10, "{files:?}");
    assert_eq!(plan.report.summary.dropped, 30);
    let statics = note_messages(&plan, NoteKind::Dropped);
    assert!(
        statics
            .iter()
            .all(|m| m.starts_with("static asset dropped: "))
    );
}

#[test]
fn har_duplicates_are_skipped_once() {
    let plan = plan_source(HAR);
    // `?page=1&size=20` and `?size=20&page=1` are the same request.
    assert_eq!(request_files(&plan).len(), 9);
    assert!(note_messages(&plan, NoteKind::Info).contains(&"1 duplicate entries skipped".into()));
}

#[test]
fn har_pseudo_and_transport_headers_are_dropped() {
    let plan = plan_source(HAR);
    let pseudo = file(
        &plan,
        "shop-capture/api-example-com/get-v1-pseudo.rman.yaml",
    );
    assert!(
        pseudo.ends_with("headers:\n  - name: Accept\n    value: application/json\n"),
        "{pseudo}"
    );
    let text = tree(&plan).to_ascii_lowercase();
    for header in [
        ":authority",
        "content-length",
        "sec-ch-ua",
        "accept-encoding",
        "connection",
    ] {
        assert!(!text.contains(&format!("name: {header}\n")), "{header}");
    }
}

#[test]
fn har_query_parameters_are_taken_once_from_a_chromium_export() {
    let plan = plan_source(HAR);
    let products = file(
        &plan,
        "shop-capture/api-example-com/get-v1-products.rman.yaml",
    );
    assert!(
        products.contains("url: https://api.example.com/v1/products\n"),
        "{products}"
    );
    assert_eq!(products.matches("name: page\n").count(), 1, "{products}");
    assert_eq!(products.matches("name: size\n").count(), 1, "{products}");
    assert!(!products.contains(":path"), "{products}");
}

#[test]
fn har_cookies_and_credentials_become_secrets() {
    let plan = plan_source(HAR);
    let products = file(
        &plan,
        "shop-capture/api-example-com/get-v1-products.rman.yaml",
    );
    assert!(products.contains("value: '{{cookie}}'"), "{products}");
    assert!(
        products.contains("Bearer {{authorization_token}}"),
        "{products}"
    );
}

#[test]
fn har_host_filter_keeps_only_that_host() {
    let plan = plan_har(HarFilter {
        hosts: vec!["api.example.com".into()],
        ..HarFilter::default()
    });
    let files = request_files(&plan);
    assert_eq!(files.len(), 7, "{files:?}");
    assert!(
        files
            .iter()
            .all(|f| f.starts_with("shop-capture/api-example-com/"))
    );
    assert!(
        note_messages(&plan, NoteKind::Info).contains(&"2 entries left out by the filters".into())
    );
}

#[test]
fn har_method_and_content_type_filters_repeat() {
    let plan = plan_har(HarFilter {
        methods: vec!["post".into(), "PUT".into()],
        ..HarFilter::default()
    });
    assert_eq!(request_files(&plan).len(), 3);
    let plan = plan_har(HarFilter {
        content_types: vec![
            "text/plain".into(),
            "application/x-www-form-urlencoded".into(),
        ],
        ..HarFilter::default()
    });
    let files = request_files(&plan);
    assert_eq!(
        files,
        [
            "shop-capture/api-example-com/post-v1-login.rman.yaml",
            "shop-capture/api-example-com/put-v1-orders-1.rman.yaml"
        ]
    );
}

#[test]
fn har_static_assets_are_kept_on_request() {
    let plan = plan_har(HarFilter {
        include_static: true,
        ..HarFilter::default()
    });
    assert_eq!(plan.report.summary.dropped, 0);
    assert_eq!(request_files(&plan).len(), 39);
}

// --- HTTP files ----------------------------------------------------------

#[test]
fn http_two_requests_the_second_with_a_json_body() {
    let text = "GET {{host}}/a\n\n###\n\nPOST {{host}}/b\n\n{\"name\": \"x\"}\n";
    let collection = Format::Http.parse(text, "two.http").unwrap();
    let requests: Vec<_> = collection
        .root
        .items
        .iter()
        .map(|i| match i {
            Item::Request(r) => r,
            Item::Folder(_) => panic!("no folders"),
        })
        .collect();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].method, "POST");
    assert!(matches!(requests[1].body, Some(ImportedBody::Json { .. })));
    let dir = tempfile::tempdir().unwrap();
    let options = rustman_import::PlanOptions {
        root: dir.path(),
        source: "two.http".into(),
        format: Format::Http,
        destination: None,
        on_conflict: rustman_import::OnConflict::Skip,
        environment: Some("local".into()),
        existing: rustman_import::secrets::Existing::none(),
        dry_run: false,
    };
    let plan = rustman_import::plan(collection, &options).unwrap();
    assert_eq!(
        request_files(&plan),
        ["two/get-a.rman.yaml", "two/post-b.rman.yaml"]
    );
    assert!(file(&plan, "two/post-b.rman.yaml").contains("body:\n  type: json\n"));
}

#[test]
fn http_private_env_values_are_secrets() {
    let plan = plan_source("http/jetbrains/api.http");
    let dev = file(&plan, "environments/dev.yaml");
    assert!(dev.contains("secrets:\n  - token\n"), "{dev}");
    assert!(dev.contains("host: http://localhost:8080\n"), "{dev}");
    let moved: Vec<(&str, &str)> = plan
        .secrets
        .iter()
        .map(|m| (m.environment.as_str(), m.secret.name()))
        .collect();
    assert_eq!(moved, [("dev", "token"), ("prod", "token")]);
    let at = &plan.report.secrets[0].location;
    assert_eq!(at.file, "http-client.private.env.json");
}

#[test]
fn http_vscode_settings_environments_are_read() {
    let plan = plan_source("http/rest-client");
    let production = file(&plan, "environments/production.yaml");
    assert!(
        production.contains("host: https://api.example.com\n"),
        "{production}"
    );
    let local = file(&plan, "environments/local.yaml");
    assert!(local.contains("host: http://localhost:9000\n"), "{local}");
}

// --- Sources -------------------------------------------------------------

#[test]
fn folder_sources_count_every_file_against_the_size_limit() {
    let options = SourceOptions {
        max_size: 600,
        ..SourceOptions::default()
    };
    let err = load(None, &fixture_path("bruno/shop"), "shop", &options).unwrap_err();
    assert_eq!(
        err.to_string(),
        "import: source exceeds 600 bytes (use --max-size)"
    );
}

#[test]
fn a_folder_without_a_known_layout_cannot_be_detected() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("notes.txt"), "hi").unwrap();
    let err = load(None, dir.path(), "notes", &SourceOptions::default()).unwrap_err();
    assert!(matches!(err, ImportError::CannotDetect(_)), "{err}");
    let err = load(
        Some(Format::Postman),
        dir.path(),
        "notes",
        &SourceOptions::default(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("is a folder"), "{err}");
}
