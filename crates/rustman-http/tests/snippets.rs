//! Snippet export (runner-parity tasks 4.1-4.3 and 4.5): every language for
//! a JSON request and a multipart request, redaction, signature auth and
//! placeholder mode. Requests are loaded from a real workspace, so they go
//! through the same resolution as `export curl`.

use std::fs;
use std::path::Path;

use rustman_core::Workspace;
use rustman_core::vars::{
    PlaceholderVar, ResolvedRequest, Resolver, SecretLookup, resolve_placeholders, resolve_request,
};
use rustman_http::{Lang, Snippet, SnippetOptions, snippet};

/// The `prod` environment's secret store: `api_token` and `password`.
struct Store;

impl SecretLookup for Store {
    fn secret(&self, _env: &str, name: &str) -> Result<Option<String>, String> {
        let value = match name {
            "api_token" => "tok-3c8f1e2d9a",
            "password" => "pw-77d1b0c4e5",
            _ => return Ok(None),
        };
        Ok(Some(value.to_owned()))
    }
}

const SECRET_VALUES: [&str; 2] = ["tok-3c8f1e2d9a", "pw-77d1b0c4e5"];

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let fixture = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        fixture.write("rustman.toml", "[workspace]\nname = \"t\"\nversion = 1\n");
        fixture.write(
            "environments/prod.yaml",
            "variables:\n  base_url: https://api.example\n  id: '42'\n  user: alice\nsecrets: [api_token, password]\n",
        );
        fixture.write("files/avatar.png", "PNG");
        fixture
    }

    fn write(&self, rel: &str, text: &str) {
        let path = self.dir.path().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn workspace(&self) -> Workspace {
        Workspace::load(self.dir.path()).unwrap_or_else(|e| panic!("{e}"))
    }

    /// The request `name`, resolved for `prod`, as a `lang` snippet.
    fn snippet(&self, name: &str, lang: Lang, show_secrets: bool) -> Snippet {
        let ws = self.workspace();
        let request = ws.find_request(name, &ws.root).unwrap();
        let env = ws.active_environment(Some("prod"), None).unwrap();
        let resolver = Resolver::for_request(&ws.settings, &request, env, &Store);
        let resolved: ResolvedRequest = resolve_request(&request, &resolver).unwrap();
        let options = SnippetOptions {
            show_secrets,
            placeholders: None,
            root: Some(&ws.root),
        };
        snippet(&resolved, lang, &options)
    }

    /// The request `name` in placeholder mode.
    fn placeholders(&self, name: &str, lang: Lang) -> Snippet {
        let ws = self.workspace();
        let request = ws.find_request(name, &ws.root).unwrap();
        let env = ws.active_environment(Some("prod"), None).unwrap();
        let resolver = Resolver::for_request(&ws.settings, &request, env, &Store);
        let (resolved, vars): (ResolvedRequest, Vec<PlaceholderVar>) =
            resolve_placeholders(&request, &resolver).unwrap();
        let options = SnippetOptions {
            show_secrets: false,
            placeholders: Some(&vars),
            root: Some(&ws.root),
        };
        snippet(&resolved, lang, &options)
    }
}

const JSON_REQUEST: &str = "version: 1
kind: http
name: create-user
method: POST
url: '{{base_url}}/users'
query:
  - name: notify
    value: 'yes'
headers:
  - name: Accept
    value: application/json
  - name: X-Request-Id
    value: abc 123
auth:
  type: bearer
  token: '{{api_token}}'
body:
  type: json
  content:
    name: Ada Lovelace
    admin: true
    teams: [core, 7]
    manager: null
";

const MULTIPART_REQUEST: &str = "version: 1
kind: http
name: upload
method: PUT
url: '{{base_url}}/users/{{id}}/avatar'
headers:
  - name: X-Trace
    value: t-1
body:
  type: multipart
  parts:
    - name: caption
      value: It's me
    - name: meta
      value: '{\"v\":1}'
      content_type: application/json
    - name: file
      file: ../../files/avatar.png
      content_type: image/png
";

fn fixture() -> Fixture {
    let f = Fixture::new();
    f.write("api/users/create.rman.yaml", JSON_REQUEST);
    f.write("api/users/upload.rman.yaml", MULTIPART_REQUEST);
    f
}

fn assert_no_secret(code: &str) {
    for value in SECRET_VALUES {
        assert!(!code.contains(value), "secret value in:\n{code}");
    }
}

