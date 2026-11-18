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

/// A Bruno collection: `collection.bru` with `vars`, and per folder
/// (`""` for the root) its `folder.bru` vars and requests (name, URL).
/// `name = value` pairs.
type Vars<'a> = &'a [(&'a str, &'a str)];

/// A folder (`""` for the root), its `folder.bru` vars and its requests
/// (name, URL).
type BrunoFolder<'a> = (&'a str, Vars<'a>, Vars<'a>);

fn plan_bruno(root_vars: Vars<'_>, folders: &[BrunoFolder<'_>]) -> Plan {
    let dir = tempfile::tempdir().unwrap();
    let vars = |vars: &[(&str, &str)]| -> String {
        let lines: Vec<String> = vars.iter().map(|(k, v)| format!("  {k}: {v}\n")).collect();
        format!("vars:pre-request {{\n{}}}\n", lines.concat())
    };
    std::fs::write(
        dir.path().join("bruno.json"),
        r#"{"version": "1", "name": "Scope", "type": "collection"}"#,
    )
    .unwrap();
    std::fs::write(dir.path().join("collection.bru"), vars(root_vars)).unwrap();
    for (folder, folder_vars, requests) in folders {
        let path = dir.path().join(folder);
        std::fs::create_dir_all(&path).unwrap();
        if !folder.is_empty() {
            std::fs::write(path.join("folder.bru"), vars(folder_vars)).unwrap();
        }
        for (name, url) in *requests {
            let text = format!(
                "meta {{\n  name: {name}\n  type: http\n}}\n\nget {{\n  url: {url}\n  body: none\n  auth: none\n}}\n"
            );
            std::fs::write(path.join(format!("{name}.bru")), text).unwrap();
        }
    }
    let (format, collection) = rustman_import::load(
        Some(Format::Bruno),
        dir.path(),
        "scope",
        &rustman_import::SourceOptions::default(),
    )
    .unwrap();
    let root = tempfile::tempdir().unwrap();
    Setup::new(root.path()).plan_collection(format, collection, "scope")
}

/// The value the first `{{name}}` of the file at `path` resolves to, from
/// the moved secrets, or its own name when it is not a secret.
fn resolved(plan: &Plan, path: &str) -> String {
    let text = file(plan, path);
    let name = text
        .split("{{")
        .nth(1)
        .and_then(|rest| rest.split("}}").next())
        .unwrap_or_else(|| panic!("{text}"));
    plan.secrets
        .iter()
        .find(|m| m.secret.name() == name)
        .map_or_else(|| format!("var:{name}"), |m| m.secret.expose().clone())
}

#[test]
fn a_folder_variable_rename_applies_only_inside_that_folder() {
    let url = "https://x.test/?p={{password}}";
    let plan = plan_bruno(
        &[("password", "ROOT-PW-1")],
        &[
            ("", &[], &[("root", url)]),
            ("f", &[("password", "FOLDER-PW-2")], &[("in-f", url)]),
            ("f/deeper", &[], &[("deep", url)]),
            ("g", &[], &[("in-g", url)]),
        ],
    );
    // Each request resolves to the value Bruno would use.
    assert_eq!(resolved(&plan, "scope/root.rman.yaml"), "ROOT-PW-1");
    assert_eq!(resolved(&plan, "scope/g/in-g.rman.yaml"), "ROOT-PW-1");
    assert_eq!(resolved(&plan, "scope/f/in-f.rman.yaml"), "FOLDER-PW-2");
    assert_eq!(
        resolved(&plan, "scope/f/deeper/deep.rman.yaml"),
        "FOLDER-PW-2"
    );
}

