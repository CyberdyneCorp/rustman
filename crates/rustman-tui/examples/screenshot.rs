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
//! shows the JSON file as that tab's exchange, a `200 OK` with fixed
//! timings and history, so a frame with a response needs no network; the
//! request's assertions are evaluated against it.
//! `--keys` presses space-separated chords last (`"Ctrl+P s w"`), e.g. to
//! show the palette.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rustman_core::Redactor;
use rustman_core::assertions::{self, AssertionResult, Context, ResponseView};
use rustman_core::model::{HttpRequest, Request};
use rustman_core::run::{
    ExchangeRecord, HeaderRecord, RequestRecord, RequestStatus, ResponseRecord, SentRequest,
    TimingsRecord,
};
use rustman_core::vars::Resolver;
use rustman_tui::app::{Action, State, update};
use rustman_tui::config::{Overrides, TuiConfig, UiOptions};
use rustman_tui::effects::execute;
use rustman_tui::keymap::chord::{Chord, Code, parse_chord};
use rustman_tui::launch::discover;
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

fn key_event(chord: &Chord) -> Result<KeyEvent, String> {
    let code = match chord.code {
        Code::Char(c) => KeyCode::Char(c),
        Code::F(n) => KeyCode::F(n),
        Code::Enter => KeyCode::Enter,
        Code::Esc => KeyCode::Esc,
        Code::Tab => KeyCode::Tab,
        Code::BackTab => KeyCode::BackTab,
        Code::Backspace => KeyCode::Backspace,
        Code::Up => KeyCode::Up,
        Code::Down => KeyCode::Down,
        Code::Left => KeyCode::Left,
        Code::Right => KeyCode::Right,
        other => return Err(format!("unsupported key {other:?}")),
    };
    let mut modifiers = KeyModifiers::NONE;
    modifiers.set(KeyModifiers::CONTROL, chord.ctrl);
    modifiers.set(KeyModifiers::ALT, chord.alt);
    Ok(KeyEvent::new(code, modifiers))
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

/// The `http` request open in the active tab, if any.
fn open_request(state: &State) -> Option<HttpRequest> {
    match state.tabs.active()?.editor.request().ok()? {
        Request::Http(request) => Some(*request),
        Request::Graphql(_) | Request::Opaque(_) => None,
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
    let start = match std::env::current_dir() {
        Ok(cwd) => cwd.join(&options.path),
        Err(err) => {
            eprintln!("error: no current directory: {err}");
            return ExitCode::from(2);
        }
    };
    let (state, mut effects) = State::new(discover(&start));
    let ui = UiOptions::resolve(&TuiConfig::default(), options.overrides, options.mode);
    let mut state = state.with_ui(ui);
    let (width, height) = (options.width, options.height);
    update(&mut state, Action::Resize { width, height });
    while let Some(effect) = effects.pop() {
        effects.extend(update(&mut state, execute(effect)));
    }
    if let Some(request) = &options.open {
        rustman_tui::workbench::open_path(&mut state, request);
    }
    if let Some(file) = &options.response {
        let body = match std::fs::read(file) {
            Ok(body) => body,
            Err(err) => {
                eprintln!("error: cannot read {}: {err}", file.display());
                return ExitCode::from(2);
            }
        };
        let path = options.open.clone().unwrap_or_default();
        let request = open_request(&state);
        match state.tabs.active_mut() {
            Some(tab) => tab.show(exchange(&path, request.as_ref(), body)),
            None => {
                eprintln!("error: --response needs a tab: pass --open");
                return ExitCode::from(2);
            }
        }
    }
    for chord in &options.keys {
        let key = match key_event(chord) {
            Ok(key) => key,
            Err(message) => {
                eprintln!("error: {message}");
                return ExitCode::from(2);
            }
        };
        effects.extend(update(&mut state, Action::Key(key)));
        while let Some(effect) = effects.pop() {
            effects.extend(update(&mut state, execute(effect)));
        }
    }
    let buffer = render(&state, options.width, options.height);
    let frame = if options.ansi {
        buffer_ansi(&buffer)
    } else {
        buffer_text(&buffer)
    };
    print!("{frame}");
    ExitCode::SUCCESS
}
