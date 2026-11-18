//! Export to Postman v2.1 and `.http` (tasks 5.1 and 5.2): the round trip
//! import → export → import, schema validation, placeholders and warnings.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Mutex;

use common::{POSTMAN_FIXTURES, Setup, apply, fixture_path, read_tree};
use rustman_core::Workspace;
use rustman_import::Format;
use rustman_import::export::{Target, export};

/// An empty workspace.
fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("rustman.toml"),
        "[workspace]\nname = \"t\"\nversion = 1\n",
    )
    .unwrap();
    dir
}

/// Import the Postman collection `text` (shown as `shown`) into `root` and
/// return the collection folder it was written to.
fn import(root: &Path, text: &str, shown: &str) -> String {
    let collection = Format::Postman.parse(text, shown).unwrap();
    let plan = Setup::new(root).plan_collection(Format::Postman, collection, shown);
    apply(&plan, root, &Mutex::new(Vec::new()));
    plan.report.destination.clone().unwrap()
}

/// The files under `root/dir`.
fn collection_files(root: &Path, dir: &str) -> BTreeMap<String, Vec<u8>> {
    read_tree(&root.join(dir))
}

fn export_text(root: &Path, dir: &str, target: Target) -> (String, Vec<String>) {
    let ws = Workspace::load(root).unwrap();
    let exported = export(&ws, Path::new(dir), target).unwrap();
    (exported.text, exported.warnings)
}

fn collection_fixtures() -> impl Iterator<Item = &'static str> {
    POSTMAN_FIXTURES
        .iter()
        .filter(|(_, format)| *format == Format::Postman)
        .map(|(name, _)| *name)
}

#[test]
fn import_export_import_is_byte_identical() {
    for name in collection_fixtures() {
        let first = workspace();
        let text = fs::read_to_string(fixture_path(name)).unwrap();
        let dir = import(first.path(), &text, "c.json");
        let (exported, _) = export_text(first.path(), &dir, Target::Postman);
        let second = workspace();
        let again = import(second.path(), &exported, "c.json");
        assert_eq!(again, dir, "{name}");
        let a = collection_files(first.path(), &dir);
        let b = collection_files(second.path(), &dir);
        for (path, bytes) in &a {
            assert_eq!(
                String::from_utf8_lossy(b.get(path).map_or(&[][..], |b| b)),
                String::from_utf8_lossy(bytes),
                "{name}: {path} differs after the round trip\n{exported}"
            );
        }
        assert_eq!(a.len(), b.len(), "{name}");
    }
}

#[test]
fn postman_exports_validate_against_the_vendored_schema() {
    let schema: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(fixture_path("postman/schema/collection-v2.1.0.json")).unwrap(),
    )
    .unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    for name in collection_fixtures() {
        let ws = workspace();
        let text = fs::read_to_string(fixture_path(name)).unwrap();
        let dir = import(ws.path(), &text, "c.json");
        let (exported, _) = export_text(ws.path(), &dir, Target::Postman);
        let doc: serde_json::Value = serde_json::from_str(&exported).unwrap();
        let errors: Vec<String> = validator.iter_errors(&doc).map(|e| e.to_string()).collect();
        assert!(errors.is_empty(), "{name}: {errors:#?}\n{exported}");
        assert_eq!(
            doc["info"]["schema"],
            rustman_import::export::POSTMAN_SCHEMA
        );
    }
}

/// A workspace with one `api` collection: bearer auth from a secret, an
/// inherited header, a JSON body, an assertion and a capture.
fn hand_written() -> tempfile::TempDir {
    let ws = workspace();
    let write = |rel: &str, text: &str| {
        let path = ws.path().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    };
    write(
        "api/_collection.yaml",
        "name: API\nheaders:\n  - name: X-Team\n    value: core\nvariables:\n  base: https://api.example.com\n",
    );
    write(
        "api/create-order.rman.yaml",
        "version: 1\nkind: http\nname: Create order\nmethod: POST\nurl: '{{base}}/orders'\n\
         query:\n  - name: dry\n    value: '1'\n\
         body:\n  type: json\n  content: {sku: A-1, qty: 2}\n\
         auth:\n  type: bearer\n  token: '{{token}}'\n\
         assertions:\n  - status: 201\n\
         captures:\n  - var: order_id\n    from: json\n    path: $.id\n",
    );
    write(
        "api/upload.rman.yaml",
        "version: 1\nkind: http\nname: Upload\nmethod: POST\nurl: '{{base}}/files'\n\
         body:\n  type: multipart\n  parts:\n    - name: note\n      value: hi\n    - name: doc\n      file: data/doc.pdf\n\
         auth:\n  type: api_key\n  name: key\n  value: '{{api_key}}'\n  in: query\n",
    );
    ws
}

