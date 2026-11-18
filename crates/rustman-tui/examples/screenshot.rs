//! Render the TUI off-screen, for README screenshots and quick visual checks.
//!
//! ```text
//! cargo run -p rustman-tui --example screenshot -- \
//!     [--ansi] [--size WxH] [--theme NAME] [--ascii] [--no-color] [PATH]
//! ```
//!
//! Opens PATH (default: the current directory) the way `rustman tui` does,
//! runs the start-up effects synchronously, and prints one frame (120x36 by
//! default) as plain text, or with ANSI colours with `--ansi`. `tui.toml` is
//! not read, so the output only depends on the flags.

use std::path::PathBuf;
use std::process::ExitCode;

use rustman_tui::app::{Action, State, update};
use rustman_tui::config::{Overrides, TuiConfig, UiOptions};
use rustman_tui::effects::execute;
use rustman_tui::launch::discover;
use rustman_tui::snapshot::{buffer_ansi, buffer_text, render};
use rustman_tui::theme::ColorMode;

struct Options {
    ansi: bool,
    overrides: Overrides,
    mode: ColorMode,
    width: u16,
    height: u16,
    path: PathBuf,
}

fn parse(args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut options = Options {
        ansi: false,
        overrides: Overrides::default(),
        mode: ColorMode::TrueColor,
        width: 120,
        height: 36,
        path: PathBuf::from("."),
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
    let buffer = render(&state, options.width, options.height);
    let frame = if options.ansi {
        buffer_ansi(&buffer)
    } else {
        buffer_text(&buffer)
    };
    print!("{frame}");
    ExitCode::SUCCESS
}