#[test]
fn every_language_renders_the_json_request() {
    let f = fixture();
    for lang in Lang::ALL {
        let snippet = f.snippet("api/users/create", lang, false);
        assert_no_secret(&snippet.code);
        assert!(
            snippet.warnings.is_empty(),
            "{lang}: {:?}",
            snippet.warnings
        );
        insta::assert_snapshot!(format!("json_{lang}"), snippet.code);
    }
}

#[test]
fn every_language_renders_the_multipart_request() {
    let f = fixture();
    for lang in Lang::ALL {
        let snippet = f.snippet("api/users/upload", lang, false);
        insta::assert_snapshot!(format!("multipart_{lang}"), snippet.code);
    }
}

#[test]
fn multipart_limitations_are_warned_about() {
    let f = fixture();
    let warnings = |lang| f.snippet("api/users/upload", lang, false).warnings;
    assert_eq!(
        warnings(Lang::Httpie),
        ["snippet: httpie sends text parts without their content type"]
    );
    assert_eq!(
        warnings(Lang::Wget),
        ["snippet: wget cannot send multipart bodies"]
    );
    assert!(warnings(Lang::Curl).is_empty());
}

#[test]
fn every_language_renders_placeholders() {
    let f = fixture();
    for lang in Lang::ALL {
        let snippet = f.placeholders("api/users/create", lang);
        assert_no_secret(&snippet.code);
        insta::assert_snapshot!(format!("placeholders_{lang}"), snippet.code);
    }
}

