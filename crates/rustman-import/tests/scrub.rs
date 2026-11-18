//! Secrets the second review found outside the checked fields: folder
//! variable scope, well-known tokens and repeated values in free text,
//! name and value detection gaps, and the related minor fixes.

mod common;

use common::{Setup, file, tree};
use rustman_import::{Format, HarFilter, Plan};
use serde_json::{Value, json};

const SCHEMA: &str = "https://schema.getpostman.com/json/collection/v2.1.0/collection.json";
const GITHUB: &str = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
const JWT: &str = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJDTlJZMl9KV1QifQ.c2ln";

fn plan_as(format: Format, text: &str) -> Plan {
    let dir = tempfile::tempdir().unwrap();
    let parsed = format.parse(text, "c.json").unwrap();
    Setup::new(dir.path()).plan_collection(format, parsed, "c.json")
}

fn plan_postman(collection: Value) -> Plan {
    plan_as(Format::Postman, &collection.to_string())
}

fn request(name: &str, url: &str) -> Value {
    json!({"name": name, "request": {"method": "GET", "url": url}})
}

/// The stored value of the secret `name`.
fn stored<'p>(plan: &'p Plan, name: &str) -> &'p str {
    plan.secrets
        .iter()
        .find(|m| m.secret.name() == name)
        .unwrap_or_else(|| panic!("no secret {name}"))
        .secret
        .expose()
}

/// No planned file and no report output contains `value`.
fn assert_nowhere(plan: &Plan, value: &str) {
    for f in &plan.files {
        assert!(!f.text.contains(value), "{value} in {}\n{}", f.path, f.text);
    }
    assert!(
        !plan.report.to_json().contains(value),
        "{value} in the report"
    );
    assert!(
        !plan.report.summary_text().contains(value),
        "{value} in the summary"
    );
}

#[test]
fn a_folder_variable_rename_applies_only_inside_that_folder() {
    let plan = plan_postman(json!({
        "info": {"name": "Two", "schema": SCHEMA},
        "variable": [{"key": "password", "value": "ROOT-PW-1"}],
        "item": [
            request("root", "https://root.example.com/login?p={{password}}"),
            {"name": "F", "variable": [{"key": "password", "value": "FOLDER-PW-2"}],
             "item": [
                request("in f", "https://f.example.com/?p={{password}}"),
                {"name": "Deeper", "item": [request("deep", "https://d.example.com/?p={{password}}")]}
             ]},
            {"name": "G", "item": [request("in g", "https://g.example.com/?p={{password}}")]}
        ]
    }));
    // Each request resolves to the value Postman would use.
    let value_of = |path: &str| {
        let text = file(&plan, path);
        let name = text
            .split("value: '{{")
            .nth(1)
            .and_then(|rest| rest.split("}}").next())
            .unwrap_or_else(|| panic!("{text}"));
        stored(&plan, name).to_owned()
    };
    assert_eq!(value_of("two/root.rman.yaml"), "ROOT-PW-1");
    assert_eq!(value_of("two/g/in-g.rman.yaml"), "ROOT-PW-1");
    assert_eq!(value_of("two/f/in-f.rman.yaml"), "FOLDER-PW-2");
    assert_eq!(value_of("two/f/deeper/deep.rman.yaml"), "FOLDER-PW-2");
}

#[test]
fn a_plain_folder_variable_shadows_a_renamed_one() {
    let plan = plan_postman(json!({
        "info": {"name": "Shadow", "schema": SCHEMA},
        "variable": [{"key": "password", "value": "ROOT-PW-1"}],
        "item": [
            {"name": "F", "variable": [{"key": "password", "value": "{{other}}"}],
             "item": [request("in f", "https://f.example.com/?p={{password}}")]}
        ]
    }));
    let text = file(&plan, "shadow/f/in-f.rman.yaml");
    assert!(text.contains("value: '{{password}}'"), "{text}");
}

