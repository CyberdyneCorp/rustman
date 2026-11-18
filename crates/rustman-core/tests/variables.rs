//! Environments (task 3.1), the resolver over a real workspace (task 3.2) and
//! dynamic variables inside requests (task 3.3).

use std::fs;
use std::io::ErrorKind;
use std::net::{TcpListener, TcpStream};
use std::path::Path;

use rustman_core::model::{Auth, VarValue, Variables};
use rustman_core::vars::{
    Layer, NoSecrets, ResolveError, ResolvedBody, ResolvedPartSource, ResolvedRequest, Resolver,
    SecretLookup, VariablesSource, parse_overrides, resolve_request,
};
use rustman_core::{ConfigError, Workspace};

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new(settings_extra: &str) -> Self {
        let fixture = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        let toml = format!("[workspace]\nname = \"t\"\nversion = 1\n{settings_extra}");
        fixture.write("rustman.toml", &toml);
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

    fn load(&self) -> Workspace {
        Workspace::load(self.root()).unwrap_or_else(|e| panic!("{e}"))
    }
}

fn two_envs() -> Fixture {
    let f = Fixture::new("[defaults]\ndefault_environment = \"local\"\n");
    f.write(
        "environments/local.yaml",
        "variables:\n  base_url: http://local\n",
    );
    f.write(
        "environments/staging.yaml",
        "variables:\n  base_url: http://staging\n",
    );
    f.write(
        "environments/prod.yaml",
        "variables:\n  base_url: http://prod\n",
    );
    f
}

fn active(ws: &Workspace, flag: Option<&str>, env_var: Option<&str>) -> Option<String> {
    ws.active_environment(flag, env_var)
        .unwrap()
        .map(|e| e.name.clone())
}

mod environments {
    use super::*;

    #[test]
    fn are_loaded_sorted_by_name() {
        let ws = two_envs().load();
        let names: Vec<&str> = ws.environments.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["local", "prod", "staging"]);
        assert!(ws.warnings.is_empty(), "{:?}", ws.warnings);
        assert!(
            ws.collections().is_empty(),
            "environments/ is not a collection"
        );
    }

    #[test]
    fn flag_beats_env_var_beats_default() {
        let ws = two_envs().load();
        assert_eq!(
            active(&ws, Some("prod"), Some("staging")).as_deref(),
            Some("prod")
        );
        assert_eq!(
            active(&ws, None, Some("staging")).as_deref(),
            Some("staging")
        );
        assert_eq!(active(&ws, None, None).as_deref(), Some("local"));
    }

    #[test]
    fn none_is_active_without_a_default() {
        let f = Fixture::new("");
        f.write("environments/local.yaml", "variables: {}\n");
        assert_eq!(active(&f.load(), None, None), None);
    }

    #[test]
    fn missing_environment_is_a_config_error() {
        let ws = two_envs().load();
        let err = ws.active_environment(Some("qa"), None).unwrap_err();
        assert!(matches!(err, ConfigError::EnvironmentNotFound { .. }));
        assert_eq!(
            err.to_string(),
            "environment 'qa' not found (available: local, prod, staging)"
        );
        let err = ws.active_environment(None, Some("qa")).unwrap_err();
        assert!(err.to_string().starts_with("environment 'qa' not found"));
    }

    #[test]
    fn missing_default_environment_is_a_config_error() {
        let f = Fixture::new("[defaults]\ndefault_environment = \"gone\"\n");
        let err = f.load().active_environment(None, None).unwrap_err();
        assert_eq!(
            err.to_string(),
            "environment 'gone' not found (available: none)"
        );
    }

    #[test]
    fn invalid_name_is_ignored_with_a_warning() {
        let f = two_envs();
        f.write("environments/my env.yaml", "variables: {}\n");
        f.write("environments/notes.txt", "not an environment\n");
        let ws = f.load();
        assert_eq!(ws.warnings, ["ignoring environment 'my env': invalid name"]);
        assert_eq!(ws.environments.len(), 3);
    }

    #[test]
    fn invalid_file_reports_path_and_line() {
        let f = two_envs();
        f.write("environments/bad.yaml", "variables:\n  a: 1\nsecret: [x]\n");
        let err = Workspace::load(f.root()).unwrap_err();
        let text = err.to_string();
        let expected = Path::new("environments").join("bad.yaml");
        assert!(
            text.starts_with(&format!("{}:3: unknown field `secret`", expected.display())),
            "{text}"
        );
    }
}

