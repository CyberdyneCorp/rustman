//! A stdio MCP server for the client's integration tests: newline-delimited
//! JSON-RPC on stdin/stdout, scripted by flags.
//!
//! - `--version <v>`: the protocol version answered to `initialize`
//!   (default: the one requested).
//! - `--stderr <n>`: write `n` warning lines to stderr at start and before
//!   every answer.
//! - `--exit-after-init <code>`: after `notifications/initialized`, write
//!   60 numbered lines to stderr and exit with `code`.
//! - `--no-prompts`: do not declare the `prompts` capability.
//! - `--marker <path>`: create `path` at start (proves a spawn happened).
//! - `--count <path>`: append `spawn` to `path` at start and `initialize`
//!   on every `initialize` request (counts spawns and sessions).
//! - `--ignore-eof`: keep running when stdin closes.
//! - `--echo-env <name>`: write the variable's value to stderr at start.
//! - `--pages <n>`: serve `tools/list` over `n` pages.
//! - `--endless`: `tools/list` always has a next page.
//! - `--mute`: read every message and answer none (not even `initialize`).
//! - `--grandchild <path>`: start a copy of this program that appends a
//!   line to `path` every 50 ms (for two minutes at most), like a server
//!   behind a launcher.
//! - `--heartbeat <path>`: be that copy.
//!
//! Tools: `env` (`{"name": ...}` → the variable's value), `cwd`, `search`
//! (requires `path`), `fail` (`isError`), `count` (`outputSchema`
//! `{count: integer}`, answers `{"count": <value>}`), `hang` (never
//! answers), `slow` (reports 40/100 progress, never answers), `progress5`
//! (5 progress notifications, then answers), `log` (a debug and an info
//! message, or `{"text": ...}` at info, filtered by `logging/setLevel`,
//! default `info`), `add_tool`
//! (adds `extra` and sends `tools/list_changed`), `sample` (asks the client
//! for `sampling/createMessage`), `ask_email` (asks for an email through
//! `elicitation/create`) and `roots` (asks for `roots/list`); the last three
//! answer with the client's reply as text. The unlisted tool `huge` answers
//! with a 17 MiB text (one line over the client's 16 MiB limit).
//!
//! Resources: `file:///README.md` (text), `file:///logo.png` (blob, second
//! page) and the template `file:///{path}`; subscribing sends
//! `notifications/resources/updated`. Prompts: `review` (requires `code`)
//! and `greet`; completion offers `rust`, `ruby`, `python` by prefix.
//! Received cancellations and root changes are written to stderr.

use std::collections::HashMap;
use std::io::{BufRead, Write};

use serde_json::{Value, json};

#[derive(Default)]
struct Script {
    version: Option<String>,
    stderr: usize,
    exit_after_init: Option<i32>,
    no_prompts: bool,
    ignore_eof: bool,
    pages: usize,
    endless: bool,
    count: Option<String>,
    mute: bool,
}

/// What the server is waiting for and what it remembers.
#[derive(Default)]
struct State {
    level: u8,
    extra_tool: bool,
    next_id: u64,
    /// Requests sent to the client → the `tools/call` id awaiting them.
    pending: HashMap<String, Value>,
}

fn main() {
    let script = parse_args();
    let mut state = State {
        level: level_rank("info"),
        ..State::default()
    };
    warn(script.stderr);
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if script.mute {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        handle(&script, &mut state, &message);
    }
    if script.ignore_eof {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(60));
        }
    }
}

fn parse_args() -> Script {
    let mut script = Script {
        pages: 1,
        ..Script::default()
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut number = || args.next().and_then(|n| n.parse::<i64>().ok());
        match arg.as_str() {
            "--version" => script.version = args.next(),
            "--stderr" => script.stderr = number().unwrap_or(0) as usize,
            "--exit-after-init" => script.exit_after_init = number().map(|n| n as i32),
            "--pages" => script.pages = number().unwrap_or(1) as usize,
            "--endless" => script.endless = true,
            "--no-prompts" => script.no_prompts = true,
            "--ignore-eof" => script.ignore_eof = true,
            "--mute" => script.mute = true,
            "--grandchild" => grandchild(&args.next().unwrap_or_default()),
            "--heartbeat" => heartbeat(&args.next().unwrap_or_default()),
            "--echo-env" => {
                let name = args.next().unwrap_or_default();
                eprintln!("{name}={}", std::env::var(&name).unwrap_or_default());
            }
            "--count" => {
                script.count = args.next();
                append(script.count.as_deref(), "spawn");
            }
            "--marker" => {
                if let Some(path) = args.next() {
                    std::fs::write(path, "spawned").expect("the marker is writable");
                }
            }
            _ => {}
        }
    }
    script
}