#[test]
fn known_tokens_are_moved_from_descriptions_names_and_url_paths() {
    let plan = plan_postman(json!({
        "info": {"name": "Tok", "schema": SCHEMA, "description": format!("Use key {GITHUB}")},
        "item": [
            {"name": "root", "request": {"method": "GET", "description": format!("token {JWT}"),
              "url": format!("https://api.example.com/keys/{GITHUB}")}},
            {"name": format!("by {GITHUB}"), "request": {"method": "GET", "url": "https://x.test"}},
            {"name": "aws", "request": {"method": "GET", "url": "https://x.test", "header": [
                {"key": "X-Note", "value": "ASIAABCDEFGHIJKLMNOP"},
                {"key": "X-Goog", "value": "AIzaSyA1234567890abcdefghijklmnopqrstuv"}]}},
            {"name": "pem", "request": {"method": "POST", "url": "https://x.test", "body": {"mode": "raw",
              "raw": "-----BEGIN PRIVATE KEY-----\nMIIBVQ\n-----END PRIVATE KEY-----"}}}
        ]
    }));
    for value in [GITHUB, JWT, "ASIAABCDEFGHIJKLMNOP", "AIzaSyA", "MIIBVQ"] {
        assert_nowhere(&plan, value);
    }
    assert!(file(&plan, "tok/_collection.yaml").contains("Use key «redacted:github_token»"));
    let root = file(&plan, "tok/root.rman.yaml");
    assert!(root.contains("description: token «redacted:jwt»"), "{root}");
    assert!(
        root.contains("url: https://api.example.com/keys/{{github_token}}"),
        "{root}"
    );
    assert_eq!(stored(&plan, "github_token"), GITHUB);
    assert_eq!(stored(&plan, "jwt"), JWT);
}

#[test]
fn a_moved_value_is_replaced_wherever_it_repeats() {
    let plan = plan_postman(json!({
        "info": {"name": "Rep", "schema": SCHEMA},
        "item": [
            {"name": "root", "request": {"method": "POST",
              "description": "the key is SAME-VALUE-42",
              "url": "https://api.example.com/keys/SAME-VALUE-42?echo=SAME-VALUE-42",
              "header": [{"key": "X-Api-Key", "value": "SAME-VALUE-42"},
                         {"key": "X-Note", "value": "SAME-VALUE-42"}],
              "body": {"mode": "raw", "raw": "{\"note\": \"SAME-VALUE-42\"}"}}}
        ]
    }));
    assert_nowhere(&plan, "SAME-VALUE-42");
    let root = file(&plan, "rep/root.rman.yaml");
    assert!(
        root.contains("url: https://api.example.com/keys/{{x_api_key}}"),
        "{root}"
    );
    assert!(
        root.contains("description: the key is «redacted:x_api_key»"),
        "{root}"
    );
    assert!(root.contains("\"note\": \"{{x_api_key}}\""), "{root}");
}

#[test]
fn credentials_in_url_variables_scheme_headers_and_arrays_are_moved() {
    let plan = plan_postman(json!({
        "info": {"name": "Gap", "schema": SCHEMA},
        "variable": [{"key": "dbUrl", "value": "postgres://admin:DB-PW-77@db/x"}],
        "item": [{"name": "r", "request": {"method": "POST", "url": "https://x.test",
            "header": [{"key": "X-Client", "value": "Token TOK-SCHEME-1"},
                       {"key": "X-Bot", "value": "Bot BOT-2"},
                       {"key": "X-Pwd", "value": "PWD-3"}],
            "body": {"mode": "raw", "raw": "{\"tokens\": [\"ARR-4\", \"ARR-5\"], \"pin\": 12345678}"}}}]
    }));
    for value in [
        "DB-PW-77",
        "TOK-SCHEME-1",
        "BOT-2",
        "PWD-3",
        "ARR-4",
        "ARR-5",
        "12345678",
    ] {
        assert_nowhere(&plan, value);
    }
    let root = file(&plan, "gap/_collection.yaml");
    assert!(
        root.contains("dbUrl: postgres://admin:{{dburl_password}}@db/x"),
        "{root}"
    );
    let r = file(&plan, "gap/r.rman.yaml");
    assert!(r.contains("value: Token {{x_client}}"), "{r}");
}

#[test]
fn environment_url_credentials_are_moved() {
    let env = json!({"name": "Staging", "values": [
        {"key": "db_url", "value": "https://user:ENV-URL-PW@db.example.com", "enabled": true}]});
    let plan = plan_as(Format::PostmanEnv, &env.to_string());
    assert_nowhere(&plan, "ENV-URL-PW");
    let staging = file(&plan, "environments/staging.yaml");
    assert!(
        staging.contains("db_url: https://user:{{db_url_password}}@db.example.com"),
        "{staging}"
    );
    assert_eq!(stored(&plan, "db_url_password"), "ENV-URL-PW");
}

