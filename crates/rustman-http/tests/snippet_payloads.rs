//! Snippets stay inert whatever the request holds: values that a shell, an
//! HTTPie request item, a curl `-F` value or a programming language would
//! read as code or as "read this file" are written as plain text.
//!
//! Every language is checked statically: shell snippets are split into
//! words the way a POSIX shell does (and fail on any unquoted expansion),
//! HTTPie items are parsed with HTTPie's own separator rules, and code
//! snippets must carry each value as a string literal. Where an interpreter
//! is installed (bash with curl, wget or `http`; python3 with requests or
//! httpx; node) the snippet also runs against a loopback server, which must
//! receive the values byte for byte, nothing from the decoy file, and no
//! command may have run. Missing interpreters are reported as skipped.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

use rustman_core::Workspace;
use rustman_core::vars::{ResolvedRequest, Resolver, SecretLookup, resolve_request};
use rustman_http::{Lang, Snippet, SnippetOptions, snippet};

struct NoSecrets;

impl SecretLookup for NoSecrets {
    fn secret(&self, _env: &str, _name: &str) -> Result<Option<String>, String> {
        Ok(None)
    }
}

/// The decoy a file-reading item would leak, next to the snippet.
const DECOY: &str = "decoy.txt";
const DECOY_CONTENT: &str = "DECOY-FILE-CONTENT-7f3a";
/// The file a command substitution would create.
const PWNED: &str = "PWNED";

/// Header values: each would read `decoy.txt`, run a command or expand
/// history if written unescaped somewhere.
const HEADER_VALUES: [(&str, &str); 9] = [
    ("X-Embed", "=@decoy.txt"),
    ("X-Json-Embed", ":=@decoy.txt"),
    ("X-Header-Embed", "@decoy.txt"),
    ("X-Curl-Read", "<decoy.txt"),
    ("X-Query", "==x"),
    ("X-Subst", "$(touch PWNED)"),
    ("X-Backtick", "`touch PWNED`"),
    ("X-Quotes", "!! ' \" \\x ;"),
    ("X-Empty", ""),
];

/// Query, form and multipart values: the header values plus a line break.
const BODY_VALUES: [&str; 9] = [
    "=@decoy.txt",
    ":=@decoy.txt",
    "@decoy.txt",
    "<decoy.txt",
    "$(touch PWNED)",
    "`touch PWNED`",
    "!! ' \" \\x",
    "line 1\nline 2",
    "",
];

/// `text` as a YAML single-quoted scalar (a line break is written as a
/// blank line inside it).
fn yaml(text: &str) -> String {
    format!(
        "'{}'",
        text.replace('\'', "''").replace('\n', "\n\n          ")
    )
}

fn headers_yaml() -> String {
    HEADER_VALUES
        .iter()
        .map(|(n, v)| format!("  - name: {n}\n    value: {}\n", yaml(v)))
        .collect()
}

fn fields_yaml(indent: &str) -> String {
    BODY_VALUES
        .iter()
        .enumerate()
        .map(|(i, v)| format!("{indent}- name: f{i}\n{indent}  value: {}\n", yaml(v)))
        .collect()
}

/// A form request with every payload in a header, a query value, a form
/// field and the URL path.
fn form_request() -> String {
    format!(
        "version: 1\nkind: http\nname: form\nmethod: POST\n\
         url: '{{{{base_url}}}}/p/$(touch${{IFS}}PWNED)/!!/=@decoy.txt'\n\
         query:\n  - name: q\n    value: {}\n  - name: '=@decoy.txt'\n    value: ':=@decoy.txt'\n\
         headers:\n{}body:\n  type: form\n  fields:\n{}",
        yaml("=@decoy.txt $(touch PWNED) `touch PWNED` !! ' \" \\x"),
        headers_yaml(),
        fields_yaml("    "),
    )
}

/// A text part whose name and type would make curl `-F` read the decoy
/// (`name=@file`, `;headers=@file`): curl leaves it out, with warnings.
const TYPED_PART: &str = "    - name: 'typed=@decoy.txt'
      value: typed
      content_type: 'text/plain;headers=@decoy.txt'
";

