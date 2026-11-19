//! Render the TUI off-screen, for README screenshots and quick visual checks.
//!
//! ```text
//! cargo run -p rustman-tui --example screenshot -- \
//!     [--ansi] [--size WxH] [--theme NAME] [--ascii] [--no-color] \
//!     [--open REQUEST] [--response BODY.json] [--keys KEYS] [PATH]
//! ```
//!
//! Opens PATH (default: the current directory) the way `rustman tui` does,
//! runs the start-up effects synchronously, and prints one frame (120x36 by
//! default) as plain text, or with ANSI colours with `--ansi`. `tui.toml` is
//! not read, so the output only depends on the flags. `--open` opens a
//! request file (relative to the workspace root) in a tab. `--response`
//! shows the JSON file as that tab's exchange, so a frame with a response
//! needs no network: for an HTTP request it is the body of a `200 OK` with
//! fixed timings and history, and the request's assertions are evaluated
//! against it (a GraphQL request reads it as a GraphQL result); for a
//! gRPC request it is the call's record (the `grpc` member of a
//! `rustman.report/v1` request).
//! `--keys` presses space-separated chords last (`"Ctrl+P s w"`), e.g. to
//! show the palette.
//!
//! `examples/screenshots/` holds the sample workspace and responses of the
//! README frames, and `render.sh` there renders them all.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rustman_core::Redactor;
use rustman_core::assertions::{self, AssertionResult, Context, ResponseView};
use rustman_core::model::{GraphqlRequest, HttpRequest, Request};
use rustman_core::run::{
    ExchangeRecord, GraphqlRecord, HeaderRecord, RequestRecord, RequestStatus, ResponseRecord,
    SentRequest, TimingsRecord,
};
use rustman_core::vars::Resolver;
use rustman_tui::app::{Action, Effect, State, update};
use rustman_tui::config::{Overrides, TuiConfig, UiOptions};
use rustman_tui::effects::execute;
use rustman_tui::keymap::chord::{Chord, Code, parse_chord};
use rustman_tui::launch::discover;
use rustman_tui::response::graphql::GraphqlView;
use rustman_tui::response::grpc::GrpcView;
use rustman_tui::response::{RawBody, Received, Source};
use rustman_tui::snapshot::{buffer_ansi, buffer_text, render};
use rustman_tui::theme::ColorMode;

/// The sample response's headers and timings (ms).
const HEADERS: [(&str, &str); 2] = [
    ("content-type", "application/json"),
    ("x-request-id", "req-7f3a"),
];
const TTFB_MS: f64 = 71.0;
const TOTAL_MS: f64 = 84.0;

struct Options {
    ansi: bool,
    overrides: Overrides,
    mode: ColorMode,
    width: u16,
    height: u16,
    path: PathBuf,
    keys: Vec<Chord>,
    open: Option<PathBuf>,
    response: Option<PathBuf>,
}

fn parse(args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut options = Options {
        ansi: false,
        overrides: Overrides::default(),
        mode: ColorMode::TrueColor,
        width: 120,
        height: 36,
        path: PathBuf::from("."),
        keys: Vec::new(),
        open: None,
        response: None,
    };
    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--ansi" => options.ansi = true,
            "--ascii" => options.overrides.ascii = true,
            "--no-color" => options.mode = ColorMode::NoColor,
            "--theme" => {
                let name = args.next().ok_or("--theme needs a name")?;
                options.overrides.theme = Some(name.parse()?);
            }
            "--size" => {
                let size = args.next().ok_or("--size needs WxH")?;
                (options.width, options.height) = parse_size(&size)?;
            }
            "--open" => options.open = Some(args.next().ok_or("--open needs a request")?.into()),
            "--response" => {
                options.response = Some(args.next().ok_or("--response needs a file")?.into());
            }
            "--keys" => {
                let keys = args.next().ok_or("--keys needs chords")?;
                options.keys = keys.split(' ').map(parse_chord).collect::<Result<_, _>>()?;
            }
            _ => options.path = PathBuf::from(arg),
        }
    }
    Ok(options)
}

fn parse_size(size: &str) -> Result<(u16, u16), String> {
    let (w, h) = size.split_once('x').ok_or("size must be WxH")?;
    let number = |s: &str| {
        s.parse::<u16>()
            .map_err(|e| format!("bad size {size}: {e}"))
    };
    Ok((number(w)?, number(h)?))
}