#[test]
fn btoa_literals_are_redacted_in_comments_and_the_report() {
    let plan = plan_postman(json!({
        "info": {"name": "Scr", "schema": SCHEMA},
        "item": [{"name": "r", "request": {"method": "GET", "url": "https://x.test"},
          "event": [{"listen": "prerequest", "script": {"exec": [
            "let creds = btoa('user:BTOA-SECRET-9');",
            "secret = 'PLAIN-ASSIGN-8';",
            "pm.request.headers.add({key: 'Authorization', value: 'Basic ' + creds});"
          ]}}]}]
    }));
    assert_nowhere(&plan, "BTOA-SECRET-9");
    assert_nowhere(&plan, "PLAIN-ASSIGN-8");
    let r = file(&plan, "scr/r.rman.yaml");
    assert!(r.contains("btoa('«redacted:basic_credentials»')"), "{r}");
}

#[test]
fn postman_inherit_auth_keeps_the_parent_auth() {
    let plan = plan_postman(json!({
        "info": {"name": "Inh", "schema": SCHEMA},
        "auth": {"type": "bearer", "bearer": [{"key": "token", "value": "{{t}}"}]},
        "item": [{"name": "r", "request": {"method": "GET", "url": "https://x.test",
            "auth": {"type": "inherit"}}}]
    }));
    let r = file(&plan, "inh/r.rman.yaml");
    assert!(!r.contains("auth:"), "{r}");
    assert!(
        plan.report
            .notes
            .iter()
            .all(|n| !n.message.contains("inherit"))
    );
}

#[test]
fn har_verifiers_and_raw_multipart_parts_are_moved() {
    let har = json!({"log": {"entries": [
        {"request": {"method": "POST", "url": "https://x.test/token?code_verifier=VERIF-Q-1",
          "postData": {"mimeType": "application/x-www-form-urlencoded",
                       "text": "code_verifier=VERIF-F-2&grant_type=authorization_code"}}},
        {"request": {"method": "POST", "url": "https://x.test/upload",
          "postData": {"mimeType": "multipart/form-data; boundary=----B",
            "text": "------B\r\nContent-Disposition: form-data; name=\"password\"\r\n\r\nMP-PW-3\r\n------B--\r\n"}}}
    ]}});
    let dir = tempfile::tempdir().unwrap();
    let collection =
        rustman_import::har::parse(&har.to_string(), "c.har", &HarFilter::default()).unwrap();
    let plan = Setup::new(dir.path()).plan_collection(Format::Har, collection, "c.har");
    for value in ["VERIF-Q-1", "VERIF-F-2", "MP-PW-3"] {
        assert_nowhere(&plan, value);
    }
    let upload = plan
        .files
        .iter()
        .find(|f| f.path.ends_with("post-upload.rman.yaml"))
        .unwrap_or_else(|| panic!("{}", tree(&plan)));
    assert!(upload.text.contains("type: multipart\n"), "{}", upload.text);
}

#[test]
fn insomnia_environments_with_the_same_file_name_stay_apart() {
    let export = json!({"_type": "export", "__export_format": 4, "resources": [
        {"_id": "wrk", "_type": "workspace", "name": "W", "parentId": null},
        {"_id": "env", "_type": "environment", "parentId": "wrk", "name": "Base", "data": {}},
        {"_id": "e1", "_type": "environment", "parentId": "env", "name": "Prod", "data": {"host": "a"}, "metaSortKey": 1},
        {"_id": "e2", "_type": "environment", "parentId": "env", "name": "prod", "data": {"host": "b"}, "metaSortKey": 2},
        {"_id": "r", "_type": "request", "parentId": "wrk", "name": "r", "method": "GET", "url": "https://x.test"}
    ]});
    let plan = plan_as(Format::Insomnia, &export.to_string());
    assert!(file(&plan, "environments/prod.yaml").contains("host: a\n"));
    assert!(file(&plan, "environments/prod-2.yaml").contains("host: b\n"));
    assert!(
        plan.report
            .notes
            .iter()
            .any(|n| n.message.contains("imported as 'prod-2'")),
        "{:?}",
        plan.report.notes
    );
}
