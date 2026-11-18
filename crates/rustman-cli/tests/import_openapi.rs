//! `rustman import openapi`: version and format detection (task 1.2),
//! sources (1.3), references (1.4), diagnostics (1.5) and the generated
//! collection (1.6), end to end through the binary.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{Server, rustman, write};

const PETSTORE_31: &str = "\
openapi: 3.1.0
info:
  title: Pet Store
  version: '1'
paths:
  /pets:
    get:
      operationId: listPets
      tags: [pets]
      responses:
        '200':
          description: OK
    post:
      operationId: createPet
      tags: [pets]
      responses:
        '201':
          description: Created
  /users:
    get:
      responses:
        '200':
          description: OK
";

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    dir
}

/// Every file under `dir`, relative and sorted.
fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        for entry in fs::read_dir(next).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path.strip_prefix(dir).unwrap().to_path_buf());
            }
        }
    }
    out.sort();
    out
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn a_yaml_3_1_document_is_imported_into_tag_folders() {
    let ws = workspace();
    write(ws.path(), "petstore.yaml", PETSTORE_31);
    let output = rustman(ws.path())
        .args(["import", "openapi", "petstore.yaml"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let api = ws.path().join("pet-store");
    assert!(api.join("pets/list-pets.rman.yaml").is_file());
    assert!(api.join("pets/create-pet.rman.yaml").is_file());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.starts_with("imported 3 operations into "),
        "{stdout}"
    );
    // The missing operationId: a warning at its pointer, and a file named
    // from the method and path.
    assert!(
        stderr(&output).contains(
            "warning: /paths/~1users/get: operation has no operationId; \
             its request file is named from the method and path"
        ),
        "{}",
        stderr(&output)
    );
    assert!(api.join("untagged/get-users.rman.yaml").is_file());
    // The generated files load: running them fails only on the variables
    // nothing defines yet (exit 3), not on invalid files (exit 2).
    let run = rustman(ws.path())
        .args(["run", "pet-store", "--reporter", "json"])
        .output()
        .unwrap();
    assert_eq!(run.status.code(), Some(3), "{}", stderr(&run));
    let report = String::from_utf8(run.stdout).unwrap();
    assert!(report.contains("baseUrl"), "{report}");
}

#[test]
fn the_format_is_detected_from_content_not_the_extension() {
    let ws = workspace();
    let json = r#"{"openapi": "3.0.3", "info": {"title": "As Json", "version": "1"},
        "paths": {"/a": {"get": {"operationId": "getA", "responses": {"200": {"description": "OK"}}}}}}"#;
    write(ws.path(), "api.yaml", json);
    write(
        ws.path(),
        "other.json",
        &PETSTORE_31.replace("Pet Store", "As Yaml"),
    );
    for source in ["api.yaml", "other.json"] {
        let output = rustman(ws.path())
            .args(["import", "openapi", source])
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(0),
            "{source}: {}",
            stderr(&output)
        );
    }
    assert!(ws.path().join("as-json/untagged/get-a.rman.yaml").is_file());
    assert!(ws.path().join("as-yaml/pets/list-pets.rman.yaml").is_file());
}

