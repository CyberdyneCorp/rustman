//! Terminal behavior (cli-runner spec "Terminal-aware output" and
//! "Diagnostics stream separation"): when to use color, and the logs that
//! `-v` and `RUST_LOG` turn on, written to stderr and redacted.

use std::io::{self, IsTerminal, Write};

use clap::ValueEnum;
use rustman_core::redact::redact_logs;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;

/// `--color <WHEN>`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum ColorWhen {
    /// Color a terminal, unless NO_COLOR is set or TERM=dumb
    #[default]
    Auto,
    /// Color even when piped
    Always,
    /// Never color
    Never,
}

/// Whether to color a stream: `--color always|never` decides; `auto` colors
/// only a terminal, and not with a non-empty `NO_COLOR` or `TERM=dumb`.
pub fn use_color(when: ColorWhen, is_terminal: bool, env: &dyn Fn(&str) -> Option<String>) -> bool {
    match when {
        ColorWhen::Always => true,
        ColorWhen::Never => false,
        ColorWhen::Auto => {
            let no_color = env("NO_COLOR").is_some_and(|v| !v.is_empty());
            let dumb = env("TERM").is_some_and(|t| t == "dumb");
            is_terminal && !no_color && !dumb
        }
    }
}

fn process_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// Decide once whether stdout gets color. Reports go through
/// `anstream::stdout()`, which then keeps ANSI styles or strips them.
pub fn init_color(when: ColorWhen) {
    let choice = if use_color(when, io::stdout().is_terminal(), &process_env) {
        anstream::ColorChoice::Always
    } else {
        anstream::ColorChoice::Never
    };
    choice.write_global();
}

/// The log filter for `-v` counts when `RUST_LOG` is unset: Rustman's own
/// events at info (`-v`) or debug (`-vv`), and everything at trace (`-vvv`).
fn default_filter(verbose: u8) -> &'static str {
    match verbose {
        0 => "warn",
        1 => "warn,rustman=info,rustman_core=info,rustman_http=info",
        2 => "warn,rustman=debug,rustman_core=debug,rustman_http=debug",
        _ => "trace",
    }
}

/// Send logs to stderr when `-v` is given or `RUST_LOG` is set. Without
/// either, nothing is installed and stderr carries only warnings and errors.
pub fn init_logs(verbose: u8, color: ColorWhen) {
    let rust_log = std::env::var("RUST_LOG").ok().filter(|v| !v.is_empty());
    if verbose == 0 && rust_log.is_none() {
        return;
    }
    let filter = match rust_log {
        Some(directives) => EnvFilter::new(directives),
        None => EnvFilter::new(default_filter(verbose)),
    };
    let ansi = use_color(color, io::stderr().is_terminal(), &process_env);
    // A second subscriber (in tests that call this twice) is not an error.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(RedactingStderr)
        .with_ansi(ansi)
        .try_init();
}

/// Stderr through [`redact_logs`]: a safety net under log events, which
/// carry only redacted text, for anything else that reaches the logs.
struct RedactingStderr;

impl<'a> MakeWriter<'a> for RedactingStderr {
    type Writer = RedactingStderr;

    fn make_writer(&'a self) -> Self::Writer {
        RedactingStderr
    }
}

impl Write for RedactingStderr {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // The formatter writes each event in one call, so a secret is never
        // split between two writes.
        let line = redact_logs(&String::from_utf8_lossy(buf));
        io::stderr().write_all(line.as_bytes())?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        io::stderr().flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            vars.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    #[test]
    fn auto_colors_only_a_terminal() {
        let term = [("TERM", "xterm-256color")];
        assert!(use_color(ColorWhen::Auto, true, &env(&term)));
        assert!(!use_color(ColorWhen::Auto, false, &env(&term)));
    }

    #[test]
    fn no_color_and_dumb_terminals_turn_color_off() {
        let no_color = [("TERM", "xterm"), ("NO_COLOR", "1")];
        assert!(!use_color(ColorWhen::Auto, true, &env(&no_color)));
        let empty_no_color = [("TERM", "xterm"), ("NO_COLOR", "")];
        assert!(use_color(ColorWhen::Auto, true, &env(&empty_no_color)));
        let dumb = [("TERM", "dumb")];
        assert!(!use_color(ColorWhen::Auto, true, &env(&dumb)));
    }

    #[test]
    fn the_color_flag_wins() {
        let no_color = [("NO_COLOR", "1"), ("TERM", "dumb")];
        assert!(use_color(ColorWhen::Always, false, &env(&no_color)));
        assert!(!use_color(ColorWhen::Never, true, &env(&[])));
    }

    #[test]
    fn verbosity_raises_the_log_level() {
        assert_eq!(default_filter(0), "warn");
        assert!(default_filter(1).contains("rustman_http=info"));
        assert!(default_filter(2).contains("rustman_http=debug"));
        assert_eq!(default_filter(3), "trace");
        for verbose in 0..4 {
            EnvFilter::new(default_filter(verbose));
        }
    }
}
