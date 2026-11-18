//! `rustman export snippet` (runner-parity tasks 4.1-4.3 and 4.5): the
//! language list and errors, redaction on every language, `--show-secrets`,
//! GraphQL as a POST, kinds without export, signature-auth warnings and
//! placeholder mode. Nothing is sent.

mod common;

use common::{rustman, write};
use predicates::prelude::*;

const TOKEN: &str = "tok-5e1d7c93ab42";
const PASSWORD: &str = "pw-9f8e7d6c5b4a";

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
        "environments/prod.yaml",
        "variables:\n  base_url: https://api.example\n  id: '42'\nsecrets: [api_token, password]\n",
    );
    write(root, ".gitignore", ".rustman/\n");
    write(
        root,
        "api/users/create.rman.yaml",
        "version: 1\nkind: http\nname: create-user\nmethod: POST\nurl: '{{base_url}}/users'\nauth:\n  type: bearer\n  token: '{{api_token}}'\nbody:\n  type: json\n  content:\n    name: Ada\n",
    );
    write(
        root,
        "api/users/get.rman.yaml",
        "version: 1\nkind: http\nname: get-user\nurl: '{{base_url}}/users/{{id}}'\n",
    );
    write(
        root,
        "api/private.rman.yaml",
        "version: 1\nkind: http\nname: private\nurl: '{{base_url}}/private'\nauth:\n  type: aws_sigv4\n  region: eu-west-1\n  service: s3\n  access_key: AKIDEXAMPLE\n  secret_key: '{{password}}'\n",
    );
    for (name, value) in [("api_token", TOKEN), ("password", PASSWORD)] {
        rustman(root)
            .args(["secret", "set", name, "--env", "prod"])
            .write_stdin(value)
            .assert()
            .success();
    }
    dir
}

#[test]
fn list_prints_the_eleven_languages() {
    let dir = tempfile::tempdir().unwrap();
    rustman(dir.path())
        .args(["export", "snippet", "--list"])
        .assert()
        .success()
        .stdout(
            "curl\nhttpie\nwget\npython-requests\npython-httpx\njs-fetch\nnode-axios\ngo\n\
             rust-reqwest\npowershell\ncsharp\n",
        );
}

#[test]
fn an_unknown_language_exits_2_and_lists_the_supported_ones() {
    let ws = workspace();
    rustman(ws.path())
        .args(["export", "snippet", "--lang", "cobol", "api/users/get"])
        .assert()
        .code(2)
        .stdout("")
        .stderr(predicate::str::contains(
            "error: snippet: unknown language 'cobol' (supported: curl, httpie, wget, python-requests",
        ));
}

#[test]
fn python_requests_posts_the_resolved_json_request() {
    let ws = workspace();
    rustman(ws.path())
        .args(["export", "snippet", "--lang", "python-requests", "api/users/create", "--env", "prod"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "response = requests.post(\n    \"https://api.example/users\",\n    json={\"name\": \"Ada\"},\n    headers={",
        ));
}

/// The canary: no language prints a secret value unless asked to.
#[test]
fn no_language_prints_a_secret_without_show_secrets() {
    let ws = workspace();
    for lang in [
        "curl",
        "httpie",
        "wget",
        "python-requests",
        "python-httpx",
        "js-fetch",
        "node-axios",
        "go",
        "rust-reqwest",
        "powershell",
        "csharp",
    ] {
        for request in ["api/users/create", "api/private"] {
            let output = rustman(ws.path())
                .args([
                    "export", "snippet", "--lang", lang, request, "--env", "prod",
                ])
                .output()
                .unwrap();
            assert!(output.status.success(), "{lang} {request}");
            let all = [output.stdout, output.stderr].concat();
            let text = String::from_utf8(all).unwrap();
            for secret in [TOKEN, PASSWORD] {
                assert!(!text.contains(secret), "{lang} {request}:\n{text}");
            }
        }
    }
}