#[test]
fn swagger_2_is_rejected_without_touching_the_workspace() {
    let ws = workspace();
    write(
        ws.path(),
        "legacy.json",
        r#"{"swagger": "2.0", "info": {"title": "L", "version": "1"}, "paths": {}}"#,
    );
    let before = files(ws.path());
    let output = rustman(ws.path())
        .args(["import", "openapi", "legacy.json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        stderr(&output).trim_end(),
        "error: Swagger 2.0 is not supported; convert it to OpenAPI 3 first (e.g. with swagger2openapi)"
    );
    assert_eq!(files(ws.path()), before);
}

#[test]
fn other_versions_and_non_openapi_documents_exit_2() {
    let ws = workspace();
    write(
        ws.path(),
        "v4.yaml",
        "openapi: 4.0.0\ninfo: {title: x, version: '1'}\n",
    );
    write(ws.path(), "plain.yaml", "title: not an api\n");
    write(ws.path(), "broken.json", "{\"openapi\": ");
    for (source, message) in [
        (
            "v4.yaml",
            "error: OpenAPI 4.0.0 is not supported (expected 3.0.x or 3.1.x)",
        ),
        (
            "plain.yaml",
            "error: plain.yaml: not an OpenAPI document (no 'openapi' field)",
        ),
        ("broken.json", "error: broken.json: invalid JSON"),
    ] {
        let output = rustman(ws.path())
            .args(["import", "openapi", source])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{source}");
        assert!(
            stderr(&output).starts_with(message),
            "{source}: {}",
            stderr(&output)
        );
    }
}

#[test]
fn structural_errors_exit_2_with_pointers_and_write_nothing() {
    let ws = workspace();
    write(
        ws.path(),
        "bad.yaml",
        "openapi: 3.1.0\ninfo:\n  version: '1'\npaths:\n  /a:\n    get:\n      responses: {}\n",
    );
    let before = files(ws.path());
    let output = rustman(ws.path())
        .args(["import", "openapi", "bad.yaml"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        stderr(&output),
        "warning: /paths/~1a/get: operation has no operationId; its request file is named from the method and path\n\
         error: /info/title: required string 'title' is missing\n"
    );
    assert_eq!(files(ws.path()), before);
}

#[test]
fn colliding_names_get_a_suffix_and_a_warning() {
    let ws = workspace();
    let doc = "openapi: 3.1.0\ninfo: {title: C, version: '1'}\npaths:\n  \
               /a:\n    get: {operationId: getThing, responses: {'200': {description: OK}}}\n  \
               /b:\n    get: {operationId: get_thing, responses: {'200': {description: OK}}}\n";
    write(ws.path(), "c.yaml", doc);
    let output = rustman(ws.path())
        .args(["import", "openapi", "c.yaml", "--into", "out"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(ws.path().join("out/untagged/get-thing.rman.yaml").is_file());
    assert!(
        ws.path()
            .join("out/untagged/get-thing-2.rman.yaml")
            .is_file()
    );
    assert!(
        stderr(&output)
            .contains("warning: /paths/~1b/get: the file name 'get-thing' is already used"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn an_existing_collection_is_not_overwritten() {
    let ws = workspace();
    write(ws.path(), "petstore.yaml", PETSTORE_31);
    write(
        ws.path(),
        "pet-store/mine.rman.yaml",
        "version: 1\nkind: http\nname: mine\nurl: http://x\n",
    );
    let output = rustman(ws.path())
        .args(["import", "openapi", "petstore.yaml"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).contains("already exists and is not empty"),
        "{}",
        stderr(&output)
    );
    assert_eq!(
        files(&ws.path().join("pet-store")),
        [PathBuf::from("mine.rman.yaml")]
    );
}

#[test]
fn remote_references_are_refused_unless_allowed() {
    let server = Server::http();
    let ws = workspace();
    let doc = format!(
        "openapi: 3.1.0\ninfo: {{title: R, version: '1'}}\npaths:\n  /p:\n    get:\n      \
         operationId: getP\n      responses:\n        '200':\n          description: OK\n          \
         content:\n            application/json:\n              schema:\n                \
         $ref: '{}#/Pet'\n",
        server.url("/openapi/pet.json")
    );
    write(ws.path(), "r.yaml", &doc);
    let refused = rustman(ws.path())
        .args(["import", "openapi", "r.yaml"])
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(2));
    assert!(
        stderr(&refused).contains("pass --allow-remote-refs to fetch it"),
        "{}",
        stderr(&refused)
    );
    assert!(!ws.path().join("r").exists());
    let allowed = rustman(ws.path())
        .args(["import", "openapi", "r.yaml", "--allow-remote-refs"])
        .output()
        .unwrap();
    assert_eq!(allowed.status.code(), Some(0), "{}", stderr(&allowed));
    assert!(ws.path().join("r/untagged/get-p.rman.yaml").is_file());
}

#[test]
fn a_document_is_imported_from_a_url() {
    let server = Server::http();
    let ws = workspace();
    let output = rustman(ws.path())
        .args(["import", "openapi", &server.url("/openapi/petstore.yaml")])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(
        ws.path()
            .join("url-petstore/pets/list-pets.rman.yaml")
            .is_file()
    );
}

#[test]
fn an_oversized_url_exits_2_naming_the_limit() {
    let server = Server::http();
    let ws = workspace();
    let before = files(ws.path());
    let url = server.url(&format!("/bytes/{}", 25 * 1024 * 1024));
    let output = rustman(ws.path())
        .args(["import", "openapi", &url])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        stderr(&output).trim_end(),
        format!("error: {url} is larger than the 20 MiB limit")
    );
    assert_eq!(files(ws.path()), before);
}

#[test]
fn credentials_in_the_source_url_never_reach_the_output() {
    let server = Server::http();
    let ws = workspace();
    let canary = "canary-pw-5b1e";
    let closed = common::closed_port();
    for url in [
        server.url("/status/404"),
        server.url(&format!("/bytes/{}", 21 * 1024 * 1024)),
        format!("http://localhost:{closed}/openapi.json"),
    ] {
        let url = url.replace("://", &format!("://ana:{canary}@"));
        let output = rustman(ws.path())
            .args(["import", "openapi", &url])
            .output()
            .unwrap();
        assert_ne!(output.status.code(), Some(0));
        let all = format!(
            "{}{}",
            stderr(&output),
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(!all.contains(canary), "{all}");
        assert!(all.contains("error: "), "{all}");
    }
}

/// Every file under `dir` with its contents.
fn snapshot_tree(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    files(dir)
        .into_iter()
        .map(|rel| {
            let bytes = fs::read(dir.join(&rel)).unwrap();
            (rel, bytes)
        })
        .collect()
}

const SECURED: &str = "\
openapi: 3.1.0
info: {title: Secured, version: '1'}
servers:
  - url: https://staging.example.com
    description: Staging
  - url: https://{region}.example.com/v1
    description: Production
    variables:
      region: {default: eu}
security:
  - bearerAuth: []
paths:
  /me:
    get:
      operationId: getMe
      parameters:
        - name: X-Api-Key
          in: header
          example: canary-param-9d2f
          schema: {type: string, default: canary-param-9d2f}
      responses:
        '200': {description: OK}
  /basic:
    get:
      operationId: basicThing
      security: [{basicAuth: []}]
      responses:
        '200': {description: OK}
  /key:
    get:
      operationId: keyThing
      security: [{cookieKey: []}]
      responses:
        '200': {description: OK}
  /oauth:
    get:
      operationId: oauthThing
      security: [{oauth: [read]}]
      responses:
        '200': {description: OK}
  /public:
    get:
      operationId: publicThing
      security: []
      responses:
        '200': {description: OK}
components:
  securitySchemes:
    bearerAuth: {type: http, scheme: bearer}
    basicAuth: {type: http, scheme: basic}
    cookieKey: {type: apiKey, in: cookie, name: sid}
    oauth:
      type: oauth2
      flows:
        clientCredentials:
          tokenUrl: https://id.example.com/token
          scopes: {read: Read}
    oidc: {type: openIdConnect, openIdConnectUrl: https://id.example.com/.well-known}
";

#[test]
fn credentials_in_a_source_url_are_written_nowhere() {
    let server = Server::http();
    let ws = workspace();
    let canary = "canary-pw-71c3";
    let url = server
        .url("/openapi/petstore.yaml")
        .replace("://", &format!("://ana:{canary}@"));
    let output = rustman(ws.path())
        .args(["import", "openapi", &url])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    // The relative server URL is resolved against the source, without
    // its credentials.
    let local = fs::read_to_string(ws.path().join("environments/local.yaml")).unwrap();
    assert_eq!(
        local,
        format!("variables:\n  baseUrl: {}\n", server.url("/v1"))
    );
    let meta = fs::read_to_string(ws.path().join("url-petstore/_collection.yaml")).unwrap();
    assert!(
        meta.contains(&format!(
            "location: {}\n",
            server.url("/openapi/petstore.yaml")
        )),
        "{meta}"
    );
    for rel in files(ws.path()) {
        let text = fs::read_to_string(ws.path().join(&rel)).unwrap();
        assert!(!text.contains(canary), "{}: {text}", rel.display());
    }
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        stderr(&output)
    );
    assert!(!all.contains(canary), "{all}");
}

#[test]
fn servers_and_security_schemes_become_environments_and_auth_without_credentials() {
    let ws = workspace();
    write(ws.path(), "secured.yaml", SECURED);
    let output = rustman(ws.path())
        .args(["import", "openapi", "secured.yaml"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("created environment 'staging'"), "{stdout}");
    assert!(
        stderr(&output).contains(
            "warning: /components/securitySchemes/oidc: security scheme 'oidc' (openIdConnect) \
             is not configured; set up its auth by hand"
        ),
        "{}",
        stderr(&output)
    );
    let read = |rel: &str| fs::read_to_string(ws.path().join(rel)).unwrap();
    assert_eq!(
        read("environments/production.yaml"),
        "variables:\n  baseUrl: https://eu.example.com/v1\n  region: eu\nsecrets:\n  \
         - bearerAuth_token\n  - basicAuth_username\n  - basicAuth_password\n  - cookieKey_key\n  \
         - oauth_client_id\n  - oauth_client_secret\n"
    );
    let root = read("secured/_collection.yaml");
    assert!(
        root.contains("auth:\n  type: bearer\n  token: '{{bearerAuth_token}}'\n"),
        "{root}"
    );
    let key = read("secured/untagged/key-thing.rman.yaml");
    assert!(key.contains("value: sid={{cookieKey_key}}"), "{key}");
    let public = read("secured/untagged/public-thing.rman.yaml");
    assert!(public.contains("auth:\n  type: none\n"), "{public}");
    let oauth = read("secured/untagged/oauth-thing.rman.yaml");
    assert!(
        oauth.contains("client_secret: '{{oauth_client_secret}}'"),
        "{oauth}"
    );
    // The values documented for a credential parameter are not written to
    // any tracked file (the snapshot under .rustman/ is git-ignored).
    for rel in files(ws.path()) {
        if rel.starts_with(".rustman") || rel == Path::new("secured.yaml") {
            continue;
        }
        let text = fs::read_to_string(ws.path().join(&rel)).unwrap();
        assert!(!text.contains("canary-param"), "{}: {text}", rel.display());
    }
    // Secrets are declared without values, and the workspace loads.
    let loaded = rustman_core::Workspace::load(ws.path()).unwrap();
    let production = loaded
        .environments
        .iter()
        .find(|e| e.name == "production")
        .unwrap();
    assert!(production.is_secret("bearerAuth_token"));
    assert!(production.variables.get("bearerAuth_token").is_none());
}

#[test]
fn the_snapshot_and_the_source_hash_are_recorded() {
    let ws = workspace();
    write(ws.path(), "specs/petstore.yaml", PETSTORE_31);
    let output = rustman(ws.path())
        .args(["import", "openapi", "specs/petstore.yaml"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let snapshot = ws.path().join(".rustman/openapi/pet-store.json");
    let document: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&snapshot).unwrap()).unwrap();
    assert_eq!(document["info"]["title"], "Pet Store");
    let meta = fs::read_to_string(ws.path().join("pet-store/_collection.yaml")).unwrap();
    let hash = rustman_explore::store::sha256(PETSTORE_31);
    assert!(
        meta.ends_with(&format!(
            "source:\n  format: openapi\n  location: specs/petstore.yaml\n  sha256: '{hash}'\n"
        )),
        "{meta}"
    );
}

#[test]
fn existing_environments_are_kept_unless_syncing() {
    let ws = workspace();
    write(ws.path(), "secured.yaml", SECURED);
    write(
        ws.path(),
        "environments/staging.yaml",
        "variables:\n  baseUrl: http://localhost:8080\n",
    );
    let output = rustman(ws.path())
        .args(["import", "openapi", "secured.yaml"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(
        stderr(&output).contains(
            "warning: environment 'staging' already exists and was not changed (use --sync to update it)"
        ),
        "{}",
        stderr(&output)
    );
    assert_eq!(
        fs::read_to_string(ws.path().join("environments/staging.yaml")).unwrap(),
        "variables:\n  baseUrl: http://localhost:8080\n"
    );
}

#[test]
fn a_dry_run_sync_prints_the_summary_and_changes_no_file() {
    let ws = workspace();
    write(ws.path(), "petstore.yaml", PETSTORE_31);
    let first = rustman(ws.path())
        .args(["import", "openapi", "petstore.yaml"])
        .output()
        .unwrap();
    assert_eq!(first.status.code(), Some(0), "{}", stderr(&first));
    let changed = PETSTORE_31
        .replace(
            "    post:\n      operationId: createPet",
            "    put:\n      operationId: replacePets",
        )
        .replace("  /users:\n", "  /people:\n");
    write(ws.path(), "new.yaml", &changed);
    let before = snapshot_tree(ws.path());
    let output = rustman(ws.path())
        .args(["import", "openapi", "new.yaml", "--sync", "--dry-run"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "added: replacePets (pets/replace-pets.rman.yaml)\n\
         added: GET /people (untagged/get-people.rman.yaml)\n\
         removed: createPet (moved to _removed/create-pet.rman.yaml)\n\
         removed: GET /users (moved to _removed/get-users.rman.yaml)\n\
         2 added, 0 changed, 2 removed, 0 conflicts, 1 unchanged (dry run: nothing was written)\n"
    );
    assert_eq!(snapshot_tree(ws.path()), before);

    let json = rustman(ws.path())
        .args(["import", "openapi", "new.yaml", "--sync", "--json"])
        .output()
        .unwrap();
    assert_eq!(json.status.code(), Some(0), "{}", stderr(&json));
    let summary: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(summary["added"][0]["operationId"], "replacePets");
    assert_eq!(
        summary["removed"][0]["file"],
        "_removed/create-pet.rman.yaml"
    );
    assert_eq!(summary["dryRun"], false);
    assert!(
        ws.path()
            .join("pet-store/_removed/create-pet.rman.yaml")
            .is_file()
    );
    assert!(
        !ws.path()
            .join("pet-store/pets/create-pet.rman.yaml")
            .exists()
    );
}

#[test]
fn dry_run_and_json_need_sync() {
    let ws = workspace();
    let output = rustman(ws.path())
        .args(["import", "openapi", "x.yaml", "--dry-run"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("--sync"), "{}", stderr(&output));
}
