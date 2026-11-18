//! Snippets run as printed (runner-parity task 4.4): every language whose
//! interpreter is on this machine runs its snippet against the local echo
//! server, which must receive the same method, path, query, headers and
//! body. A language without its interpreter is reported as skipped, never
//! as passed; `RUSTMAN_REQUIRE_SNIPPET_LANGS` (comma-separated ids) turns a
//! skip of those languages into a failure, so CI proves what it installs.
//!
//! Ignored by default, since it depends on the tools installed:
//! `cargo test -p rustman-cli --test snippet_run -- --ignored --nocapture`.
//! Two languages need packages, so they run only when pointed at them:
//! `node-axios` with `RUSTMAN_SNIPPET_NODE_DIR` (a folder where `npm install
//! axios` ran: the snippet runs there), and `rust-reqwest` with
//! `RUSTMAN_SNIPPET_CARGO=1` (cargo fetches reqwest; builds go to
//! `RUSTMAN_SNIPPET_CARGO_TARGET`, else a temporary folder).
//!
//! Every language but PowerShell and C# (whose .NET client refuses
//! non-ASCII header values, as `export snippet` warns) also sends a
//! non-ASCII header and body, which must arrive as the UTF-8 bytes the
//! engine sends. JSON bodies are compared as JSON: a client may re-encode
//! them (`json=` in Python writes `\u00e9` and `", "`), which is the same
//! document.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{Server, rustman, workspace, write};
use serde_json::Value;

const REQUEST: &str = "version: 1
kind: http
name: create
method: POST
url: '{{base_url}}/echo'
query:
  - name: q
    value: a b&c
headers:
  - name: X-Request-Id
    value: snippet-42
  - name: Accept
    value: application/json
body:
  type: json
  content:
    name: Ada
    tags: [x, 1]
";

/// The non-ASCII header the second fixture adds.
const UNICODE_HEADER: (&str, &str) = ("X-Label", "café ✓");

/// [`REQUEST`] with a non-ASCII header and body value.
fn unicode_request() -> String {
    let (name, value) = UNICODE_HEADER;
    REQUEST
        .replace(
            "headers:\n",
            &format!("headers:\n  - name: {name}\n    value: {value}\n"),
        )
        .replace("name: Ada", "name: Adé")
}

/// Whether `lang`'s client can send a non-ASCII header value.
fn sends_unicode_headers(lang: &str) -> bool {
    !matches!(lang, "powershell" | "csharp")
}