#[test]
fn http_exports_keep_secrets_as_placeholders_and_warn() {
    let ws = hand_written();
    let (text, warnings) = export_text(ws.path(), "api", Target::Http);
    assert_eq!(
        text,
        "@base = https://api.example.com\n\
         \n\
         ### Create order\n\
         POST {{base}}/orders?dry=1\n\
         X-Team: core\n\
         Authorization: Bearer {{token}}\n\
         Content-Type: application/json\n\
         \n\
         {\n  \"sku\": \"A-1\",\n  \"qty\": 2\n}\n\
         \n\
         ### Upload\n\
         POST {{base}}/files?key={{api_key}}\n\
         X-Team: core\n\
         Content-Type: multipart/form-data; boundary=rustman-boundary\n\
         \n\
         --rustman-boundary\n\
         Content-Disposition: form-data; name=\"note\"\n\
         \n\
         hi\n\
         --rustman-boundary\n\
         Content-Disposition: form-data; name=\"doc\"; filename=\"doc.pdf\"\n\
         \n\
         < data/doc.pdf\n\
         --rustman-boundary--\n"
    );
    assert_eq!(
        warnings,
        [
            "export: api/create-order.rman.yaml: 1 assertions, 1 captures not exported (no equivalent)"
        ]
    );
}

#[test]
fn postman_exports_warn_about_folder_headers_and_assertions() {
    let ws = hand_written();
    let (text, warnings) = export_text(ws.path(), "api", Target::Postman);
    assert!(text.contains("\"token\""), "{text}");
    assert!(text.contains("\"{{token}}\""), "{text}");
    assert_eq!(
        warnings,
        [
            "export: api/create-order.rman.yaml: 1 assertions, 1 captures not exported (no equivalent)",
            "export: api: folder headers not exported (Postman folders have none)",
        ]
    );
}

#[test]
fn one_request_exports_with_what_it_inherits() {
    let ws = hand_written();
    let rel = Path::new("api").join("create-order.rman.yaml");
    let workspace = Workspace::load(ws.path()).unwrap();
    let exported = export(&workspace, &rel, Target::Http).unwrap();
    assert!(
        exported
            .text
            .starts_with("@base = https://api.example.com\n\n### Create order\n")
    );
    assert!(
        exported.text.contains("X-Team: core\n"),
        "{}",
        exported.text
    );
    let err = export(&workspace, Path::new("nope"), Target::Http).unwrap_err();
    assert_eq!(
        err.to_string(),
        "export: nope is not a request file or collection folder of the workspace"
    );
}

#[test]
fn exported_scripts_and_dynamic_variables_use_postman_forms() {
    let ws = workspace();
    let text =
        fs::read_to_string(fixture_path("postman/billing-api.postman_collection.json")).unwrap();
    let dir = import(ws.path(), &text, "c.json");
    let (postman, _) = export_text(ws.path(), &dir, Target::Postman);
    assert!(postman.contains("{{$guid}}"), "{postman}");
    assert!(!postman.contains("{{$uuid}}"), "{postman}");
    assert!(postman.contains("\"listen\": \"test\""), "{postman}");
    let (http, warnings) = export_text(ws.path(), &dir, Target::Http);
    let mut settings = insta::Settings::clone_current();
    settings.set_prepend_module_to_snapshot(false);
    settings.bind(|| {
        insta::assert_snapshot!("export-billing-api-http", http);
        insta::assert_snapshot!("export-billing-api-http-warnings", warnings.join("\n"));
    });
}

#[test]
fn an_unsupported_auth_does_not_inherit_the_collection_credential() {
    let ws = workspace();
    let text = serde_json::json!({
        "info": {"name": "Storage", "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"},
        "auth": {"type": "bearer", "bearer": [{"key": "token", "value": "{{internal_token}}"}]},
        "item": [{
            "name": "Report",
            "request": {
                "method": "GET",
                "url": "https://my-bucket.s3.amazonaws.com/report.csv",
                "auth": {"type": "awsv4", "awsv4": [{"key": "accessKey", "value": "{{aws_key}}"}]}
            }
        }]
    })
    .to_string();
    let dir = import(ws.path(), &text, "storage.json");
    let rel = Path::new(&dir).join("report.rman.yaml");
    let request = fs::read_to_string(ws.path().join(&rel)).unwrap();
    assert!(request.contains("auth:\n  type: none\n"), "{request}");
    let workspace = Workspace::load(ws.path()).unwrap();
    let exported = export(&workspace, &rel, Target::Http).unwrap();
    assert!(
        !exported.text.contains("internal_token"),
        "{}",
        exported.text
    );
    assert!(
        !exported.text.contains("Authorization"),
        "{}",
        exported.text
    );
}