/// A store holding `api_token` for `prod`.
struct Store;

impl SecretLookup for Store {
    fn secret(&self, env: &str, name: &str) -> Result<Option<String>, String> {
        Ok((env == "prod" && name == "api_token").then(|| "s3cr3t".to_owned()))
    }
}

/// A workspace where `x` is defined at every file-backed layer.
fn layered() -> Fixture {
    let f = Fixture::new("[variables]\nx = \"settings\"\nonly_settings = 1\n");
    f.write(
        "environments/prod.yaml",
        "variables:\n  x: env\n  only_env: true\nsecrets: [api_token]\n",
    );
    f.write(
        "api/_collection.yaml",
        "variables:\n  x: root-folder\n  only_root: r\n",
    );
    f.write(
        "api/users/_collection.yaml",
        "variables:\n  x: users-folder\n",
    );
    f.write(
        "api/users/get.rman.yaml",
        "version: 1\nkind: http\nname: get\nurl: '{{base}}/{{x}}'\nvariables:\n  x: request\n",
    );
    f.write(
        "api/users/list.rman.yaml",
        "version: 1\nkind: http\nname: list\nurl: '{{x}}|{{only_root}}|{{only_env}}|{{only_settings}}'\n",
    );
    f
}

fn url_of(
    ws: &Workspace,
    name: &str,
    extra: &[(Layer, &[(&str, &str)])],
) -> Result<String, ResolveError> {
    let request = &ws.find_by_name(name)[0];
    let env = ws.active_environment(Some("prod"), None).unwrap();
    let mut resolver = Resolver::for_request(&ws.settings, request, env, &Store);
    for (layer, pairs) in extra {
        let vars = Variables::new(
            pairs
                .iter()
                .map(|(n, v)| ((*n).to_owned(), VarValue::String((*v).to_owned())))
                .collect(),
        );
        resolver.push(VariablesSource::owned(*layer, vars));
    }
    resolve_request(request, &resolver).map(|r| r.url.text)
}

mod precedence {
    use super::*;

    #[test]
    fn nearest_definition_wins_through_file_layers() {
        let ws = layered().load();
        assert_eq!(url_of(&ws, "list", &[]).unwrap(), "users-folder|r|true|1");
        assert_eq!(
            url_of(&ws, "get", &[(Layer::Settings, &[("base", "b")])]).unwrap(),
            "b/request"
        );
    }

    #[test]
    fn runtime_layers_beat_file_layers() {
        let ws = layered().load();
        let base: (Layer, &[(&str, &str)]) = (Layer::Settings, &[("base", "b")]);
        let cases: [(Layer, &str); 3] = [
            (Layer::Override, "override"),
            (Layer::Capture, "capture"),
            (Layer::DataRow, "row"),
        ];
        for (layer, value) in cases {
            let pairs = [("x", value)];
            let url = url_of(&ws, "get", &[base, (layer, &pairs)]).unwrap();
            assert_eq!(url, format!("b/{value}"));
        }
        let all: [(Layer, &[(&str, &str)]); 4] = [
            base,
            (Layer::DataRow, &[("x", "row")]),
            (Layer::Capture, &[("x", "capture")]),
            (Layer::Override, &[("x", "override")]),
        ];
        assert_eq!(url_of(&ws, "get", &all).unwrap(), "b/override");
    }