#[test]
fn js_fetch_redacts_the_bearer_token_unless_show_secrets() {
    let ws = workspace();
    let args = [
        "export",
        "snippet",
        "--lang",
        "js-fetch",
        "api/users/create",
        "--env",
        "prod",
    ];
    rustman(ws.path())
        .args(args)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "\"Authorization\": \"Bearer «redacted:api_token»\"",
        ));
    rustman(ws.path())
        .args(args)
        .arg("--show-secrets")
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "\"Authorization\": \"Bearer {TOKEN}\""
        )));
}

#[test]
fn graphql_exports_as_an_http_post() {
    let ws = workspace();
    write(
        ws.path(),
        "gql/user.rman.yaml",
        "version: 1\nkind: graphql\nname: user\nendpoint: '{{base_url}}/graphql'\ndocument: 'query User { user(id: \"{{literal}}\") { name } }'\nvariables:\n  id: '{{id}}'\n",
    );
    rustman(ws.path())
        .args(["export", "snippet", "--lang", "js-fetch", "gql/user", "--env", "prod"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "fetch(\"https://api.example/graphql\", {\n  method: \"POST\",",
        ))
        .stdout(predicate::str::contains(
            r#"body: JSON.stringify({"query": "query User { user(id: \"{{literal}}\") { name } }", "variables": {"id": "42"}}),"#,
        ));
}

#[test]
fn other_kinds_exit_2() {
    let ws = workspace();
    write(
        ws.path(),
        "tools/m.rman.yaml",
        "version: 1\nkind: mcp\nname: m\n",
    );
    rustman(ws.path())
        .args([
            "export", "snippet", "--lang", "go", "tools/m", "--env", "prod",
        ])
        .assert()
        .code(2)
        .stderr("error: snippet: kind 'mcp' has no snippet export\n");
    write(
        ws.path(),
        "events/k.rman.yaml",
        "version: 1\nkind: kafka\nname: k\n",
    );
    rustman(ws.path())
        .args([
            "export", "snippet", "--lang", "go", "events/k", "--env", "prod",
        ])
        .assert()
        .code(2)
        .stdout("");
}

