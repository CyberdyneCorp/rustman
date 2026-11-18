//! Workspace loading: collections, ordering, inheritance (task 2.5) and
//! path containment (task 2.4).

use std::fs;
use std::path::Path;

use rustman_core::model::{Auth, NameValue, VarValue};
use rustman_core::{ConfigError, Workspace};

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let fixture = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        fixture.write("rustman.toml", "[workspace]\nname = \"t\"\nversion = 1\n");
        fixture
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn write(&self, rel: &str, text: &str) {
        let path = self.root().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn request(&self, rel: &str, name: &str, extra: &str) {
        let text = format!("version: 1\nkind: http\nname: {name}\nurl: http://x\n{extra}");
        self.write(rel, &text);
    }

    fn load(&self) -> Result<Workspace, ConfigError> {
        Workspace::load(self.root())
    }
}

fn names(ws: &Workspace) -> Vec<String> {
    ws.requests()
        .iter()
        .map(|r| r.request().name().to_owned())
        .collect()
}

fn errors(err: ConfigError) -> Vec<String> {
    match err {
        ConfigError::Invalid(errors) => errors.iter().map(ToString::to_string).collect(),
        other => panic!("expected invalid files, got {other}"),
    }
}

#[test]
fn explicit_order_then_file_name() {
    let f = Fixture::new();
    f.request("users/login.rman.yaml", "login", "");
    f.request("users/list.rman.yaml", "list", "");
    f.request("users/delete.rman.yaml", "delete", "");
    f.write("users/_collection.yaml", "order: [login, list]\n");
    assert_eq!(names(&f.load().unwrap()), ["login", "list", "delete"]);
}

#[test]
fn without_order_entries_sort_by_file_name() {
    let f = Fixture::new();
    f.request("b.rman.yaml", "b", "");
    f.request("a/z.rman.yaml", "a/z", "");
    f.request("c.rman.yaml", "c", "");
    assert_eq!(names(&f.load().unwrap()), ["a/z", "b", "c"]);
}

#[test]
fn order_can_place_folders_and_warns_on_unknown_entries() {
    let f = Fixture::new();
    f.request("api/a.rman.yaml", "a", "");
    f.request("api/setup/s.rman.yaml", "s", "");
    f.write("api/_collection.yaml", "order: [setup, ghost]\n");
    let ws = f.load().unwrap();
    assert_eq!(names(&ws), ["s", "a"]);
    let expected = format!(
        "{}: order entry 'ghost' matches no request or folder",
        Path::new("api").join("_collection.yaml").display()
    );
    assert_eq!(ws.warnings, [expected]);
}

#[test]
fn tool_directories_are_not_collections() {
    let f = Fixture::new();
    f.request("users/get.rman.yaml", "get", "");
    f.write("users/__snapshots__/get.default.snap.yaml", "x: 1\n");
    f.write("mocks/users.rman-mock.yaml", "x: 1\n");
    f.request(".rustman/history/h.rman.yaml", "hidden", "");
    f.request("_removed/old.rman.yaml", "removed", "");
    f.write("environments/local.yaml", "variables: {}\n");
    let ws = f.load().unwrap();
    let collections: Vec<_> = ws.collections().iter().map(|c| c.name()).collect();
    assert_eq!(collections, ["users"]);
    assert_eq!(names(&ws), ["get"]);
}

#[test]
fn nested_collections_and_ids() {
    let f = Fixture::new();
    f.request("api/users/get.rman.yaml", "get", "");
    f.write("api/users/_collection.yaml", "name: User API\n");
    let ws = f.load().unwrap();
    let collections: Vec<_> = ws.collections().iter().map(|c| c.name()).collect();
    assert_eq!(collections, ["api", "User API"]);
    assert_eq!(ws.requests()[0].entry.id(), "api/users/get");
}

#[test]
fn inherited_header_is_added() {
    let f = Fixture::new();
    f.write(
        "users/_collection.yaml",
        "headers:\n  - {name: X-Team, value: core}\n  - {name: Accept, value: text/plain}\n",
    );
    f.request(
        "users/get.rman.yaml",
        "get",
        "headers:\n  - {name: accept, value: application/json}\n",
    );
    let ws = f.load().unwrap();
    let headers = ws.requests()[0].effective_headers();
    assert_eq!(
        headers,
        [
            NameValue::new("X-Team", "core"),
            NameValue::new("accept", "application/json")
        ]
    );
}