    #[test]
    fn persisted_captures_sit_between_collections_and_environment() {
        let ws = layered().load();
        let persisted = (
            Layer::PersistedCapture,
            &[("only_env", "persisted"), ("x", "persisted")][..],
        );
        assert_eq!(
            url_of(&ws, "list", &[persisted]).unwrap(),
            "users-folder|r|persisted|1"
        );
    }

    #[test]
    fn override_from_the_command_line_beats_environment() {
        let f = Fixture::new("");
        f.write(
            "environments/local.yaml",
            "variables:\n  base_url: http://localhost:8080\n",
        );
        f.write(
            "r.rman.yaml",
            "version: 1\nkind: http\nname: r\nurl: '{{base_url}}/users'\n",
        );
        let ws = f.load();
        let request = &ws.find_by_name("r")[0];
        let env = ws.active_environment(Some("local"), None).unwrap();
        let overrides = parse_overrides(&["base_url=http://127.0.0.1:9000"]).unwrap();
        let resolver = Resolver::for_request(&ws.settings, request, env, &NoSecrets)
            .with(VariablesSource::owned(Layer::Override, overrides));
        let resolved = resolve_request(request, &resolver).unwrap();
        assert_eq!(resolved.url.text, "http://127.0.0.1:9000/users");
    }
}

mod requests {
    use super::*;

    fn resolve(f: &Fixture, name: &str) -> Result<ResolvedRequest, ResolveError> {
        let ws = f.load();
        let request = &ws.find_by_name(name)[0];
        let env = ws.active_environment(Some("prod"), None).unwrap();
        let resolver = Resolver::for_request(&ws.settings, request, env, &Store)
            .with_os_env(|n| (n == "CI_TOKEN").then(|| "ci".to_owned()));
        resolve_request(request, &resolver)
    }

    fn fixture() -> Fixture {
        let f = Fixture::new("[variables]\nhost = \"h\"\nv = \"val\"\n");
        f.write("environments/prod.yaml", "secrets: [api_token]\n");
        f.write(
            "api/_collection.yaml",
            "headers:\n  - name: X-Inherited\n    value: '{{v}}'\n",
        );
        f
    }

    #[test]
    fn every_field_is_interpolated() {
        let f = fixture();
        f.write(
            "api/all.rman.yaml",
            r#"version: 1
kind: http
name: all
method: POST
url: 'http://{{host}}/x'
query:
  - name: '{{v}}-q'
    value: '{{v}}'
headers:
  - name: Authorization
    value: 'Bearer {{api_token}}'
body:
  type: json
  content:
    '{{v}}': ['{{v}}', 1, true, '\{{literal}}']
auth:
  type: api_key
  name: X-Key
  value: '{{$env CI_TOKEN}}'
settings:
  proxy: 'http://{{host}}:3128'
  ca_bundle: 'certs/{{v}}.pem'
"#,
        );
        let r = resolve(&f, "all").unwrap();
        assert_eq!(r.method, "POST");
        assert_eq!(r.url.text, "http://h/x");
        assert_eq!(
            (
                r.query[0].name.text.as_str(),
                r.query[0].value.text.as_str()
            ),
            ("val-q", "val")
        );
        let headers: Vec<(&str, &str)> = r
            .headers
            .iter()
            .map(|h| (h.name.text.as_str(), h.value.text.as_str()))
            .collect();
        assert_eq!(
            headers,
            [("X-Inherited", "val"), ("Authorization", "Bearer s3cr3t")]
        );
        assert_eq!(r.headers[1].value.secret_spans[0].range, 7..13);
        assert_eq!(r.secrets.len(), 1);
        assert_eq!(
            r.body,
            ResolvedBody::Json(serde_json::json!({"val": ["val", 1, true, "{{literal}}"]}))
        );
        let Some(Auth::ApiKey { value, .. }) = &r.auth else {
            panic!("{:?}", r.auth)
        };
        assert_eq!(value, "ci");
        assert_eq!(r.settings.proxy.as_deref(), Some("http://h:3128"));
        assert_eq!(
            r.settings.ca_bundle.as_deref().map(String::as_str),
            Some("certs/val.pem")
        );
    }

