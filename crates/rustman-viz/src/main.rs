//! `rustman-viz`: the companion executable (add-raven-visualizer tasks
//! 3.1–3.5). `rustman` starts it and writes `rustman-viz/1` lines to its
//! stdin; see `docs/viz.md`.
//!
//! Exit codes: 0 when the windows are closed (or `--protocol-check`
//! succeeded), 2 for bad arguments, 3 when graphics cannot start (after an
//! `error` line `vulkan_unavailable` on stdout).

mod raven_adapter;

use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use rustman_viz::app::App;
use rustman_viz::inbox::Inbox;
use rustman_viz::reader::{read_session, viewer};
use rustman_viz::surface::Surface;
use rustman_viz_protocol::{ErrorMessage, Message, PROTOCOL, VULKAN_UNAVAILABLE, encode_line};

use raven_adapter::{Backend, RavenSurface};

/// `1`: draw with no window system and no GPU (CI).
const HEADLESS_VAR: &str = "RUSTMAN_VIZ_HEADLESS";

/// `1`: behave as if the graphics could not start (task 3.4 test hook).
const FAIL_GRAPHICS_VAR: &str = "RUSTMAN_VIZ_FAIL_GRAPHICS";

/// Exit code when graphics cannot start.
const EXIT_NO_GRAPHICS: u8 = 3;

const USAGE: &str = "\
rustman-viz: the graphical companion of rustman.

rustman starts it (rustman run --viz, rustman explore run --viz,
rustman viz <report>, the TUI's v key); it is not meant to be run alone.

Usage: rustman-viz [--protocol-check | --version | --help]

  --protocol-check   print the protocols this build speaks and exit
  --version          print the version and exit
  --capture <dir>    once stdin ends, save every window as <dir>/<view>.png
                     and exit (screenshots; needs a GPU backend)";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut capture = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--protocol-check" => {
                println!("{PROTOCOL}");
                return ExitCode::SUCCESS;
            }
            "--version" | "-V" => {
                println!("{}", viewer());
                return ExitCode::SUCCESS;
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "--capture" => match iter.next() {
                Some(dir) => capture = Some(dir.clone()),
                None => return usage("--capture needs a folder"),
            },
            other => return usage(&format!("unexpected argument {other}")),
        }
    }
    let backend = if std::env::var(HEADLESS_VAR).is_ok_and(|v| v == "1") {
        Backend::Headless
    } else {
        Backend::Default
    };
    let started = if std::env::var(FAIL_GRAPHICS_VAR).is_ok_and(|v| v == "1") {
        Err("graphics disabled by RUSTMAN_VIZ_FAIL_GRAPHICS".to_owned())
    } else {
        raven_adapter::init(backend)
    };
    if let Err(e) = started {
        eprintln!("rustman-viz: {e}");
        no_graphics(&e);
        return ExitCode::from(EXIT_NO_GRAPHICS);
    }
    run(capture.as_deref(), backend == Backend::Headless);
    raven_adapter::shutdown();
    ExitCode::SUCCESS
}

fn usage(problem: &str) -> ExitCode {
    eprintln!("rustman-viz: {problem}\n\n{USAGE}");
    ExitCode::from(2)
}

/// The `error` line the producer turns into `Graphics unavailable`.
fn no_graphics(detail: &str) {
    let message = Message::Error(ErrorMessage {
        code: VULKAN_UNAVAILABLE.to_owned(),
        message: format!("Graphics unavailable: {detail}"),
    });
    if let Ok(line) = encode_line(&message) {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(line.as_bytes()).and_then(|()| out.flush());
    }
}

/// The event loop: messages from the reader thread go to the app, which
/// draws at most 10 times a second; it ends when the producer is gone and
/// no window is open, or when the user closed every window. Headless, it
/// ends once the producer is gone and the views are drawn.
fn run(capture: Option<&str>, headless: bool) {
    let inbox = Arc::new(Inbox::default());
    let reader_inbox = Arc::clone(&inbox);
    std::thread::spawn(move || {
        let mut out = std::io::stdout();
        read_session(
            std::io::stdin().lock(),
            &mut out,
            &reader_inbox,
            &raven_adapter::wake,
        );
    });
    let mut app = App::new();
    let mut surface = RavenSurface::new();
    let mut gone = false;
    let mut opened = false;
    let mut settled: Option<Instant> = None;
    loop {
        if !raven_adapter::wait_frame(0.1) {
            break;
        }
        for message in inbox.drain() {
            app.handle(&mut surface, message);
        }
        for error in app.errors.drain(..) {
            eprintln!("rustman-viz: {error}");
        }
        if !gone && inbox.is_closed() {
            gone = true;
            app.producer_gone();
        }
        for event in surface.events() {
            app.ui_event(event);
        }
        app.render(&mut surface, Instant::now());
        let open = app.has_windows(&surface);
        opened |= open;
        if (gone && (!open || (headless && !app.needs_render()))) || (opened && !open) {
            break;
        }
        // Charts animate to their values: wait for them to settle.
        if let Some(dir) = capture
            && gone
            && !app.needs_render()
            && settled.get_or_insert_with(Instant::now).elapsed()
                >= std::time::Duration::from_secs(3)
        {
            capture_all(&mut surface, dir);
            break;
        }
    }
    drop(surface);
}

fn capture_all(surface: &mut RavenSurface, dir: &str) {
    for (window, kind) in surface.open_windows() {
        let name = kind.file().trim_end_matches(".xml");
        let path = format!("{dir}/{name}.png");
        match surface.capture(window, &path) {
            Ok(()) => eprintln!("rustman-viz: wrote {path}"),
            Err(e) => eprintln!("rustman-viz: cannot capture {name}: {e}"),
        }
    }
}