/// How a language's snippet is run: the file it is written to, the folder
/// it runs in (a new temporary one when `None`), files written next to it,
/// and the command, or why it cannot run here.
struct Runner {
    file: &'static str,
    dir: Option<PathBuf>,
    extra: Vec<(&'static str, &'static str)>,
    env: Vec<(&'static str, PathBuf)>,
    command: Result<Vec<String>, String>,
}

impl Runner {
    fn new(file: &'static str, command: Result<Vec<String>, String>) -> Self {
        Self {
            file,
            dir: None,
            extra: Vec::new(),
            env: Vec::new(),
            command,
        }
    }
}

/// A Cargo project around the `rust-reqwest` snippet, outside any workspace.
const REQWEST_MANIFEST: &str = "[package]
name = \"snippet\"
version = \"0.0.0\"
edition = \"2021\"

[dependencies]
reqwest = { version = \"0.12\", default-features = false, features = [\"blocking\"] }

[workspace]
";

fn runner(lang: &str) -> Runner {
    let shell = |tool: &str| {
        let command = available(tool, &["--version"]).map(|()| words(&["bash", "snippet.sh"]));
        Runner::new("snippet.sh", command)
    };
    match lang {
        "curl" | "wget" => shell(lang),
        "httpie" => shell("http"),
        "python-requests" => python("requests"),
        "python-httpx" => python("httpx"),
        "js-fetch" => Runner::new(
            "snippet.mjs",
            available("node", &["--version"]).map(|()| words(&["node", "snippet.mjs"])),
        ),
        "node-axios" => axios(),
        "go" => Runner::new(
            "main.go",
            available("go", &["version"]).map(|()| words(&["go", "run", "main.go"])),
        ),
        "rust-reqwest" => reqwest(),
        "powershell" => Runner::new(
            "snippet.ps1",
            available("pwsh", &["-Version"])
                .map(|()| words(&["pwsh", "-NoProfile", "-File", "snippet.ps1"])),
        ),
        "csharp" => Runner::new(
            "snippet.cs",
            dotnet_file_programs().map(|()| words(&["dotnet", "run", "snippet.cs"])),
        ),
        _ => Runner::new("snippet.txt", Err(format!("no runner for {lang}"))),
    }
}

/// `node-axios` runs in `RUSTMAN_SNIPPET_NODE_DIR`, where axios is installed.
fn axios() -> Runner {
    let Some(dir) = std::env::var_os("RUSTMAN_SNIPPET_NODE_DIR").map(PathBuf::from) else {
        let reason = "RUSTMAN_SNIPPET_NODE_DIR (a folder with axios installed) is not set";
        return Runner::new("snippet-axios.mjs", Err(reason.into()));
    };
    let check = Command::new("node")
        .args(["--input-type=module", "-e", "import 'axios'"])
        .current_dir(&dir)
        .output();
    let command = match check {
        Ok(out) if out.status.success() => Ok(words(&["node", "snippet-axios.mjs"])),
        Ok(_) => Err(format!("axios is not installed in {}", dir.display())),
        Err(_) => Err("node is not installed".to_owned()),
    };
    let mut runner = Runner::new("snippet-axios.mjs", command);
    runner.dir = Some(dir);
    runner
}

/// `rust-reqwest` builds a Cargo project when `RUSTMAN_SNIPPET_CARGO` is set.
fn reqwest() -> Runner {
    if std::env::var_os("RUSTMAN_SNIPPET_CARGO").is_none() {
        let reason = "RUSTMAN_SNIPPET_CARGO is not set (building it fetches reqwest)";
        return Runner::new("src/main.rs", Err(reason.into()));
    }
    let command = available("cargo", &["--version"]).map(|()| words(&["cargo", "run", "--quiet"]));
    let mut runner = Runner::new("src/main.rs", command);
    runner.extra.push(("Cargo.toml", REQWEST_MANIFEST));
    if let Some(target) = std::env::var_os("RUSTMAN_SNIPPET_CARGO_TARGET") {
        runner.env.push(("CARGO_TARGET_DIR", PathBuf::from(target)));
    }
    runner
}

fn python(module: &str) -> Runner {
    let check = format!("import {module}");
    let command = available("python3", &["-c", &check]).map(|()| words(&["python3", "snippet.py"]));
    Runner::new("snippet.py", command)
}

fn words(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_owned()).collect()
}

/// `Ok` when `tool args` runs and succeeds.
fn available(tool: &str, args: &[&str]) -> Result<(), String> {
    match Command::new(tool).args(args).output() {
        Ok(out) if out.status.success() => Ok(()),
        Ok(_) => Err(format!("`{tool} {}` failed", args.join(" "))),
        Err(_) => Err(format!("{tool} is not installed")),
    }
}

/// `dotnet run <file>.cs` needs .NET 10 or later.
fn dotnet_file_programs() -> Result<(), String> {
    let out = Command::new("dotnet")
        .arg("--version")
        .output()
        .map_err(|_| "dotnet is not installed".to_owned())?;
    let version = String::from_utf8_lossy(&out.stdout);
    let major: u32 = version
        .split('.')
        .next()
        .unwrap_or("0")
        .trim()
        .parse()
        .unwrap_or(0);
    if major >= 10 {
        Ok(())
    } else {
        Err(format!(
            "dotnet {} cannot run a single .cs file",
            version.trim()
        ))
    }
}

/// The JSON the echo server answered: the last stdout line that is JSON.
fn echoed(stdout: &str) -> Option<Value> {
    stdout.lines().rev().find_map(|line| {
        serde_json::from_str::<Value>(line.trim())
            .ok()
            .filter(Value::is_object)
    })
}

fn header<'a>(echo: &'a Value, name: &str) -> Option<&'a str> {
    echo["headers"]
        .as_array()?
        .iter()
        .find(|pair| {
            pair[0]
                .as_str()
                .is_some_and(|n| n.eq_ignore_ascii_case(name))
        })
        .and_then(|pair| pair[1].as_str())
}