    #[test]
    fn form_multipart_text_and_paths_are_interpolated() {
        let f = fixture();
        f.write(
            "api/multi.rman.yaml",
            "version: 1\nkind: http\nname: multi\nurl: http://x\nbody:\n  type: multipart\n  parts:\n    - {name: a, value: '{{v}}'}\n    - {name: f, file: 'files/{{v}}.png'}\n",
        );
        f.write(
            "api/text.rman.yaml",
            "version: 1\nkind: http\nname: text\nurl: http://x\nbody:\n  type: text\n  content: '\\{{not_a_var}} {{v}}'\n",
        );
        let ResolvedBody::Multipart(parts) = resolve(&f, "multi").unwrap().body else {
            panic!("expected multipart")
        };
        assert_eq!(
            parts[0].source,
            ResolvedPartSource::Text(rustman_core::vars::Resolved::literal("val"))
        );
        let ResolvedPartSource::File(file) = &parts[1].source else {
            panic!("expected a file part")
        };
        assert_eq!(file.raw, "files/val.png");
        assert_eq!(file.path, f.root().join("api/files/val.png"));
        let ResolvedBody::Text(text) = resolve(&f, "text").unwrap().body else {
            panic!("expected text")
        };
        assert_eq!(text.text, "{{not_a_var}} val");
    }

    fn escape_fixture(body: &str) -> Fixture {
        let f = Fixture::new("[variables]\np = \"../../../../etc/passwd\"\nok = \"files\"\n");
        f.write("environments/prod.yaml", "secrets: [api_token]\n");
        f.write(
            "api/x.rman.yaml",
            &format!("version: 1\nkind: http\nname: x\nurl: http://x\n{body}"),
        );
        f
    }

    #[test]
    fn templated_file_paths_that_escape_are_rejected_after_interpolation() {
        let bodies = [
            "body: {type: file, path: '{{p}}'}\n",
            "body:\n  type: multipart\n  parts:\n    - {name: f, file: '{{p}}'}\n",
            "settings: {ca_bundle: '{{p}}'}\n",
            "settings: {client_key: '{{ok}}/../../../k.pem'}\n",
        ];
        for body in bodies {
            let f = escape_fixture(body);
            let err = resolve(&f, "x").unwrap_err();
            assert!(matches!(err, ResolveError::PathEscape(_)), "{body}: {err}");
            assert!(
                err.to_string().starts_with("file path escapes workspace: "),
                "{err}"
            );
        }
        let f = escape_fixture("body: {type: file, path: '{{p}}'}\n");
        assert_eq!(
            resolve(&f, "x").unwrap_err().to_string(),
            "file path escapes workspace: ../../../../etc/passwd"
        );
    }

    #[test]
    fn templated_file_paths_inside_the_workspace_resolve() {
        let f = escape_fixture(
            "body: {type: file, path: '{{ok}}/../files/./blob.bin'}\nsettings: {ca_bundle: '../certs/ca.pem'}\n",
        );
        let resolved = resolve(&f, "x").unwrap();
        let ResolvedBody::File(file) = &resolved.body else {
            panic!("expected a file body")
        };
        // The contained path is absolute and normalized, so the transport
        // never resolves it again against the working directory.
        assert_eq!(file.raw, "files/../files/./blob.bin");
        assert_eq!(file.path, f.root().join("api/files/blob.bin"));
        let ca = resolved.tls.ca_bundle.as_ref().unwrap();
        assert_eq!(ca.path, f.root().join("certs/ca.pem"));
        assert_eq!(resolved.files().len(), 2);
    }

