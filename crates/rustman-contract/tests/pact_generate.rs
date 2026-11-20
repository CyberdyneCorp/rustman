//! Pact generation (tasks 5.1, 5.2; spec "Pact generation from a
//! collection", "Pact specification versions", "Redaction in contract
//! outputs"): a workspace with a secret token is turned into Pact files,
//! which must hold no secret value and validate against the published Pact
//! JSON schemas.

use std::fs;
use std::path::Path;

use rustman_contract::pact::{Generated, Generator, PactVersion};
use rustman_core::vars::SecretLookup;
use rustman_core::{Redactor, Secret, Workspace};
use serde_json::{Value, json};

const TOKEN: &str = "canary-tok-7f3a9c";

struct Store;

impl SecretLookup for Store {
    fn secret(&self, env: &str, name: &str) -> Result<Option<String>, String> {
        Ok((env == "dev" && name == "token").then(|| TOKEN.to_owned()))
    }
}

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    write(
        root,
        "environments/dev.yaml",
        "variables:\n  base_url: http://localhost:8080\n  user_id: \"42\"\nsecrets: [token]\n",
    );
    write(
        root,
        "users/get-user.rman.yaml",
        "version: 1\nkind: http\nname: get user\nmethod: GET\nurl: \"{{base_url}}/users/{{user_id}}\"\n\
         headers:\n  - name: Authorization\n    value: \"Bearer {{token}}\"\n\
         assertions:\n  - status: 200\n  - json: {path: \"$.id\", equals: 42}\n  - latency: {total_lt: 300}\n",
    );
    write(
        root,
        "users/create-user.rman.yaml",
        "version: 1\nkind: http\nname: create user\nmethod: POST\nurl: \"{{base_url}}/users?trace={{$uuid}}\"\n\
         body:\n  type: json\n  content:\n    name: Ana\n    api_key: \"{{token}}\"\n    echo: \"key {{token}} end\"\n\
         assertions:\n  - status: 2xx\n  - json: {path: \"$.id\", type: integer}\n",
    );
    write(
        root,
        "users/q.rman.yaml",
        "version: 1\nkind: graphql\nname: q\nendpoint: \"{{base_url}}/graphql\"\ndocument: \"{ me { id } }\"\n",
    );
    dir
}

fn generate(root: &Path, version: PactVersion) -> Generated {
    let ws = Workspace::load(root).unwrap_or_else(|e| panic!("{e}"));
    let env = ws.active_environment(Some("dev"), None).unwrap();
    let redactor = Redactor::new().with(&Secret::new("token", TOKEN.to_owned()));
    let generator = Generator {
        settings: &ws.settings,
        environment: env,
        secrets: &Store,
        redactor: &redactor,
        version,
        recorded_status: &|file: &str| (file == "users/create-user.rman.yaml").then_some(201),
    };
    generator
        .generate("web", "users", &ws.requests())
        .unwrap_or_else(|e| panic!("{e}"))
}

fn interaction<'a>(pact: &'a Value, description: &str) -> &'a Value {
    pact["interactions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["description"] == description)
        .unwrap_or_else(|| panic!("no interaction {description} in {pact:#}"))
}

/// The published schema, with the `status` matching-rule category the
/// Pact reference implementation reads for `statusCode` matchers.
fn schema(version: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../fixtures/contract/pact-schemas/pact-schema-{version}.json"
    ));
    let mut schema: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    let rules = &mut schema["definitions"]["matchingRules"]["properties"];
    rules["status"] = json!({"$ref": "#/definitions/bodyMatcher"});
    schema
}

fn assert_valid(pact: &Value, version: &str) {
    let validator = jsonschema::draft7::new(&schema(version)).unwrap();
    let errors: Vec<String> = validator
        .iter_errors(pact)
        .map(|e| format!("{}: {e}", e.instance_path()))
        .collect();
    assert!(errors.is_empty(), "{errors:#?}\n{pact:#}");
}

#[test]
fn an_interaction_per_http_request_from_its_file_and_assertions() {
    let dir = workspace();
    let generated = generate(dir.path(), PactVersion::V4);
    let pact = generated.pact.to_json();
    assert_eq!(pact["consumer"]["name"], "web");
    assert_eq!(pact["provider"]["name"], "users");
    assert_eq!(pact["metadata"]["pactSpecification"]["version"], "4.0");
    let get = interaction(&pact, "get user");
    assert_eq!(get["type"], "Synchronous/HTTP");
    assert_eq!(get["request"]["method"], "GET");
    assert_eq!(get["request"]["path"], "/users/42");
    assert_eq!(get["response"]["status"], 200);
    assert_eq!(get["response"]["body"]["content"], json!({"id": 42}));
    assert_eq!(
        get["response"]["matchingRules"]["body"]["$.id"]["matchers"][0]["match"],
        "equality"
    );
    assert_eq!(pact["interactions"].as_array().unwrap().len(), 2);
    assert_valid(&pact, "v4");
}

#[test]
fn a_secret_header_becomes_a_regex_matcher_with_a_placeholder_example() {
    let dir = workspace();
    let pact = generate(dir.path(), PactVersion::V4).pact.to_json();
    let get = interaction(&pact, "get user");
    assert_eq!(
        get["request"]["headers"]["Authorization"],
        "Bearer «redacted:token»"
    );
    assert_eq!(
        get["request"]["matchingRules"]["header"]["Authorization"]["matchers"][0],
        json!({"match": "regex", "regex": "^Bearer .+$"})
    );
}

