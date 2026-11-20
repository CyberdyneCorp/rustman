//! `rustman tui [PATH]`, and `rustman` with no subcommand.

use std::path::PathBuf;

use clap::Args;
use rustman_tui::TuiError;
use rustman_tui::config::Overrides;
use rustman_tui::theme::ThemeName;

use crate::exit;

#[derive(Debug, Default, Args)]
pub struct TuiArgs {
    /// Directory to open; the workspace is searched for from here upwards
    /// (default: the current directory)
    #[arg(value_name = "PATH")]
    path: Option<PathBuf>,
    /// Colour theme: dark (default), light or high-contrast; overrides
    /// `theme` in tui.toml
    #[arg(long, value_name = "NAME", value_parser = parse_theme)]
    theme: Option<ThemeName>,
    /// Draw with ASCII characters only (borders, symbols)
    #[arg(long)]
    ascii: bool,
    /// Vim keys: hjkl navigation and modal editing; overrides `keymap` in
    /// tui.toml
    #[arg(long)]
    vim: bool,
}

/// The workspace plugin trust prompt, in a build with plugins.
fn plugin_gate() -> rustman_tui::plugin_trust::Gate {
    #[cfg(feature = "plugins")]
    let gate = rustman_tui::plugin_trust::Gate(Some(std::sync::Arc::new(
        rustman_plugin::trust::TrustGate::default(),
    )));
    #[cfg(not(feature = "plugins"))]
    let gate = rustman_tui::plugin_trust::Gate::default();
    gate
}

fn parse_theme(name: &str) -> Result<ThemeName, String> {
    name.parse()
}

/// True when the TUI can run (stdin and stdout are terminals).
pub fn is_interactive() -> bool {
    rustman_tui::is_interactive()
}

/// Run the TUI; returns the exit code (errors are printed here).
pub fn run(args: &TuiArgs) -> u8 {
    let start = match super::current_dir() {
        Ok(cwd) => args
            .path
            .as_ref()
            .map_or_else(|| cwd.clone(), |p| cwd.join(p)),
        Err(err) => return fail(&err, exit::CONFIG),
    };
    let overrides = Overrides {
        theme: args.theme,
        ascii: args.ascii,
        vim: args.vim,
    };
    match rustman_tui::run_with(&start, overrides, plugin_gate()) {
        Ok(()) => exit::SUCCESS,
        Err(err @ (TuiError::Io(_) | TuiError::Failed(_))) => fail(&err, 1),
        Err(TuiError::Signal { code, .. }) => code,
        Err(err) => fail(&err, exit::CONFIG),
    }
}

fn fail(err: &dyn std::fmt::Display, code: u8) -> u8 {
    eprintln!("{}", exit::render(err));
    code
}