    fn identity_fixture(settings: &str) -> Fixture {
        let toml = "[http]\nclient_pkcs12 = \"certs/ws.p12\"\nclient_pkcs12_password = \"{{api_token}}\"\n";
        let f = Fixture::new(toml);
        f.write("environments/prod.yaml", "secrets: [api_token]\n");
        f.write(
            "api/x.rman.yaml",
            &format!("version: 1\nkind: http\nname: x\nurl: http://x\n{settings}"),
        );
        f
    }

    #[test]
    fn the_workspace_client_certificate_applies_without_one_of_its_own() {
        let f = identity_fixture("");
        let resolved = resolve(&f, "x").unwrap();
        let p12 = resolved.tls.client_pkcs12.as_ref().unwrap();
        assert_eq!(p12.raw, "certs/ws.p12");
        assert_eq!(p12.path, f.root().join("certs/ws.p12"));
        assert_eq!(
            resolved.settings.client_pkcs12_password.as_deref(),
            Some("s3cr3t")
        );
        assert!(resolved.secrets.iter().any(|s| s.expose() == "s3cr3t"));

        let own = identity_fixture("settings: {client_cert: c.pem, client_key: k.pem}\n");
        let resolved = resolve(&own, "x").unwrap();
        assert_eq!(resolved.tls.client_pkcs12, None);
        let cert = resolved.tls.client_cert.as_ref().unwrap();
        assert_eq!(cert.path, own.root().join("api/c.pem"));
        assert_eq!(resolved.settings.client_pkcs12_password, None);
    }

    #[test]
    fn an_escaping_path_built_from_a_secret_is_redacted_in_the_error() {
        let f = escape_fixture("body: {type: file, path: '../../{{api_token}}'}\n");
        let err = resolve(&f, "x").unwrap_err().to_string();
        assert!(err.starts_with("file path escapes workspace: "), "{err}");
        assert!(!err.contains("s3cr3t"), "{err}");
    }

    #[test]
    fn two_uuids_in_one_body_differ() {
        let f = fixture();
        f.write(
            "api/ids.rman.yaml",
            "version: 1\nkind: http\nname: ids\nurl: http://x\nbody:\n  type: json\n  content: '{\"a\":\"{{$uuid}}\",\"b\":\"{{$uuid}}\"}'\n",
        );
        let ResolvedBody::JsonText(text) = resolve(&f, "ids").unwrap().body else {
            panic!("expected JSON text")
        };
        let value: serde_json::Value = serde_json::from_str(&text.text).unwrap();
        let (a, b) = (value["a"].as_str().unwrap(), value["b"].as_str().unwrap());
        assert_ne!(a, b);
        for id in [a, b] {
            assert_eq!(uuid_version(id), Some('4'), "{id}");
        }
    }

    fn uuid_version(id: &str) -> Option<char> {
        let groups: Vec<&str> = id.split('-').collect();
        let lens: Vec<usize> = groups.iter().map(|g| g.len()).collect();
        let hex = id.chars().all(|c| c == '-' || c.is_ascii_hexdigit());
        (lens == [8, 4, 4, 4, 12] && hex).then(|| groups[2].chars().next().unwrap())
    }

    #[test]
    fn every_missing_name_across_fields_is_reported() {
        let f = fixture();
        f.write(
            "api/missing.rman.yaml",
            "version: 1\nkind: http\nname: missing\nurl: '{{base_url}}/users'\nheaders:\n  - {name: X-A, value: '{{token}}'}\n  - {name: X-B, value: '{{base_url}}{{$env NOT_SET}}'}\n",
        );
        assert_eq!(
            resolve(&f, "missing").unwrap_err().to_string(),
            "unresolved variable(s): base_url, token, $env NOT_SET"
        );
    }

