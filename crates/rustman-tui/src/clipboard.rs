//! Copying to the system clipboard (spec: JSON tree viewer, scenario "Copy
//! a value"): through the platform's clipboard tool when there is one,
//! else with an OSC 52 escape sequence the terminal forwards to its
//! clipboard (which also works over SSH). No clipboard crate is used: they
//! need X11/Wayland libraries on Linux.

use std::io::Write;
use std::process::{Command, Stdio};

use base64::Engine;

/// A clipboard tool: the program and its arguments.
pub type Tool = (&'static str, &'static [&'static str]);

/// The tools tried, in order, on this platform.
pub fn platform_tools() -> Vec<Tool> {
    if cfg!(target_os = "macos") {
        vec![("pbcopy", &[])]
    } else if cfg!(windows) {
        vec![("clip", &[])]
    } else {
        let mut tools: Vec<Tool> = Vec::new();
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            tools.push(("wl-copy", &[]));
        }
        if std::env::var_os("DISPLAY").is_some() {
            tools.push(("xclip", &["-selection", "clipboard"]));
            tools.push(("xsel", &["--clipboard", "--input"]));
        }
        tools
    }
}

/// Copy `text` with the first tool that works; returns its name, or
/// `None` when none did (the caller falls back to OSC 52).
pub fn copy_with(text: &str, tools: &[Tool]) -> Option<&'static str> {
    tools
        .iter()
        .find(|(program, args)| run(program, args, text))
        .map(|(program, _)| *program)
}

fn run(program: &str, args: &[&str], text: &str) -> bool {
    let child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        return false;
    };
    let written = child
        .stdin
        .take()
        .is_some_and(|mut stdin| stdin.write_all(text.as_bytes()).is_ok());
    child.wait().is_ok_and(|status| status.success()) && written
}

/// The OSC 52 sequence that puts `text` on the terminal's clipboard.
pub fn osc52(text: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    format!("\x1b]52;c;{encoded}\x07")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc52_encodes_the_text() {
        assert_eq!(osc52("42"), "\x1b]52;c;NDI=\x07");
    }

    #[test]
    fn a_missing_tool_falls_through() {
        let tools: [Tool; 1] = [("rustman-no-such-clipboard-tool", &[])];
        assert_eq!(copy_with("42", &tools), None);
        assert_eq!(copy_with("42", &[]), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_working_tool_is_used() {
        // `false` fails; `cat` reads the text and succeeds.
        let tools: [Tool; 2] = [("false", &[]), ("cat", &[])];
        assert_eq!(copy_with("42", &tools), Some("cat"));
    }
}