/// A multipart request with every payload in a text part, one real file
/// part and [`TYPED_PART`].
fn multipart_request() -> String {
    format!(
        "version: 1\nkind: http\nname: multipart\nmethod: POST\nurl: '{{{{base_url}}}}/m'\n\
         headers:\n{}body:\n  type: multipart\n  parts:\n{}    - name: upload\n      file: upload.txt\n      content_type: text/plain\n\
         {TYPED_PART}",
        headers_yaml(),
        fields_yaml("    "),
    )
}

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new(base_url: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let write = |rel: &str, text: &str| fs::write(dir.path().join(rel), text).unwrap();
        write("rustman.toml", "[workspace]\nname = \"t\"\nversion = 1\n");
        fs::create_dir_all(dir.path().join("environments")).unwrap();
        write(
            "environments/local.yaml",
            &format!("variables:\n  base_url: {base_url}\n"),
        );
        write("form.rman.yaml", &form_request());
        write("multipart.rman.yaml", &multipart_request());
        write("upload.txt", "UPLOADED");
        write(DECOY, DECOY_CONTENT);
        Self { dir }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn resolved(&self, name: &str) -> ResolvedRequest {
        let ws = Workspace::load(self.path()).unwrap_or_else(|e| panic!("{e}"));
        let request = ws.find_request(name, &ws.root).unwrap();
        let env = ws.active_environment(Some("local"), None).unwrap();
        let resolver = Resolver::for_request(&ws.settings, &request, env, &NoSecrets);
        resolve_request(&request, &resolver).unwrap()
    }

    fn snippet(&self, name: &str, lang: Lang) -> Snippet {
        let options = SnippetOptions {
            show_secrets: false,
            placeholders: None,
            root: Some(self.path()),
        };
        snippet(&self.resolved(name), lang, &options)
    }
}

// ---------------------------------------------------------------- static

/// The words of POSIX shell `script`: single quotes, backslash escapes and
/// line continuations, nothing else. Panics on anything a shell would
/// expand or treat as an operator outside quotes.
fn shell_words(script: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut chars = script.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                let w = word.get_or_insert_with(String::new);
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => w.push(c),
                        None => panic!("unterminated quote in:\n{script}"),
                    }
                }
            }
            '\\' => match chars.next() {
                Some('\n') => {}
                Some(c) => word.get_or_insert_with(String::new).push(c),
                None => panic!("trailing backslash in:\n{script}"),
            },
            ' ' | '\t' | '\n' => words.extend(word.take()),
            '#' if word.is_none() => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            '$' | '`' | '!' | '(' | ')' | ';' | '|' | '&' | '<' | '>' | '"' | '*' | '?' | '['
            | '~' => panic!("unquoted {c:?} in:\n{script}"),
            c => word.get_or_insert_with(String::new).push(c),
        }
    }
    words.extend(word);
    words
}

/// A parsed HTTPie request item.
#[derive(Debug, PartialEq)]
struct Item {
    key: String,
    sep: String,
    value: String,
}

/// HTTPie 3.2's `KeyValueArgType(*SEPARATOR_GROUP_ALL_ITEMS)`: a backslash
/// before a separator character escapes it, and the first, longest
/// separator in an unescaped token splits the item.
fn httpie_item(arg: &str) -> Item {
    const SEPARATORS: [&str; 10] = [":", ";", ":@", "==", "==@", "=", ":=", "@", "=@", ":=@"];
    let special = |c: char| matches!(c, ':' | ';' | '=' | '@');
    // (text, escaped)
    let mut tokens: Vec<(String, bool)> = vec![(String::new(), false)];
    let mut chars = arg.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some(n) if special(n) => {
                    tokens.push((n.to_string(), true));
                    tokens.push((String::new(), false));
                }
                n => {
                    let last = &mut tokens.last_mut().unwrap().0;
                    last.push('\\');
                    last.extend(n);
                }
            }
        } else {
            tokens.last_mut().unwrap().0.push(c);
        }
    }
    for (i, (token, escaped)) in tokens.iter().enumerate() {
        if *escaped {
            continue;
        }
        let found = SEPARATORS
            .iter()
            .filter_map(|sep| token.find(sep).map(|pos| (pos, *sep)))
            .min_by_key(|(pos, sep)| (*pos, usize::MAX - sep.len()));
        if let Some((pos, sep)) = found {
            let join = |ts: &[(String, bool)]| ts.iter().map(|t| t.0.as_str()).collect::<String>();
            return Item {
                key: join(&tokens[..i]) + &token[..pos],
                sep: sep.to_owned(),
                value: token[pos + sep.len()..].to_owned() + &join(&tokens[i + 1..]),
            };
        }
    }
    panic!("not a request item: {arg:?}");
}