/// The engine sends a non-ASCII header value as UTF-8 bytes; snippets of
/// clients that would write it as Latin-1 (or refuse it) pass bytes.
#[test]
fn non_ascii_header_values_go_out_as_utf8() {
    let ws = workspace();
    write(
        ws.path(),
        "api/uni.rman.yaml",
        "version: 1\nkind: http\nname: uni\nurl: '{{base_url}}/u'\nheaders:\n  - name: X-Uni\n    value: café\n",
    );
    let export = |lang: &str| {
        rustman(ws.path())
            .args([
                "export", "snippet", "--lang", lang, "api/uni", "--env", "prod",
            ])
            .output()
            .unwrap()
    };
    for (lang, expected) in [
        ("python-requests", r#""X-Uni": "café".encode(),"#),
        ("python-httpx", r#""X-Uni": "café".encode(),"#),
        (
            "js-fetch",
            r#""X-Uni": String.fromCharCode(...new TextEncoder().encode("café")),"#,
        ),
        (
            "node-axios",
            r#""X-Uni": String.fromCharCode(...new TextEncoder().encode("café")),"#,
        ),
        ("rust-reqwest", r#".header("X-Uni", "café".as_bytes())"#),
        ("go", r#"req.Header.Add("X-Uni", "café")"#),
    ] {
        let out = export(lang);
        let code = String::from_utf8(out.stdout).unwrap();
        assert!(code.contains(expected), "{lang}:\n{code}");
        assert!(out.stderr.is_empty(), "{lang}");
    }
    let csharp = export("csharp");
    assert_eq!(
        String::from_utf8(csharp.stderr).unwrap(),
        "warning: snippet: csharp may refuse the non-ASCII value of header X-Uni\n"
    );
}

#[test]
fn sigv4_in_go_is_a_comment_and_a_warning() {
    let ws = workspace();
    rustman(ws.path())
        .args([
            "export",
            "snippet",
            "--lang",
            "go",
            "api/private",
            "--env",
            "prod",
        ])
        .assert()
        .success()
        .stdout(predicate::str::starts_with(
            "// aws_sigv4 auth: sign with the AWS SDK signer\n",
        ))
        .stderr("warning: snippet: aws_sigv4 auth cannot be precomputed for go\n");
}

#[test]
fn placeholders_keep_variables_and_conflict_with_show_secrets() {
    let ws = workspace();
    rustman(ws.path())
        .args([
            "export",
            "snippet",
            "--lang",
            "python",
            "--placeholders",
            "api/users/get",
            "--env",
            "prod",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "BASE_URL = \"https://api.example\"\nID = \"42\"\n",
        ))
        .stdout(predicate::str::contains(
            "    BASE_URL + \"/users/\" + ID,\n",
        ));
    let wget = [
        "export",
        "snippet",
        "--lang",
        "wget",
        "--placeholders",
        "api/users/create",
    ];
    rustman(ws.path())
        .args(wget)
        .args(["--env", "prod"])
        .assert()
        .success()
        .stdout(predicate::str::contains("'{{base_url}}/users'"))
        .stdout(predicate::str::contains("Bearer «redacted:api_token»"));
    rustman(ws.path())
        .args(wget)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "'Authorization: Bearer {{api_token}}'",
        ));
    rustman(ws.path())
        .args([
            "export",
            "snippet",
            "--lang",
            "go",
            "--placeholders",
            "--show-secrets",
            "api/users/get",
        ])
        .assert()
        .code(2);
}

/// Every `$ rustman export snippet ...` example in docs/snippets.md with
/// the workspace the page describes, and the output it shows.
fn documented_examples() -> Vec<(Vec<String>, String)> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/snippets.md");
    let doc = std::fs::read_to_string(path).unwrap().replace("\r\n", "\n");
    let mut examples = Vec::new();
    let mut current: Option<(Vec<String>, String)> = None;
    let mut in_console = false;
    for line in doc.lines() {
        if line == "```console" {
            in_console = true;
            continue;
        }
        if !in_console {
            continue;
        }
        if line == "```" || line.starts_with("$ ") {
            examples.extend(current.take());
            in_console = line != "```";
        }
        if let Some(command) = line.strip_prefix("$ rustman ") {
            let args = command.split(' ').map(str::to_owned).collect();
            current = Some((args, String::new()));
        } else if let Some((_, output)) = current.as_mut() {
            output.push_str(line);
            output.push('\n');
        }
    }
    examples
}

#[test]
fn the_documented_examples_print_what_the_page_shows() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "rustman.toml",
        "[workspace]\nname = \"t\"\nversion = 1\n",
    );
    write(
        root,
        "environments/prod.yaml",
        "variables:\n  base_url: https://api.example.com\n  id: '42'\nsecrets: [api_token]\n",
    );
    write(
        root,
        "users/create.rman.yaml",
        "version: 1\nkind: http\nname: create-user\nmethod: POST\nurl: '{{base_url}}/users'\nauth:\n  type: bearer\n  token: '{{api_token}}'\nbody:\n  type: json\n  content:\n    name: Ada\n    admin: false\n",
    );
    write(
        root,
        "users/get.rman.yaml",
        "version: 1\nkind: http\nname: get-user\nurl: '{{base_url}}/users/{{id}}'\n",
    );
    rustman(root)
        .args(["secret", "set", "api_token", "--env", "prod"])
        .write_stdin(TOKEN)
        .assert()
        .success();
    let examples = documented_examples();
    let checked: Vec<_> = examples
        .iter()
        .filter(|(args, _)| args.iter().any(|a| a.starts_with("users/")))
        .collect();
    assert_eq!(checked.len(), 3, "{examples:#?}");
    for (args, shown) in checked {
        let output = rustman(root).args(args).output().unwrap();
        assert!(output.status.success(), "{args:?}");
        assert_eq!(
            &String::from_utf8(output.stdout).unwrap(),
            shown,
            "{args:?}"
        );
    }
}