fn key_event(chord: &Chord) -> KeyEvent {
    let code = match chord.code {
        Code::Char(c) => KeyCode::Char(c),
        Code::F(n) => KeyCode::F(n),
        Code::Enter => KeyCode::Enter,
        Code::Esc => KeyCode::Esc,
        Code::Tab => KeyCode::Tab,
        Code::BackTab => KeyCode::BackTab,
        Code::Backspace => KeyCode::Backspace,
        Code::Delete => KeyCode::Delete,
        Code::Insert => KeyCode::Insert,
        Code::Home => KeyCode::Home,
        Code::End => KeyCode::End,
        Code::PageUp => KeyCode::PageUp,
        Code::PageDown => KeyCode::PageDown,
        Code::Up => KeyCode::Up,
        Code::Down => KeyCode::Down,
        Code::Left => KeyCode::Left,
        Code::Right => KeyCode::Right,
    };
    let mut modifiers = KeyModifiers::NONE;
    modifiers.set(KeyModifiers::CONTROL, chord.ctrl);
    modifiers.set(KeyModifiers::ALT, chord.alt);
    modifiers.set(KeyModifiers::SHIFT, chord.shift);
    KeyEvent::new(code, modifiers)
}

/// The assertions of `request` evaluated against the sample `200 OK` of
/// `body`, as the engine would after sending it.
fn assess(request: Option<&HttpRequest>, body: &[u8]) -> Vec<AssertionResult> {
    let Some(list) = request.and_then(|r| r.assertions.as_deref()) else {
        return Vec::new();
    };
    let response = ResponseView {
        status: 200,
        headers: HEADERS.map(|(n, v)| (n.to_owned(), v.to_owned())).to_vec(),
        body: Cow::Borrowed(body),
        binary: false,
        ttfb: Duration::from_secs_f64(TTFB_MS / 1000.0),
        total: Duration::from_secs_f64(TOTAL_MS / 1000.0),
        grpc: None,
    };
    let ctx = Context {
        resolver: &Resolver::new(),
        request: None,
        redactor: &Redactor::new(),
    };
    assertions::evaluate(list, &response, &ctx)
}

/// A `200 OK` exchange of `body` (JSON) for `request`, with fixed timings
/// and a made-up history, so frames are the same on every run.
fn exchange(path: &Path, request: Option<&HttpRequest>, body: Vec<u8>) -> Received {
    let file = path.to_string_lossy().replace('\\', "/");
    let id = file.trim_end_matches(".rman.yaml").to_owned();
    let mut record = RequestRecord::new(id.clone(), id, file);
    record.assertions = assess(request, &body);
    record.status = if record.assertions.iter().all(AssertionResult::passed) {
        RequestStatus::Passed
    } else {
        RequestStatus::Failed
    };
    let size = body.len() as u64;
    let header = |(name, value): (&str, &str)| HeaderRecord {
        name: name.into(),
        value: value.into(),
    };
    record.exchange = Some(ExchangeRecord {
        request: SentRequest {
            method: request.map_or("GET", HttpRequest::effective_method).into(),
            url: String::new(),
            headers: Vec::new(),
            body: None,
        },
        response: ResponseRecord {
            status: 200,
            http_version: "HTTP/1.1".into(),
            headers: HEADERS.map(header).to_vec(),
            body: String::from_utf8(body.clone()).ok(),
            body_bytes: size,
            body_truncated: false,
            binary: false,
        },
        timings: TimingsRecord {
            auth_ms: None,
            dns_ms: Some(4.2),
            connect_ms: Some(11.8),
            tls_ms: Some(23.5),
            ttfb_ms: TTFB_MS,
            download_ms: TOTAL_MS - TTFB_MS,
            total_ms: TOTAL_MS,
        },
        redirects: Vec::new(),
        insecure: false,
    });
    let raw = RawBody {
        source: Source::Memory(body.into()),
        size,
        binary: false,
        content_type: Some("application/json".into()),
    };
    let mut received = Received::with_body(record, raw);
    received.history = [
        92.0, 88.0, 101.0, 79.0, 84.0, 140.0, 95.0, 83.0, 77.0, 90.0, 86.0, 84.0,
    ]
    .to_vec();
    received
}