#[test]
fn a_bearer_secret_is_redacted_in_js_fetch() {
    let code = fixture()
        .snippet("api/users/create", Lang::JsFetch, false)
        .code;
    assert!(
        code.contains(r#""Authorization": "Bearer «redacted:api_token»""#),
        "{code}"
    );
}

#[test]
fn show_secrets_includes_them() {
    let code = fixture()
        .snippet("api/users/create", Lang::JsFetch, true)
        .code;
    assert!(
        code.contains(r#""Authorization": "Bearer tok-3c8f1e2d9a""#),
        "{code}"
    );
}

#[test]
fn python_requests_posts_json_with_headers() {
    let code = fixture()
        .snippet("api/users/create", Lang::PythonRequests, false)
        .code;
    assert!(
        code.contains("requests.post(\n    \"https://api.example/users?notify=yes\",\n    json={\"name\": \"Ada Lovelace\", \"admin\": True, \"teams\": [\"core\", 7], \"manager\": None},\n    headers={"),
        "{code}"
    );
}

#[test]
fn python_placeholders_declare_constants_for_the_url() {
    let f = Fixture::new();
    f.write(
        "users/get.rman.yaml",
        "version: 1\nkind: http\nname: get-user\nurl: '{{base_url}}/users/{{id}}'\n",
    );
    let code = f
        .placeholders("users/get", Lang::parse("python").unwrap())
        .code;
    assert!(
        code.contains("BASE_URL = \"https://api.example\"\nID = \"42\"\n"),
        "{code}"
    );
    assert!(
        code.contains("    BASE_URL + \"/users/\" + ID,\n"),
        "{code}"
    );
    let curl = f.placeholders("users/get", Lang::Curl).code;
    assert_eq!(curl, "curl '{{base_url}}/users/{{id}}' -L");
}

/// The request files a signature-auth snippet is made from.
const DIGEST: &str = "auth:\n  type: digest\n  username: '{{user}}'\n  password: '{{password}}'\n";
const SIGV4: &str = "auth:\n  type: aws_sigv4\n  region: eu-west-1\n  service: execute-api\n  access_key: AKIDEXAMPLE\n  secret_key: '{{password}}'\n";
const JWT: &str = "auth:\n  type: jwt\n  algorithm: HS256\n  secret: '{{password}}'\n  claims:\n    sub: '{{user}}'\n";

fn signed_fixture() -> Fixture {
    let f = Fixture::new();
    for (name, auth) in [("digest", DIGEST), ("sigv4", SIGV4), ("jwt", JWT)] {
        let text = format!(
            "version: 1\nkind: http\nname: {name}\nurl: '{{{{base_url}}}}/private'\n{auth}"
        );
        f.write(&format!("auth/{name}.rman.yaml"), &text);
    }
    f
}

#[test]
fn signature_auth_uses_native_helpers_or_a_comment_and_a_warning() {
    let f = signed_fixture();
    for (name, kind) in [("digest", "digest"), ("sigv4", "aws_sigv4"), ("jwt", "jwt")] {
        for lang in Lang::ALL {
            let snippet = f.snippet(&format!("auth/{name}"), lang, false);
            assert_no_secret(&snippet.code);
            let warning = format!("snippet: {kind} auth cannot be precomputed for {lang}");
            let warned = snippet.warnings.contains(&warning);
            let commented = snippet.code.contains(&format!("{kind} auth: "));
            assert_eq!(warned, commented, "{name} {lang}: {:?}", snippet.warnings);
            insta::assert_snapshot!(format!("{name}_{lang}"), snippet.code);
        }
    }
}

#[test]
fn digest_in_curl_uses_curl_s_option() {
    let code = signed_fixture()
        .snippet("auth/digest", Lang::Curl, false)
        .code;
    assert!(
        code.contains("--digest -u 'alice:«redacted:password»'"),
        "{code}"
    );
}

#[test]
fn sigv4_in_go_is_a_comment() {
    let snippet = signed_fixture().snippet("auth/sigv4", Lang::Go, false);
    assert!(
        snippet
            .code
            .contains("// aws_sigv4 auth: sign with the AWS SDK signer"),
        "{}",
        snippet.code
    );
    assert_eq!(
        snippet.warnings,
        ["snippet: aws_sigv4 auth cannot be precomputed for go"]
    );
}

#[test]
fn native_helpers_are_the_documented_ones() {
    let f = signed_fixture();
    let cases = [
        (
            "digest",
            Lang::PythonRequests,
            "HTTPDigestAuth(\"alice\", \"«redacted:password»\")",
        ),
        (
            "digest",
            Lang::Httpie,
            "--auth-type digest '--auth=alice:«redacted:password»'",
        ),
        (
            "sigv4",
            Lang::Curl,
            "--aws-sigv4 'aws:amz:eu-west-1:execute-api' -u 'AKIDEXAMPLE:«redacted:password»'",
        ),
    ];
    for (name, lang, expected) in cases {
        let snippet = f.snippet(&format!("auth/{name}"), lang, false);
        assert!(snippet.code.contains(expected), "{lang}:\n{}", snippet.code);
        assert!(
            snippet.warnings.is_empty(),
            "{lang}: {:?}",
            snippet.warnings
        );
    }
}

#[test]
fn files_are_named_relative_to_the_workspace() {
    let code = fixture()
        .snippet("api/users/upload", Lang::Curl, false)
        .code;
    assert!(
        code.contains("file=@files/avatar.png;type=image/png"),
        "{code}"
    );
    assert!(Path::new("files/avatar.png").is_relative());
}

/// A method is any RFC 9110 token, which allows shell metacharacters such as
/// backtick, `$`, `|` and `&`: shell snippets must quote it.
const ODD_METHOD_REQUEST: &str = "version: 1
kind: http
name: odd
method: \"X`id`|id&&$x\"
url: https://api.example/odd
";

#[test]
fn shell_snippets_quote_a_method_with_metacharacters() {
    let f = Fixture::new();
    f.write("odd.rman.yaml", ODD_METHOD_REQUEST);
    let quoted = "'X`id`|id&&$x'";
    for lang in [Lang::Curl, Lang::Wget, Lang::Powershell] {
        let code = f.snippet("odd", lang, false).code;
        assert!(
            code.contains(quoted),
            "{lang}: method not quoted in:\n{code}"
        );
        let unquoted = code.replace(quoted, "");
        assert!(!unquoted.contains("`id`"), "{lang}: raw method in:\n{code}");
    }
}

/// httpie reads a method that is not made of letters as the URL, and the URL
/// as a request item: the snippet is a comment instead, with a warning.
#[test]
fn httpie_refuses_a_method_that_is_not_letters() {
    let f = Fixture::new();
    f.write("odd.rman.yaml", ODD_METHOD_REQUEST);
    let snippet = f.snippet("odd", Lang::Httpie, false);
    assert!(
        snippet.code.lines().all(|line| line.starts_with('#')),
        "{}",
        snippet.code
    );
    assert_eq!(
        snippet.warnings,
        ["snippet: httpie cannot send method X`id`|id&&$x"]
    );
}