/// The words of a shell snippet after the command's options and the
/// method and URL: its request items or arguments.
fn shell_snippet(fixture: &Fixture, name: &str, lang: Lang) -> Vec<String> {
    let code = fixture.snippet(name, lang).code;
    for line in code.lines().filter(|l| !l.starts_with('#')) {
        assert!(!line.contains("--post-file"), "{lang}: {line}");
    }
    shell_words(&code)
}

#[test]
fn httpie_items_carry_every_value_as_text() {
    let f = Fixture::new("http://127.0.0.1:9");
    for name in ["form", "multipart"] {
        let words = shell_snippet(&f, name, Lang::Httpie);
        let items: Vec<Item> = words
            .iter()
            .skip_while(|w| !w.starts_with("http://"))
            .skip(1)
            .map(|w| httpie_item(w))
            .collect();
        for (header, value) in HEADER_VALUES {
            let sep = if value.is_empty() { ";" } else { ":" };
            let expected = Item {
                key: header.into(),
                sep: sep.into(),
                value: value.into(),
            };
            assert!(
                items.contains(&expected),
                "{name}: {expected:?} in {items:?}"
            );
        }
        if name == "multipart" {
            for (i, value) in BODY_VALUES.iter().enumerate() {
                let expected = Item {
                    key: format!("f{i}"),
                    sep: "=".into(),
                    value: (*value).into(),
                };
                assert!(items.contains(&expected), "{expected:?} in {items:?}");
            }
            let file = items.iter().filter(|i| i.sep == "@").collect::<Vec<_>>();
            assert_eq!(file.len(), 1, "{items:?}");
            assert_eq!(file[0].value, "upload.txt;type=text/plain");
        } else {
            let raw = words.iter().find(|w| w.starts_with("--raw=")).unwrap();
            assert!(raw.contains("%24%28touch+PWNED%29"), "{raw}");
        }
        assert!(
            items
                .iter()
                .all(|i| [":", ";", "=", "@"].contains(&i.sep.as_str())),
            "{items:?}"
        );
    }
}

#[test]
fn curl_reads_no_file_but_the_upload() {
    let f = Fixture::new("http://127.0.0.1:9");
    for name in ["form", "multipart"] {
        let words = shell_snippet(&f, name, Lang::Curl);
        for pair in words.windows(2) {
            let (option, value) = (pair[0].as_str(), pair[1].as_str());
            match option {
                "-H" => assert!(!value.starts_with('@'), "{value}"),
                "-F" => assert_eq!(value, "upload=@upload.txt;type=text/plain"),
                "--data-binary" | "-d" | "--data" | "-T" => panic!("{option} {value}"),
                _ => {}
            }
        }
        if name == "multipart" {
            assert_eq!(
                f.snippet(name, Lang::Curl).warnings,
                [
                    "the content type 'text/plain;headers=@decoy.txt' of part 'typed=@decoy.txt' cannot be written for curl -F; it is left out",
                    "part 'typed=@decoy.txt' has '=' in its name, which curl cannot send; it is left out",
                ]
            );
        }
        for (header, value) in HEADER_VALUES {
            let expected = if value.is_empty() {
                format!("{header};")
            } else {
                format!("{header}: {value}")
            };
            assert!(words.contains(&expected), "{name}: {expected} in {words:?}");
        }
    }
}

#[test]
fn wget_reads_no_file() {
    let f = Fixture::new("http://127.0.0.1:9");
    let words = shell_snippet(&f, "form", Lang::Wget);
    for (header, value) in HEADER_VALUES {
        assert!(words.contains(&format!("{header}: {value}")), "{words:?}");
    }
}

/// `text` as a string literal of `lang`, as the snippet must write it.
fn literal(lang: Lang, text: &str) -> String {
    match lang {
        Lang::RustReqwest => format!("{text:?}"),
        Lang::Powershell => format!("'{}'", text.replace('\'', "''")),
        _ => serde_json::to_string(text).unwrap(),
    }
}