/// The gRPC `call` (a request's `grpc` member in a report) for the
/// request at `path`, ended in [`TOTAL_MS`].
fn grpc_exchange(path: &Path, call: serde_json::Value) -> Received {
    let file = path.to_string_lossy().replace('\\', "/");
    let id = file.trim_end_matches(".rman.yaml").to_owned();
    let mut record = RequestRecord::new(id.clone(), id, file);
    record.status = RequestStatus::Passed;
    record.duration_ms = TOTAL_MS;
    let view = GrpcView::new(&call);
    record.grpc = Some(call);
    let mut received = Received::without_body(record);
    received.grpc = Some(Box::new(view));
    received
}

/// A `200 OK` of the GraphQL result `body` for `request`, with the
/// record of a query sent over HTTP.
fn graphql_exchange(path: &Path, request: &GraphqlRequest, body: &[u8]) -> Received {
    let result = serde_json::from_slice::<serde_json::Value>(body).unwrap_or_default();
    let errors = result["errors"].as_array().is_some_and(|e| !e.is_empty());
    let outcome = match (errors, result["data"].is_null()) {
        (false, _) => "ok",
        (true, false) => "partial",
        (true, true) => "errors",
    };
    let record = GraphqlRecord {
        operation_name: request.operation_name.clone(),
        operation_type: "query".into(),
        transport: "http".into(),
        protocol: None,
        variables: request
            .variables
            .clone()
            .map(Into::into)
            .unwrap_or_default(),
        result: Some(outcome.into()),
        apq: None,
        subscription: None,
    };
    let mut received = exchange(path, None, body.to_vec());
    received.graphql = Some(Box::new(GraphqlView::new(&record, Some(body))));
    received.record.graphql = Some(record);
    received
}

/// The exchange `--response` shows for the request open in the active
/// tab: a gRPC call's record, or else an HTTP response body (a GraphQL
/// result for a GraphQL request).
fn sample(state: &State, path: &Path, file: &[u8]) -> Result<Received, String> {
    match state.tabs.active().map(|tab| tab.editor.request()) {
        Some(Ok(Request::Grpc(_))) => serde_json::from_slice(file)
            .map(|call| grpc_exchange(path, call))
            .map_err(|e| format!("bad gRPC record: {e}")),
        Some(Ok(Request::Http(request))) => Ok(exchange(path, Some(&request), file.to_vec())),
        Some(Ok(Request::Graphql(request))) => Ok(graphql_exchange(path, &request, file)),
        Some(_) => Ok(exchange(path, None, file.to_vec())),
        None => Err("--response needs a tab: pass --open".into()),
    }
}

/// Run `effects`, and the effects of their outcomes, to completion.
fn settle(state: &mut State, mut effects: Vec<Effect>) {
    while let Some(effect) = effects.pop() {
        effects.extend(update(state, execute(effect)));
    }
}

fn main() -> ExitCode {
    let options = match parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("error: {message}");
            return ExitCode::from(2);
        }
    };
    match frame(&options) {
        Ok(frame) => {
            print!("{frame}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::from(2)
        }
    }
}

/// The frame `options` ask for.
fn frame(options: &Options) -> Result<String, String> {
    let cwd = std::env::current_dir().map_err(|e| format!("no current directory: {e}"))?;
    let (state, effects) = State::new(discover(&cwd.join(&options.path)));
    let ui = UiOptions::resolve(&TuiConfig::default(), options.overrides, options.mode);
    let mut state = state.with_ui(ui);
    let resize = || Action::Resize {
        width: options.width,
        height: options.height,
    };
    update(&mut state, resize());
    settle(&mut state, effects);
    if let Some(request) = &options.open {
        rustman_tui::workbench::open_path(&mut state, request);
        // Any update starts the tab's background loads (schemas, catalogs).
        let effects = update(&mut state, resize());
        settle(&mut state, effects);
    }
    if let Some(file) = &options.response {
        let body =
            std::fs::read(file).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
        let path = options.open.clone().unwrap_or_default();
        let received = sample(&state, &path, &body)?;
        if let Some(tab) = state.tabs.active_mut() {
            tab.show(received);
        }
    }
    for chord in &options.keys {
        let effects = update(&mut state, Action::Key(key_event(chord)));
        settle(&mut state, effects);
    }
    let buffer = render(&state, options.width, options.height);
    Ok(if options.ansi {
        buffer_ansi(&buffer)
    } else {
        buffer_text(&buffer)
    })
}