/// What every language must send, from the `curl` snippet's request;
/// with `unicode`, also the non-ASCII header and body of [`unicode_request`].
fn check(lang: &str, echo: &Value, unicode: bool) -> Result<(), String> {
    let mut problems = Vec::new();
    if echo["method"] != "POST" {
        problems.push(format!("method {}", echo["method"]));
    }
    if echo["path"] != "/echo?q=a+b%26c" {
        problems.push(format!("path {}", echo["path"]));
    }
    let mut expected = vec![
        ("X-Request-Id", "snippet-42"),
        ("Accept", "application/json"),
        ("Content-Type", "application/json"),
    ];
    if unicode {
        expected.push(UNICODE_HEADER);
    }
    for (name, value) in expected {
        if header(echo, name) != Some(value) {
            problems.push(format!("{name}: {:?}", header(echo, name)));
        }
    }
    let body: Option<Value> = echo["body"]
        .as_str()
        .and_then(|b| serde_json::from_str(b).ok());
    let name = if unicode { "Adé" } else { "Ada" };
    if body != Some(serde_json::json!({"name": name, "tags": ["x", 1]})) {
        problems.push(format!("body {}", echo["body"]));
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(format!("{lang}: {}", problems.join("; ")))
    }
}

fn run_snippet(ws: &Path, lang: &str, runner: &Runner, command: &[String]) -> Result<(), String> {
    let unicode = sends_unicode_headers(lang);
    let request = if unicode {
        "api/create-unicode"
    } else {
        "api/create"
    };
    let output = rustman(ws)
        .args(["export", "snippet", "--lang", lang, request, "--env", "dev"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{lang}: export failed");
    let temp = tempfile::tempdir().unwrap();
    let dir = runner.dir.as_deref().unwrap_or(temp.path());
    let file = dir.join(runner.file);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, &output.stdout).unwrap();
    for (name, text) in &runner.extra {
        fs::write(dir.join(name), text).unwrap();
    }
    let out = Command::new(&command[0])
        .args(&command[1..])
        .envs(runner.env.iter().map(|(k, v)| (k, v)))
        .current_dir(dir)
        .output()
        .map_err(|e| format!("{lang}: {e}"))?;
    if runner.dir.is_some() {
        let _ = fs::remove_file(&file);
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    match echoed(&stdout) {
        Some(echo) => check(lang, &echo, unicode),
        None => Err(format!(
            "{lang}: no echo in the output\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        )),
    }
}

#[test]
#[ignore = "runs the interpreters installed on this machine"]
fn every_available_language_sends_the_same_request() {
    let server = Server::http();
    let ws = workspace(&server);
    write(ws.path(), "api/create.rman.yaml", REQUEST);
    write(
        ws.path(),
        "api/create-unicode.rman.yaml",
        &unicode_request(),
    );
    let required = std::env::var("RUSTMAN_REQUIRE_SNIPPET_LANGS").unwrap_or_default();
    let required: Vec<&str> = required
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let languages = rustman(ws.path())
        .args(["export", "snippet", "--list"])
        .output()
        .unwrap();
    let mut failures = Vec::new();
    for lang in String::from_utf8(languages.stdout).unwrap().lines() {
        let runner = runner(lang);
        let result = match &runner.command {
            Ok(command) => run_snippet(ws.path(), lang, &runner, command),
            Err(reason) if required.contains(&lang) => {
                Err(format!("{lang}: required but {reason}"))
            }
            Err(reason) => {
                println!("skipped: {lang} ({reason})");
                continue;
            }
        };
        match result {
            Ok(()) => println!("passed: {lang}"),
            Err(problem) => failures.push(problem),
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