#[test]
fn code_snippets_carry_every_value_as_a_string_literal() {
    let f = Fixture::new("http://127.0.0.1:9");
    let code_langs = Lang::ALL
        .into_iter()
        .filter(|l| !matches!(l, Lang::Curl | Lang::Httpie | Lang::Wget));
    for lang in code_langs {
        let code = f.snippet("multipart", lang).code;
        for (_, value) in HEADER_VALUES.iter().filter(|(_, v)| !v.is_empty()) {
            let literal = literal(lang, value);
            assert!(code.contains(&literal), "{lang}: {literal} in:\n{code}");
        }
        for value in BODY_VALUES.iter().filter(|v| !v.is_empty()) {
            let literal = literal(lang, value);
            assert!(code.contains(&literal), "{lang}: {literal} in:\n{code}");
        }
        assert!(!code.contains(DECOY_CONTENT), "{lang}");
    }
}

/// Python snippets parse, call only the HTTP client and `print`, and open
/// only the upload; every payload is a string constant.
#[test]
fn python_snippets_call_nothing_but_the_client() {
    if !tool_runs("python3", &["--version"]) {
        eprintln!("skipped: python3 is not installed");
        return;
    }
    let f = Fixture::new("http://127.0.0.1:9");
    let check = r#"
import ast, sys
tree = ast.parse(open(sys.argv[1]).read())
allowed = {"requests.post", "requests.request", "httpx.request", "print", "open", "open.read", "HTTPDigestAuth", "httpx.DigestAuth", "encode"}
for node in ast.walk(tree):
    if isinstance(node, ast.Call):
        name = ast.unparse(node.func)
        base = name.split("(")[0]
        if name.endswith(".encode"):
            name = "encode"
        assert name in allowed or base in allowed, name
        if name == "open":
            assert ast.literal_eval(node.args[0]) == "upload.txt", ast.unparse(node)
consts = {n.value for n in ast.walk(tree) if isinstance(n, ast.Constant)}
for value in sys.argv[2:]:
    assert value in consts, value
"#;
    for lang in [Lang::PythonRequests, Lang::PythonHttpx] {
        for name in ["form", "multipart"] {
            let path = f.path().join("check.py");
            fs::write(&path, f.snippet(name, lang).code).unwrap();
            let mut args = vec![
                "-c".to_owned(),
                check.to_owned(),
                path.display().to_string(),
            ];
            args.extend(HEADER_VALUES.iter().map(|(_, v)| (*v).to_owned()));
            if name == "multipart" {
                args.extend(BODY_VALUES.iter().map(|v| (*v).to_owned()));
            }
            let out = Command::new("python3").args(&args).output().unwrap();
            assert!(
                out.status.success(),
                "{lang} {name}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}

// --------------------------------------------------------------- execute

fn tool_runs(tool: &str, args: &[&str]) -> bool {
    Command::new(tool)
        .args(args)
        .output()
        .is_ok_and(|o| o.status.success())
}

/// One raw HTTP/1.1 request, read up to its `Content-Length` (or the end of
/// a chunked body), answered with `200`.
fn read_request(stream: &mut std::net::TcpStream) -> Vec<u8> {
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    let mut data = Vec::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = stream.read(&mut buf).unwrap_or(0);
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
        let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") else {
            continue;
        };
        let head = String::from_utf8_lossy(&data[..end]).to_ascii_lowercase();
        let length = head
            .lines()
            .find_map(|l| l.strip_prefix("content-length:"))
            .and_then(|v| v.trim().parse::<usize>().ok());
        let done = match length {
            Some(length) => data.len() >= end + 4 + length,
            None if head.contains("transfer-encoding: chunked") => data.ends_with(b"0\r\n\r\n"),
            None => true,
        };
        if done {
            break;
        }
    }
    let _ =
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
    data
}

/// A loopback server that records every request it receives.
struct Server {
    url: String,
    requests: mpsc::Receiver<Vec<u8>>,
}

impl Server {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (tx, requests) = mpsc::channel();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                if tx.send(read_request(&mut stream)).is_err() {
                    break;
                }
            }
        });
        Self { url, requests }
    }
}