    #[test]
    fn unset_secret_names_the_command_to_set_it() {
        let f = Fixture::new("");
        f.write("environments/prod.yaml", "secrets: [other]\n");
        f.write(
            "r.rman.yaml",
            "version: 1\nkind: http\nname: r\nurl: 'http://x/{{other}}'\n",
        );
        assert_eq!(
            resolve(&f, "r").unwrap_err().to_string(),
            "unresolved variable(s): other (secret not set; run 'rustman secret set other --env prod')"
        );
    }

    #[test]
    fn unsupported_kinds_are_not_resolved() {
        let f = Fixture::new("");
        f.write("environments/prod.yaml", "");
        f.write(
            "g.rman.yaml",
            "version: 1\nkind: graphql\nname: g\nendpoint: http://x\ndocument: '{ a }'\n",
        );
        assert!(matches!(
            resolve(&f, "g"),
            Err(ResolveError::Unsupported(_))
        ));
    }

    /// Stands in for the transport, which arrives in task group 4: it takes a
    /// `ResolvedRequest`, so it cannot run without one. Task 3.2 stays open
    /// until this test is re-done against the real transport.
    fn send(request: &ResolvedRequest) {
        let addr = request.url.text.trim_start_matches("http://");
        let addr = addr.split('/').next().unwrap();
        drop(TcpStream::connect(addr).unwrap());
    }

    fn pending_connection(listener: &TcpListener) -> bool {
        match listener.accept() {
            Ok(_) => true,
            Err(e) if e.kind() == ErrorKind::WouldBlock => false,
            Err(e) => panic!("{e}"),
        }
    }

    #[test]
    fn no_connection_is_opened_on_unresolved_variables() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let f = Fixture::new("");
        f.write("environments/prod.yaml", "");
        let url = format!("http://127.0.0.1:{port}");
        f.write(
            "bad.rman.yaml",
            &format!("version: 1\nkind: http\nname: bad\nurl: '{url}/{{{{base_url}}}}/users'\n"),
        );
        f.write(
            "good.rman.yaml",
            &format!("version: 1\nkind: http\nname: good\nurl: '{url}/users'\n"),
        );

        let err = resolve(&f, "bad").map(|r| send(&r)).unwrap_err();
        assert_eq!(err.to_string(), "unresolved variable(s): base_url");
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(!pending_connection(&listener), "a connection was opened");

        // Control: a resolvable request does reach the listener.
        send(&resolve(&f, "good").unwrap());
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(pending_connection(&listener));
    }
}

mod placeholders {
    use super::*;
    use rustman_core::vars::{PlaceholderVar, resolve_placeholders};

    #[test]
    fn variables_stay_as_markers_and_secrets_are_never_read() {
        let f = Fixture::new("");
        f.write(
            "environments/prod.yaml",
            "variables:\n  base_url: https://api.example\nsecrets: [api_token]\n",
        );
        f.write(
            "r.rman.yaml",
            "version: 1\nkind: http\nname: r\nurl: '{{base_url}}/users/{{id}}'\nheaders:\n  - name: Authorization\n    value: 'Bearer {{api_token}}'\n  - name: X-Base\n    value: '{{base_url}}'\n",
        );
        let ws = f.load();
        let request = &ws.find_by_name("r")[0];
        let env = ws.active_environment(Some("prod"), None).unwrap();
        let resolver = Resolver::for_request(&ws.settings, request, env, &Store);
        let (resolved, vars) = resolve_placeholders(request, &resolver).unwrap();
        assert_eq!(resolved.url.text, "{{base_url}}/users/{{id}}");
        assert_eq!(
            resolved.headers[0].value.text,
            "Bearer «redacted:api_token»"
        );
        assert!(resolved.secrets.is_empty());
        let var = |name: &str, value: &str| PlaceholderVar {
            name: name.into(),
            value: value.into(),
        };
        assert_eq!(
            vars,
            [var("base_url", "https://api.example"), var("id", "{{id}}")]
        );
    }
}