#[test]
fn secret_and_dynamic_values_in_bodies_and_queries_are_matchers() {
    let dir = workspace();
    let pact = generate(dir.path(), PactVersion::V4).pact.to_json();
    let create = interaction(&pact, "create user");
    let request = &create["request"];
    assert_eq!(request["query"]["trace"], json!(["«dynamic:$uuid»"]));
    assert_eq!(
        request["matchingRules"]["query"]["trace"]["matchers"][0]["regex"],
        "^.+$"
    );
    let body = &request["body"]["content"];
    assert_eq!(body["api_key"], "«redacted:token»");
    assert_eq!(body["echo"], "key «redacted:token» end");
    let rules = &request["matchingRules"]["body"];
    assert_eq!(rules["$.echo"]["matchers"][0]["regex"], "^key .+ end$");
    assert_eq!(request["headers"]["Content-Type"], "application/json");
    let status = &create["response"]["matchingRules"]["status"]["$"]["matchers"][0];
    assert_eq!(*status, json!({"match": "statusCode", "status": "success"}));
}

#[test]
fn no_secret_value_appears_in_a_generated_file() {
    let dir = workspace();
    for version in [PactVersion::V3, PactVersion::V4] {
        let text = serde_json::to_string(&generate(dir.path(), version).pact.to_json()).unwrap();
        assert!(!text.contains(TOKEN), "{text}");
        assert!(text.contains("«redacted:token»"), "{text}");
    }
}

/// A numeric secret in a json `equals` assertion becomes a redacted
/// string, not the number itself.
#[test]
fn a_numeric_secret_in_an_expected_body_is_redacted() {
    let dir = workspace();
    write(
        dir.path(),
        "users/pin.rman.yaml",
        "version: 1\nkind: http\nname: pin\nmethod: GET\nurl: \"{{base_url}}/pin\"\n\
         assertions:\n  - json: {path: \"$.pin\", equals: 90817263}\n",
    );
    let ws = Workspace::load(dir.path()).unwrap_or_else(|e| panic!("{e}"));
    let env = ws.active_environment(Some("dev"), None).unwrap();
    let redactor = Redactor::new().with(&Secret::new("pin", "90817263".to_owned()));
    let generator = Generator {
        settings: &ws.settings,
        environment: env,
        secrets: &Store,
        redactor: &redactor,
        version: PactVersion::V4,
        recorded_status: &|_: &str| None,
    };
    let generated = generator
        .generate("web", "users", &ws.requests())
        .unwrap_or_else(|e| panic!("{e}"));
    let text = serde_json::to_string(&generated.pact.to_json()).unwrap();
    assert!(!text.contains("90817263"), "{text}");
    let pin = interaction(&generated.pact.to_json(), "pin")["response"]["body"].clone();
    assert_eq!(pin["content"]["pin"], "«redacted:pin»", "{pin:#}");
}

#[test]
fn version_3_files_validate_and_say_so() {
    let dir = workspace();
    let generated = generate(dir.path(), PactVersion::V3);
    let pact = generated.pact.to_json();
    assert_eq!(pact["metadata"]["pactSpecification"]["version"], "3.0.0");
    let get = interaction(&pact, "get user");
    assert!(get.get("type").is_none());
    assert_eq!(get["response"]["body"], json!({"id": 42}));
    let create = interaction(&pact, "create user");
    assert_eq!(
        create["response"]["status"], 201,
        "the recorded status of a 2xx assertion"
    );
    assert!(create["response"]["matchingRules"].get("status").is_none());
    assert_valid(&pact, "v3");
}

#[test]
fn omitted_assertions_and_skipped_kinds_warn() {
    let dir = workspace();
    let generated = generate(dir.path(), PactVersion::V4);
    assert_eq!(
        generated.warnings,
        vec![
            "latency assertion has no Pact equivalent (users/get-user.rman.yaml)",
            "users/q.rman.yaml: kind graphql skipped (only http requests become Pact interactions)",
        ]
    );
}

#[test]
fn an_unresolved_variable_stops_generation() {
    let dir = workspace();
    write(
        dir.path(),
        "users/broken.rman.yaml",
        "version: 1\nkind: http\nname: broken\nurl: \"{{nowhere}}/x\"\n",
    );
    let ws = Workspace::load(dir.path()).unwrap();
    let generator = Generator {
        settings: &ws.settings,
        environment: None,
        secrets: &Store,
        redactor: &Redactor::new(),
        version: PactVersion::V4,
        recorded_status: &|_: &str| None,
    };
    let error = generator
        .generate("web", "users", &ws.requests())
        .unwrap_err();
    assert!(error.to_string().starts_with("users/"), "{error}");
    assert!(error.to_string().contains("unresolved variable"), "{error}");
}

#[test]
fn the_schema_check_rejects_an_invalid_file() {
    let dir = workspace();
    let mut pact = generate(dir.path(), PactVersion::V4).pact.to_json();
    pact["interactions"][0]["request"]
        .as_object_mut()
        .unwrap()
        .remove("path");
    let validator = jsonschema::draft7::new(&schema("v4")).unwrap();
    assert!(!validator.is_valid(&pact));
}