#[test]
fn a_rename_never_takes_another_variables_name() {
    // `token_2` is a plain collection variable: folder A's moved `token`
    // must not become `{{token_2}}`, which resolves to `{{env_tok}}`.
    let url = "https://x.test/?t={{token}}";
    let plan = plan_bruno(
        &[("token", "TOK_ROOT_0001"), ("token_2", "{{env_tok}}")],
        &[
            ("", &[], &[("root", url)]),
            ("a", &[("token", "TOK_A_0002")], &[("in-a", url)]),
        ],
    );
    assert_eq!(resolved(&plan, "scope/root.rman.yaml"), "TOK_ROOT_0001");
    assert_eq!(resolved(&plan, "scope/a/in-a.rman.yaml"), "TOK_A_0002");
    assert!(plan.secrets.iter().all(|m| m.secret.name() != "token_2"));
}

#[test]
fn postman_folder_variables_are_dropped_as_postman_ignores_them() {
    let plan = plan_postman(json!({
        "info": {"name": "Two", "schema": SCHEMA},
        "variable": [{"key": "password", "value": "ROOT-PW-1"}],
        "item": [
            {"name": "F", "variable": [{"key": "password", "value": "FOLDER-PW-2"}],
             "item": [request("in f", "https://f.example.com/?p={{password}}")]}
        ]
    }));
    // Postman sends the collection's value from inside F too.
    assert_eq!(resolved(&plan, "two/f/in-f.rman.yaml"), "ROOT-PW-1");
    assert_nowhere(&plan, "FOLDER-PW-2");
    assert!(
        plan.secrets
            .iter()
            .all(|m| m.secret.expose() != "FOLDER-PW-2")
    );
    let note = plan
        .report
        .notes
        .iter()
        .find(|n| n.message.starts_with("folder variable password dropped"))
        .unwrap_or_else(|| panic!("{:?}", plan.report.notes));
    assert_eq!(note.kind, rustman_import::ir::NoteKind::Dropped);
}