/// How a language's snippet runs, when its interpreter is installed.
fn runner(lang: Lang) -> Option<(&'static str, Vec<&'static str>)> {
    let available = match lang {
        Lang::Curl => tool_runs("curl", &["--version"]),
        Lang::Wget => tool_runs("wget", &["--version"]),
        Lang::Httpie => tool_runs("http", &["--version"]),
        Lang::PythonRequests => tool_runs("python3", &["-c", "import requests"]),
        Lang::PythonHttpx => tool_runs("python3", &["-c", "import httpx"]),
        Lang::JsFetch => tool_runs("node", &["--version"]),
        _ => false,
    };
    let shell = tool_runs("bash", &["--version"]);
    match lang {
        Lang::Curl | Lang::Wget | Lang::Httpie if available && shell => {
            Some(("snippet.sh", vec!["bash", "snippet.sh"]))
        }
        Lang::PythonRequests | Lang::PythonHttpx if available => {
            Some(("snippet.py", vec!["python3", "snippet.py"]))
        }
        Lang::JsFetch if available => Some(("snippet.mjs", vec!["node", "snippet.mjs"])),
        _ => None,
    }
}

/// Whether `haystack` holds `needle`.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    needle.is_empty() || haystack.windows(needle.len()).any(|w| w == needle)
}

/// The value of header `name` in raw request `request`.
fn header_value(request: &[u8], name: &str) -> Option<String> {
    let text = String::from_utf8_lossy(request);
    let head = text.split("\r\n\r\n").next()?;
    head.lines().skip(1).find_map(|line| {
        let (n, v) = line.split_once(':')?;
        n.eq_ignore_ascii_case(name).then(|| v.trim().to_owned())
    })
}

#[test]
fn snippets_send_the_payloads_as_text_and_run_nothing() {
    let server = Server::start();
    let f = Fixture::new(&server.url);
    for lang in [
        Lang::Curl,
        Lang::Httpie,
        Lang::Wget,
        Lang::PythonRequests,
        Lang::PythonHttpx,
        Lang::JsFetch,
    ] {
        let Some((file, command)) = runner(lang) else {
            eprintln!("skipped: {lang} (interpreter not installed)");
            continue;
        };
        let names: &[&str] = if lang == Lang::Wget {
            &["form"]
        } else {
            &["form", "multipart"]
        };
        for name in names {
            let code = f.snippet(name, lang).code;
            fs::write(f.path().join(file), &code).unwrap();
            let out = Command::new(command[0])
                .args(&command[1..])
                .current_dir(f.path())
                .env_remove("HTTP_PROXY")
                .env_remove("http_proxy")
                .env_remove("ALL_PROXY")
                .env("NO_PROXY", "*")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{lang} {name} failed: {}\n{code}",
                String::from_utf8_lossy(&out.stderr)
            );
            let request = server
                .requests
                .recv_timeout(Duration::from_secs(20))
                .unwrap_or_else(|_| panic!("{lang} {name}: no request"));
            assert!(
                !f.path().join(PWNED).exists(),
                "{lang} {name} ran a command"
            );
            assert!(
                !contains(&request, DECOY_CONTENT.as_bytes()),
                "{lang} {name} sent the decoy file:\n{}",
                String::from_utf8_lossy(&request)
            );
            for (header, value) in HEADER_VALUES {
                assert_eq!(
                    header_value(&request, header).as_deref(),
                    Some(value),
                    "{lang} {name}: {header} in\n{}",
                    String::from_utf8_lossy(&request)
                );
            }
            if *name == "multipart" {
                for value in BODY_VALUES {
                    let part = format!("\r\n\r\n{}\r\n--", value.replace('\n', "\r\n"));
                    let lf_part = format!("\r\n\r\n{value}\r\n--");
                    assert!(
                        contains(&request, part.as_bytes())
                            || contains(&request, lf_part.as_bytes()),
                        "{lang}: part {value:?} in\n{}",
                        String::from_utf8_lossy(&request)
                    );
                }
                assert!(contains(&request, b"\r\n\r\nUPLOADED\r\n--"), "{lang}");
            } else {
                let expected = form_body();
                assert!(
                    request.ends_with(expected.as_bytes()),
                    "{lang}: form body {expected} in\n{}",
                    String::from_utf8_lossy(&request)
                );
            }
        }
    }
}

/// The form body every snippet sends: the fields, form-encoded.
fn form_body() -> String {
    let mut out = form_urlencoded::Serializer::new(String::new());
    for (i, value) in BODY_VALUES.iter().enumerate() {
        out.append_pair(&format!("f{i}"), value);
    }
    out.finish()
}