#[test]
fn disabled_headers_are_neither_sent_nor_override() {
    let f = Fixture::new();
    f.write(
        "users/_collection.yaml",
        "headers:\n  - {name: X-Team, value: core}\n",
    );
    f.request(
        "users/get.rman.yaml",
        "get",
        "headers:\n  - {name: x-team, value: mine, enabled: false}\n  - {name: X-Debug, value: '1', enabled: false}\n",
    );
    let ws = f.load().unwrap();
    let headers = ws.requests()[0].effective_headers();
    assert_eq!(headers, [NameValue::new("X-Team", "core")]);
}

#[test]
fn nearer_collection_headers_win() {
    let f = Fixture::new();
    f.write(
        "_collection.yaml",
        "headers:\n  - {name: X-Team, value: root}\n",
    );
    f.write(
        "a/_collection.yaml",
        "headers:\n  - {name: x-team, value: a}\n",
    );
    f.request("a/r.rman.yaml", "r", "");
    let ws = f.load().unwrap();
    assert_eq!(
        ws.requests()[0].effective_headers(),
        [NameValue::new("x-team", "a")]
    );
}

#[test]
fn inherited_auth_and_none_override() {
    let f = Fixture::new();
    f.write(
        "users/_collection.yaml",
        "auth: {type: bearer, token: '{{token}}'}\n",
    );
    f.request("users/a.rman.yaml", "inherits", "");
    f.request("users/b.rman.yaml", "opts out", "auth: {type: none}\n");
    let ws = f.load().unwrap();
    let requests = ws.requests();
    assert_eq!(
        requests[0].effective_auth(),
        Some(&Auth::Bearer {
            token: "{{token}}".into()
        })
    );
    assert_eq!(requests[1].effective_auth(), Some(&Auth::None));
}

#[test]
fn collection_variables_are_nearest_first() {
    let f = Fixture::new();
    f.write("_collection.yaml", "variables: {page: 1}\n");
    f.write("a/_collection.yaml", "variables: {page: 2}\n");
    f.request("a/r.rman.yaml", "r", "");
    let ws = f.load().unwrap();
    let vars = ws.requests()[0].collection_variables();
    let pages: Vec<_> = vars.iter().map(|v| v.get("page").cloned()).collect();
    assert_eq!(
        pages,
        [Some(VarValue::Integer(2)), Some(VarValue::Integer(1))]
    );
}

#[test]
fn reserved_kinds_are_listed() {
    let f = Fixture::new();
    f.write(
        "api/q.rman.yaml",
        "version: 1\nkind: graphql\nname: q\nquery: '{ a }'\n",
    );
    let ws = f.load().unwrap();
    let request = ws.find_by_name("q");
    assert_eq!(request.len(), 1);
    assert!(request[0].request().unsupported_reason().is_some());
}

#[test]
fn invalid_files_are_all_reported() {
    let f = Fixture::new();
    f.write("a.rman.yaml", "version: 1\nkind: soap\nname: a\n");
    f.write("b/_collection.yaml", "nme: x\n");
    f.request("b/c.rman.yaml", "c", "");
    let errs = errors(f.load().unwrap_err());
    assert_eq!(errs.len(), 2, "{errs:?}");
    assert_eq!(
        errs[0],
        "a.rman.yaml:2: unknown request kind 'soap' (expected one of: http, graphql, mcp)"
    );
    assert!(errs[1].starts_with(&format!(
        "{}:1: unknown field `nme`",
        Path::new("b").join("_collection.yaml").display()
    )));
}

#[test]
fn newer_settings_version_fails_to_load() {
    let f = Fixture::new();
    f.write("rustman.toml", "[workspace]\nname = \"t\"\nversion = 2\n");
    assert!(matches!(
        f.load(),
        Err(ConfigError::NewerSettingsVersion(2))
    ));
}

#[test]
fn discover_from_a_nested_folder() {
    let f = Fixture::new();
    f.request("api/users/get.rman.yaml", "get", "");
    let ws = Workspace::discover(&f.root().join("api/users"), None).unwrap();
    assert_eq!(ws.root, f.root());
}

#[test]
fn path_escape_is_rejected() {
    let f = Fixture::new();
    f.request(
        "users/x.rman.yaml",
        "x",
        "body: {type: file, path: ../../etc/passwd}\n",
    );
    let errs = errors(f.load().unwrap_err());
    let file = Path::new("users").join("x.rman.yaml").display().to_string();
    assert_eq!(
        errs,
        [format!(
            "{file}:5: file path escapes workspace: ../../etc/passwd"
        )]
    );
}

#[test]
fn every_kind_of_file_reference_is_contained() {
    let f = Fixture::new();
    let extra = "\
body:
  type: multipart
  parts:
    - {name: a, file: ../../a.png}
settings:
  ca_bundle: ../../ca.pem
  client_cert: /etc/cert.pem
  client_key: ../../key.pem
  client_pkcs12: ../../c.p12
assertions:
  - schema: {file: ../../s.json}
";
    f.request("users/x.rman.yaml", "x", extra);
    assert_eq!(errors(f.load().unwrap_err()).len(), 6);
}