#[test]
fn a_path_variable_does_not_take_over_a_collection_variable() {
    let plan = plan_postman(json!({
        "info": {"name": "Path", "schema": SCHEMA},
        "variable": [{"key": "token", "value": "{{env_tok}}"}],
        "auth": {"type": "bearer", "bearer": [{"key": "token", "value": "{{token}}"}]},
        "item": [{"name": "r", "request": {"method": "GET",
            "url": {"raw": "https://api.test/:token/x", "variable": [{"key": "token", "value": "TOK_PATH_0004"}]}}}]
    }));
    let r = file(&plan, "path/r.rman.yaml");
    assert!(r.contains("url: https://api.test/{{path_token}}/x"), "{r}");
    assert_eq!(resolved(&plan, "path/r.rman.yaml"), "TOK_PATH_0004");
    // The request inherits the collection's bearer `{{token}}`, which no
    // request variable now shadows.
    assert!(!r.contains("name: token\n"), "{r}");
    let root = file(&plan, "path/_collection.yaml");
    assert!(root.contains("token: '{{token}}'"), "{root}");
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

fn review_notes(plan: &Plan) -> Vec<String> {
    plan.report
        .notes
        .iter()
        .filter(|n| n.kind == rustman_import::ir::NoteKind::Review)
        .map(|n| n.message.clone())
        .collect()
}

#[test]
fn weak_secret_values_never_change_other_data() {
    // The reviewer's inputs: secrets whose values are ordinary words and
    // numbers that also appear as data.
    let plan = plan_postman(json!({
        "info": {"name": "FP 123456 example", "schema": SCHEMA},
        "variable": [
            {"key": "password", "value": "123456"}, {"key": "api_key", "value": "example"},
            {"key": "client_secret", "value": "application"}, {"key": "pin", "value": "000000"}
        ],
        "item": [
            {"name": "Login", "request": {"method": "POST", "url": "https://auth.local/login",
              "auth": {"type": "basic", "basic": [{"key": "username", "value": "admin"}, {"key": "password", "value": "password"}]},
              "body": {"mode": "raw", "raw": "{\"grant_type\":\"password\",\"amount\":123456}"}}},
            {"name": "example", "request": {"method": "POST",
              "url": "https://api.example.com/password/reset?type=password&zip=123456&page=000000",
              "header": [{"key": "Content-Type", "value": "application/json"}],
              "body": {"mode": "urlencoded", "urlencoded": [{"key": "grant_type", "value": "password"}]}}}
        ]
    }));
    let dest = "fp-123456-example";
    let login = file(&plan, &format!("{dest}/login.rman.yaml"));
    assert!(
        login.contains(r#""grant_type":"password","amount":123456"#),
        "{login}"
    );
    let example = file(&plan, &format!("{dest}/example.rman.yaml"));
    for kept in [
        "url: https://api.example.com/password/reset",
        "value: password",
        "value: '123456'",
        "value: '000000'",
        "value: application/json",
        "name: example",
    ] {
        assert!(example.contains(kept), "{kept}: {example}");
    }
    let notes = review_notes(&plan);
    assert!(
        notes.iter().any(
            |n| n.starts_with("the value of secret password also appears in ")
                && n.ends_with("; review it")
        ),
        "{notes:?}"
    );
    // Notes name the secret and the field, never the value.
    assert!(
        notes.iter().any(|n| n.contains("example:query zip")),
        "{notes:?}"
    );
    assert!(
        notes
            .iter()
            .all(|n| !n.contains("123456") || n.contains("FP 123456")),
        "{notes:?}"
    );
}

#[test]
fn a_secret_like_value_off_token_boundaries_is_redacted_and_noted() {
    let plan = plan_postman(json!({
        "info": {"name": "Off", "schema": SCHEMA},
        "item": [{"name": "r", "request": {"method": "GET", "url": "https://x.test/?a=xSAME-VALUE-42",
            "header": [{"key": "X-Api-Key", "value": "SAME-VALUE-42"}]}}]
    }));
    assert_nowhere(&plan, "SAME-VALUE-42");
    let r = file(&plan, "off/r.rman.yaml");
    assert!(r.contains("value: x«redacted:x_api_key»"), "{r}");
    assert!(
        review_notes(&plan).iter().any(|n| n.ends_with(
            "field query a contained secret x_api_key inside other text; it was redacted, edit the field to use {{x_api_key}}"
        )),
        "{:?}",
        review_notes(&plan)
    );
}

#[test]
fn percent_encoded_keys_and_basic_values_in_json_are_moved() {
    let plan = plan_postman(json!({
        "info": {"name": "Enc", "schema": SCHEMA},
        "item": [{"name": "r", "request": {"method": "POST",
            "url": "https://a.test/cb?k=-----BEGIN%20RSA%20PRIVATE%20KEY-----%0AENCPEMBODY%0A-----END%20RSA%20PRIVATE%20KEY-----",
            "body": {"mode": "raw", "raw": "{\"authHeader\": \"Basic dTpFTkNCQVNJQw==\"}"}}}]
    }));
    assert_nowhere(&plan, "ENCPEMBODY");
    assert_nowhere(&plan, "dTpFTkNCQVNJQw==");
    let r = file(&plan, "enc/r.rman.yaml");
    assert!(r.contains("value: '{{private_key}}'"), "{r}");
    assert!(r.contains(r#""authHeader": "Basic {{authheader}}""#), "{r}");
    assert!(stored(&plan, "private_key").contains("ENCPEMBODY"));
}

#[test]
fn environment_files_only_take_their_own_secrets_as_whole_values() {
    let export = json!({"_type": "export", "__export_format": 4, "resources": [
        {"_id": "wrk", "_type": "workspace", "name": "W", "parentId": null},
        {"_id": "env", "_type": "environment", "parentId": "wrk", "name": "Base",
         "data": {"api_key": "SAME-VAL-42X", "copy": "SAME-VAL-42X", "note": "key SAME-VAL-42X", "other": "HDR-VAL-99X"}},
        {"_id": "r", "_type": "request", "parentId": "wrk", "name": "r", "method": "GET", "url": "https://x.test",
         "headers": [{"name": "X-Api-Key", "value": "HDR-VAL-99X"}]}
    ]});
    let plan = plan_as(Format::Insomnia, &export.to_string());
    let base = file(&plan, "environments/base.yaml");
    // (a) whole value, same environment.
    assert!(base.contains("copy: '{{api_key}}'"), "{base}");
    // Embedded values and other environments' secrets are left alone.
    assert!(base.contains("note: key SAME-VAL-42X"), "{base}");
    assert!(base.contains("other: HDR-VAL-99X"), "{base}");
}

/// `text` with each `{{name}}` of a moved secret replaced by its value:
/// the bytes the request would send.
fn sent(plan: &Plan, text: &str) -> String {
    let mut out = text.to_owned();
    for m in &plan.secrets {
        out = out.replace(&format!("{{{{{}}}}}", m.secret.name()), m.secret.expose());
    }
    out
}

/// The `url:` line or JSON `content:` of a planned request file.
fn line_of(text: &str, key: &str) -> String {
    text.lines()
        .find_map(|l| l.trim_start().strip_prefix(key))
        .unwrap_or_else(|| panic!("no {key} in {text}"))
        .trim()
        .trim_matches('\'')
        .replace("''", "'")
}

#[test]
fn encoded_repeats_are_redacted_never_sent_with_different_bytes() {
    // The reviewer's echo cases: a raw `{{name}}` in an encoded context
    // would send invalid JSON, an extra path segment or an extra field.
    let plan = plan_postman(json!({
        "info": {"name": "Echo", "schema": SCHEMA},
        "variable": [{"key": "password", "value": "Pa\"ss&w0rd+1"},
                     {"key": "slash_secret", "value": "ab/cd-1234"},
                     {"key": "form_secret", "value": "p@ss&w0rd=1"}],
        "item": [
            {"name": "json", "request": {"method": "POST", "url": "https://a.test/x",
              "body": {"mode": "raw", "raw": "{\"note\": \"Pa\\\"ss&w0rd+1\", \"s\": \"ab\\/cd-1234\"}"}}},
            {"name": "path", "request": {"method": "GET", "url": "https://a.test/u/ab%2Fcd-1234/x"}},
            {"name": "form", "request": {"method": "POST", "url": "https://a.test/f",
              "body": {"mode": "raw", "raw": "note=p%40ss%26w0rd%3D1"}}}
        ]
    }));
    let originals = [
        (
            "json",
            "content:",
            r#"{"note": "Pa\"ss&w0rd+1", "s": "ab\/cd-1234"}"#,
        ),
        ("path", "url:", "https://a.test/u/ab%2Fcd-1234/x"),
        ("form", "content:", "note=p%40ss%26w0rd%3D1"),
    ];
    for (name, key, original) in originals {
        let text = file(&plan, &format!("echo/{name}.rman.yaml"));
        let written = line_of(text, key);
        let bytes = sent(&plan, &written);
        assert!(
            bytes == original || written.contains("«redacted:"),
            "{name}: {written}"
        );
    }
    for value in [
        "Pa\\\"ss&w0rd+1",
        "ab\\/cd-1234",
        "ab%2Fcd-1234",
        "p%40ss%26w0rd%3D1",
    ] {
        assert_nowhere(&plan, value);
    }
    let notes = review_notes(&plan);
    assert!(
        notes.iter().any(|n| n.contains("field body contained secret password in encoded form; it was redacted, edit the field to use {{password}}")),
        "{notes:?}"
    );
}

#[test]
fn long_letter_and_digit_secrets_are_replaced_raw() {
    let plan = plan_postman(json!({
        "info": {"name": "Long", "schema": SCHEMA},
        "variable": [{"key": "access_token", "value": "AbCdEfGhIjKlMnOpQrStUvWxYzAbCdEf"},
                     {"key": "client_secret", "value": "8473629184736291"}],
        "item": [{"name": "r", "request": {"method": "POST", "url": "https://a.test/x",
            "header": [{"key": "X-Note", "value": "AbCdEfGhIjKlMnOpQrStUvWxYzAbCdEf"}],
            "body": {"mode": "raw", "raw": "{\"acct\": \"8473629184736291\", \"n\": 8473629184736291}"}}}]
    }));
    assert_nowhere(&plan, "AbCdEfGhIjKlMnOpQrStUvWxYzAbCdEf");
    assert_nowhere(&plan, "8473629184736291");
    let r = file(&plan, "long/r.rman.yaml");
    assert!(r.contains("value: '{{access_token}}'"), "{r}");
    let body = line_of(r, "content:");
    assert_eq!(
        sent(&plan, &body),
        r#"{"acct": "8473629184736291", "n": 8473629184736291}"#
    );
}

#[test]
fn a_token_shaped_secret_is_found_inside_a_word() {
    let gh = "ghp_FNEGaaaaBBBBccccDDDDeeee0123456789";
    let plan = plan_postman(json!({
        "info": {"name": "Gh", "schema": SCHEMA},
        "item": [{"name": "r", "request": {"method": "POST", "url": "https://a.test/x",
            "body": {"mode": "raw", "raw": format!("{{\"enc\": \"{gh}\", \"w\": \"abc{gh}\"}}")}}}]
    }));
    assert_nowhere(&plan, "FNEGaaaaBBBB");
    let body = line_of(file(&plan, "gh/r.rman.yaml"), "content:");
    assert_eq!(
        sent(&plan, &body),
        format!("{{\"enc\": \"{gh}\", \"w\": \"abc{gh}\"}}")
    );
}

#[test]
fn a_moved_secret_never_takes_an_undeclared_referenced_name() {
    let plan = plan_postman(json!({
        "info": {"name": "Ref", "schema": SCHEMA},
        "item": [
            {"name": "A", "request": {"method": "GET", "url": "https://a.test/a",
              "header": [{"key": "Authorization", "value": "Bearer {{token}}"}]}},
            {"name": "B", "request": {"method": "GET", "url": "https://a.test/b",
              "auth": {"type": "bearer", "bearer": [{"key": "token", "value": "abc123def456ghi"}]}}}
        ]
    }));
    // `{{token}}` in A is meant to come from the user's environment.
    assert!(plan.secrets.iter().all(|m| m.secret.name() != "token"));
    let b = file(&plan, "ref/b.rman.yaml");
    assert!(b.contains("token: '{{token_2}}'"), "{b}");
    assert!(file(&plan, "ref/a.rman.yaml").contains("Bearer {{token}}"));
}

#[test]
fn concurrent_imports_into_one_workspace_keep_both_environments_changes() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("rustman.toml"),
        "[workspace]\nname = \"t\"\nversion = 1\n",
    )
    .unwrap();
    let root = dir.path().to_path_buf();
    let import = move |name: &'static str, secret: &'static str| {
        let root = root.clone();
        std::thread::spawn(move || {
            let collection = json!({
                "info": {"name": name, "schema": SCHEMA},
                "item": [{"name": "r", "request": {"method": "GET", "url": "https://a.test",
                    "header": [{"key": format!("X-{name}-Key"), "value": secret}]}}]
            });
            let _lock = rustman_import::lock_workspace(&root).unwrap();
            let ws = rustman_core::Workspace::load(&root).unwrap();
            let mut setup = Setup::new(&root);
            setup.environments = &ws.environments;
            let parsed = Format::Postman
                .parse(&collection.to_string(), "c.json")
                .unwrap();
            let plan = setup.plan_collection(Format::Postman, parsed, "c.json");
            std::thread::sleep(std::time::Duration::from_millis(50));
            let save = |_: &[(&str, &rustman_core::Secret<String>)]| Ok(());
            rustman_import::apply(&plan, &root, &save).unwrap();
        })
    };
    let first = import("Alpha", "ALPHA-SECRET-1");
    let second = import("Beta", "BETA-SECRET-2");
    first.join().unwrap();
    second.join().unwrap();
    let local = std::fs::read_to_string(dir.path().join("environments/local.yaml")).unwrap();
    assert!(local.contains("x_alpha_key"), "{local}");
    assert!(local.contains("x_beta_key"), "{local}");
    assert!(dir.path().join(rustman_import::IMPORT_LOCK).exists());
}