/// Start a copy of this program that beats into `path`, detached from our
/// pipes. It is never waited for: it must outlive this process unless the
/// client kills the whole tree.
#[expect(
    clippy::zombie_processes,
    reason = "the grandchild is left running on purpose"
)]
fn grandchild(path: &str) {
    let exe = std::env::current_exe().expect("the fixture knows its path");
    std::process::Command::new(exe)
        .args(["--heartbeat", path])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("the grandchild starts");
}

/// Append a line to `path` every 50 ms, for two minutes at most.
fn heartbeat(path: &str) -> ! {
    for _ in 0..2400 {
        append(Some(path), "beat");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    std::process::exit(0);
}

/// Append `line` to the file at `path`, when there is one.
fn append(path: Option<&str>, line: &str) {
    let Some(path) = path else { return };
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("the count file is writable");
    let _ = writeln!(file, "{line}");
}

fn warn(lines: usize) {
    let mut stderr = std::io::stderr();
    for i in 0..lines {
        let _ = writeln!(stderr, "warning {i}: fixture noise {{\"jsonrpc\":\"2.0\"}}");
    }
}

fn send(message: &Value) {
    let mut stdout = std::io::stdout();
    let _ = writeln!(stdout, "{message}");
    let _ = stdout.flush();
}

fn notify(method: &str, params: Value) {
    send(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
}

fn handle(script: &Script, state: &mut State, message: &Value) {
    let method = message["method"].as_str().unwrap_or_default();
    let Some(id) = message.get("id") else {
        notification(script, method, &message["params"]);
        return;
    };
    if message.get("method").is_none() {
        answered(state, id, message);
        return;
    }
    warn(script.stderr);
    if method == "initialize" {
        append(script.count.as_deref(), "initialize");
    }
    let reply = match answer(script, state, method, id, &message["params"]) {
        Ok(Some(result)) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Ok(None) => return,
        Err((code, text)) => {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": text}})
        }
    };
    send(&reply);
    after(method, &message["params"]);
}

fn notification(script: &Script, method: &str, params: &Value) {
    match method {
        "notifications/initialized" => {
            if let Some(code) = script.exit_after_init {
                exit_noisily(code);
            }
        }
        "notifications/cancelled" => eprintln!(
            "cancelled {} {}",
            params["requestId"],
            params["reason"].as_str().unwrap_or_default()
        ),
        "notifications/roots/list_changed" => eprintln!("roots changed"),
        _ => {}
    }
}

/// Notifications sent once an answer is out.
fn after(method: &str, params: &Value) {
    if method == "resources/subscribe" {
        notify(
            "notifications/resources/updated",
            json!({"uri": params["uri"]}),
        );
    }
}

/// The client answered one of our requests: answer the waiting call.
fn answered(state: &mut State, id: &Value, message: &Value) {
    let key = id.to_string();
    let Some(call) = state.pending.remove(&key) else {
        return;
    };
    let text = match message.get("result") {
        Some(result) => result.to_string(),
        None => format!("error: {}", message["error"]),
    };
    send(&json!({"jsonrpc": "2.0", "id": call, "result": text_result(&text)}));
}

fn exit_noisily(code: i32) -> ! {
    let mut stderr = std::io::stderr();
    for i in 1..=60 {
        let _ = writeln!(stderr, "exit line {i}");
    }
    let _ = stderr.flush();
    std::process::exit(code);
}

type Answer = Result<Option<Value>, (i64, String)>;

fn answer(script: &Script, state: &mut State, method: &str, id: &Value, params: &Value) -> Answer {
    match method {
        "initialize" => Ok(Some(initialize(script, params))),
        "tools/list" => Ok(Some(tools_page(script, state, params))),
        "tools/call" => call(state, id, params),
        "logging/setLevel" => {
            state.level = level_rank(params["level"].as_str().unwrap_or("info"));
            Ok(Some(json!({})))
        }
        _ => static_answer(method, params).map(Some),
    }
}

fn static_answer(method: &str, params: &Value) -> Result<Value, (i64, String)> {
    match method {
        "ping" | "resources/subscribe" | "resources/unsubscribe" => Ok(json!({})),
        "resources/list" => Ok(resources_page(params)),
        "resources/templates/list" => Ok(json!({"resourceTemplates": [
            {"uriTemplate": "file:///{path}", "name": "file"}
        ]})),
        "resources/read" => read(params),
        "prompts/list" => Ok(json!({"prompts": [
            {"name": "review", "arguments": [{"name": "code", "required": true}]},
            {"name": "greet", "arguments": [{"name": "language"}]}
        ]})),
        "prompts/get" => Ok(prompt(params)),
        "completion/complete" => Ok(complete(params)),
        _ => Err((-32601, format!("method not found: {method}"))),
    }
}

fn initialize(script: &Script, params: &Value) -> Value {
    let requested = params["protocolVersion"].as_str().unwrap_or("2025-11-25");
    let version = script.version.as_deref().unwrap_or(requested);
    let mut capabilities = json!({
        "tools": {"listChanged": true},
        "logging": {},
        "resources": {"subscribe": true, "listChanged": true},
        "completions": {}
    });
    if !script.no_prompts {
        capabilities["prompts"] = json!({});
    }
    json!({
        "protocolVersion": version,
        "capabilities": capabilities,
        "serverInfo": {"name": "fixture", "title": "Fixture server", "version": "1.2.3"},
        "instructions": "Use the env tool.",
        "_meta": {"client": params["clientInfo"].clone(), "clientCapabilities": params["capabilities"].clone()}
    })
}

fn tool(name: &str, input: Value) -> Value {
    json!({"name": name, "inputSchema": input})
}

fn base_tools(state: &State) -> Vec<Value> {
    let object = json!({"type": "object"});
    let mut tools: Vec<Value> = [
        "env",
        "cwd",
        "fail",
        "hang",
        "slow",
        "progress5",
        "log",
        "add_tool",
        "sample",
        "ask_email",
        "roots",
    ]
    .iter()
    .map(|name| tool(name, object.clone()))
    .collect();
    tools.push(tool(
        "search",
        json!({"type": "object", "required": ["path"], "properties": {"path": {"type": "string"}}}),
    ));
    let mut count = tool("count", object.clone());
    count["outputSchema"] = json!({
        "type": "object",
        "required": ["count"],
        "properties": {"count": {"type": "integer"}}
    });
    tools.push(count);
    if state.extra_tool {
        tools.push(tool("extra", object));
    }
    tools
}

/// Page `n` of `pages`: the base tools come last.
fn tools_page(script: &Script, state: &State, params: &Value) -> Value {
    let page: usize = params["cursor"]
        .as_str()
        .and_then(|c| c.trim_start_matches('p').parse().ok())
        .unwrap_or(1);
    if script.endless {
        let tools = vec![tool(&format!("page{page}"), json!({"type": "object"}))];
        return json!({"tools": tools, "nextCursor": format!("p{}", page + 1)});
    }
    if page < script.pages {
        let tools = vec![
            tool(&format!("page{page}-a"), json!({"type": "object"})),
            tool(&format!("page{page}-b"), json!({"type": "object"})),
        ];
        return json!({"tools": tools, "nextCursor": format!("p{}", page + 1)});
    }
    json!({"tools": base_tools(state)})
}

fn text_result(text: &str) -> Value {
    json!({"content": [{"type": "text", "text": text}]})
}

fn call(state: &mut State, id: &Value, params: &Value) -> Answer {
    let arguments = &params["arguments"];
    let token = params["_meta"]["progressToken"].clone();
    let name = params["name"].as_str().unwrap_or_default();
    match name {
        "env" => {
            let name = arguments["name"].as_str().unwrap_or_default();
            Ok(Some(text_result(&std::env::var(name).unwrap_or_default())))
        }
        "cwd" => Ok(Some(text_result(
            &std::env::current_dir()
                .map(|d| d.display().to_string())
                .unwrap_or_default(),
        ))),
        "search" => Ok(Some(text_result(&format!("found {}", arguments["path"])))),
        "huge" => Ok(Some(text_result(&"x".repeat(17 << 20)))),
        "fail" => Ok(Some(
            json!({"content": [{"type": "text", "text": "no such file"}], "isError": true}),
        )),
        "count" => Ok(Some(json!({
            "content": [{"type": "text", "text": "counted"}],
            "structuredContent": {"count": arguments["value"]}
        }))),
        "hang" => Ok(None),
        "slow" => {
            progress(&token, 40.0);
            Ok(None)
        }
        "progress5" => {
            for step in 1..=5 {
                progress(&token, f64::from(step) * 20.0);
            }
            Ok(Some(text_result("done")))
        }
        _ => stateful_call(state, id, name, arguments),
    }
}

fn stateful_call(state: &mut State, id: &Value, name: &str, arguments: &Value) -> Answer {
    match name {
        "log" => {
            if let Some(text) = arguments["text"].as_str() {
                log(state, "info", text);
            } else {
                log(state, "debug", "a debug message");
                log(state, "info", "an info message");
            }
            Ok(Some(text_result("logged")))
        }
        "add_tool" => {
            state.extra_tool = true;
            notify("notifications/tools/list_changed", json!({}));
            Ok(Some(text_result("added")))
        }
        "sample" => ask(
            state,
            id,
            "sampling/createMessage",
            json!({
                "messages": [{"role": "user", "content": {"type": "text", "text": "Summarize the file"}}],
                "systemPrompt": "Be brief.",
                "maxTokens": 100
            }),
        ),
        "ask_email" => ask(
            state,
            id,
            "elicitation/create",
            json!({
                "mode": "form",
                "message": "Your email?",
                "requestedSchema": {
                    "type": "object",
                    "required": ["email"],
                    "properties": {"email": {"type": "string", "format": "email"}}
                }
            }),
        ),
        "roots" => ask(state, id, "roots/list", json!({})),
        other => Err((-32602, format!("unknown tool {other:?}"))),
    }
}

/// Send a request to the client; the call is answered with its reply.
fn ask(state: &mut State, call: &Value, method: &str, params: Value) -> Answer {
    state.next_id += 1;
    let id = json!(format!("srv-{}", state.next_id));
    state.pending.insert(id.to_string(), call.clone());
    send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
    Ok(None)
}

fn progress(token: &Value, done: f64) {
    if token.is_null() {
        return;
    }
    notify(
        "notifications/progress",
        json!({"progressToken": token, "progress": done, "total": 100.0}),
    );
}

fn level_rank(level: &str) -> u8 {
    match level {
        "debug" => 0,
        "info" => 1,
        "notice" => 2,
        "warning" => 3,
        _ => 4,
    }
}

fn log(state: &State, level: &str, text: &str) {
    if level_rank(level) >= state.level {
        notify(
            "notifications/message",
            json!({"level": level, "logger": "fixture", "data": text}),
        );
    }
}

fn resources_page(params: &Value) -> Value {
    if params["cursor"].is_null() {
        return json!({
            "resources": [{"uri": "file:///README.md", "name": "README", "mimeType": "text/markdown"}],
            "nextCursor": "r2"
        });
    }
    json!({"resources": [{"uri": "file:///logo.png", "name": "logo", "mimeType": "image/png"}]})
}

fn read(params: &Value) -> Result<Value, (i64, String)> {
    match params["uri"].as_str().unwrap_or_default() {
        "file:///README.md" => Ok(json!({"contents": [
            {"uri": "file:///README.md", "mimeType": "text/markdown", "text": "# Readme"}
        ]})),
        "file:///logo.png" => Ok(json!({"contents": [
            {"uri": "file:///logo.png", "mimeType": "image/png", "blob": "iVBORw0KGgo="}
        ]})),
        other => Err((-32002, format!("resource not found: {other}"))),
    }
}

fn prompt(params: &Value) -> Value {
    let code = params["arguments"]["code"].as_str().unwrap_or_default();
    json!({
        "description": "Review code",
        "messages": [
            {"role": "user", "content": {"type": "text", "text": format!("Review: {code}")}},
            {"role": "assistant", "content": {"type": "text", "text": "Looking."}}
        ]
    })
}

fn complete(params: &Value) -> Value {
    let typed = params["argument"]["value"].as_str().unwrap_or_default();
    let values: Vec<&str> = ["rust", "ruby", "python"]
        .into_iter()
        .filter(|v| v.starts_with(typed))
        .collect();
    json!({"completion": {"values": values, "hasMore": false}})
}