#[test]
fn paths_inside_the_workspace_and_templated_paths_are_accepted_at_load() {
    let f = Fixture::new();
    f.request(
        "users/x.rman.yaml",
        "x",
        "body: {type: file, path: ../fixtures/blob.bin}\n",
    );
    f.request(
        "users/y.rman.yaml",
        "y",
        "body: {type: file, path: '{{dir}}/blob.bin'}\n",
    );
    assert!(f.load().is_ok());
}

#[test]
fn settings_ca_bundle_is_contained() {
    let f = Fixture::new();
    f.write(
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n[http]\nca_bundle = \"../ca.pem\"\n",
    );
    assert_eq!(
        errors(f.load().unwrap_err()),
        ["rustman.toml:5: file path escapes workspace: ../ca.pem"]
    );
}

#[test]
fn settings_client_certificate_paths_are_contained() {
    let f = Fixture::new();
    f.write(
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n[http]\nclient_cert = \"certs/c.pem\"\nclient_key = \"/etc/k.pem\"\nclient_pkcs12 = \"../c.p12\"\n",
    );
    assert_eq!(
        errors(f.load().unwrap_err()),
        [
            "rustman.toml:6: file path escapes workspace: /etc/k.pem",
            "rustman.toml:7: file path escapes workspace: ../c.p12",
        ]
    );
}

#[test]
fn settings_ca_bundle_is_not_interpolated_so_braces_do_not_skip_the_check() {
    let f = Fixture::new();
    f.write(
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n[http]\nca_bundle = \"../{{x}}.pem\"\n",
    );
    assert_eq!(
        errors(f.load().unwrap_err()),
        ["rustman.toml:5: file path escapes workspace: ../{{x}}.pem"]
    );
}

#[cfg(unix)]
#[test]
fn symlink_pointing_outside_is_rejected() {
    let f = Fixture::new();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("passwd"), "x").unwrap();
    std::os::unix::fs::symlink(outside.path().join("passwd"), f.root().join("link")).unwrap();
    f.request("x.rman.yaml", "x", "body: {type: file, path: link}\n");
    assert_eq!(
        errors(f.load().unwrap_err()),
        ["x.rman.yaml:5: file path escapes workspace: link"]
    );
}

#[cfg(unix)]
#[test]
fn symlinked_request_file_pointing_outside_is_rejected() {
    let f = Fixture::new();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("evil.rman.yaml");
    fs::write(
        &target,
        "version: 1\nkind: http\nname: evil\nurl: http://x\n",
    )
    .unwrap();
    std::os::unix::fs::symlink(&target, f.root().join("evil.rman.yaml")).unwrap();
    assert_eq!(
        errors(f.load().unwrap_err()),
        ["evil.rman.yaml: file path escapes workspace: evil.rman.yaml"]
    );
}

#[cfg(unix)]
#[test]
fn symlinked_folders_are_not_followed() {
    let f = Fixture::new();
    let outside = tempfile::tempdir().unwrap();
    fs::write(
        outside.path().join("r.rman.yaml"),
        "version: 1\nkind: http\nname: r\nurl: http://x\n",
    )
    .unwrap();
    std::os::unix::fs::symlink(outside.path(), f.root().join("linked")).unwrap();
    assert!(f.load().unwrap().requests().is_empty());
}

#[test]
fn requests_are_found_by_file_id_or_unique_name() {
    let f = Fixture::new();
    f.request("api/me.rman.yaml", "Me", "");
    f.request("a/list.rman.yaml", "List", "");
    f.request("b/list.rman.yaml", "List", "");
    let ws = f.load().unwrap();
    let id = |target: &str, cwd: &Path| ws.find_request(target, cwd).unwrap().entry.id();
    assert_eq!(id("Me", f.root()), "api/me");
    assert_eq!(id("api/me", f.root()), "api/me");
    assert_eq!(id("api/me.rman.yaml", f.root()), "api/me");
    assert_eq!(id("me.rman.yaml", &f.root().join("api")), "api/me");
    // A path wins over a name, so an ambiguous name stays reachable.
    assert_eq!(id("a/list.rman.yaml", f.root()), "a/list");
    let err = ws.find_request("List", f.root()).unwrap_err();
    assert_eq!(
        err.to_string(),
        "request name 'List' is ambiguous: a/list.rman.yaml, b/list.rman.yaml"
    );
    let err = ws.find_request("nope", f.root()).unwrap_err();
    assert!(matches!(err, ConfigError::RequestNotFound(_)));
}
