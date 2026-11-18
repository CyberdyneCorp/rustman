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

/// The variable that overrides OSC 52 detection: `1` on, `0` off.
pub const OSC52_VAR: &str = "RUSTMAN_OSC52";

/// `TERM_PROGRAM` (or `LC_TERMINAL`, which SSH forwards) values of
/// terminals that take OSC 52.
const OSC52_PROGRAMS: [&str; 7] = [
    "iTerm.app",
    "iTerm2",
    "WezTerm",
    "ghostty",
    "vscode",
    "rio",
    "tmux",
];

/// `TERM` substrings of terminals that take OSC 52.
const OSC52_TERMS: [&str; 6] = [
    "kitty",
    "alacritty",
    "foot",
    "wezterm",
    "ghostty",
    "contour",
];

/// Whether the terminal takes OSC 52, from its environment (`var` reads
/// a variable): [`OSC52_VAR`] decides when set. Inside tmux it depends on
/// tmux's `set-clipboard` option (`tmux_clipboard` reads it): only `on`
/// passes an application's sequence on; the default, `external`, drops
/// it. Otherwise Windows Terminal and the terminals known to support it
/// do. Others (Apple's Terminal among them) get the read-only pane.
pub fn osc52_supported(
    var: impl Fn(&str) -> Option<String>,
    tmux_clipboard: impl FnOnce() -> Option<String>,
) -> bool {
    match var(OSC52_VAR).as_deref().map(str::trim) {
        Some("1" | "true" | "on") => return true,
        Some("0" | "false" | "off") => return false,
        _ => {}
    }
    if var("TMUX").is_some() {
        return tmux_clipboard().is_some_and(|value| value.trim() == "on");
    }
    if var("WT_SESSION").is_some() {
        return true;
    }
    let program = |name: &str| var(name).is_some_and(|v| OSC52_PROGRAMS.contains(&v.as_str()));
    let term = var("TERM").unwrap_or_default();
    program("TERM_PROGRAM")
        || program("LC_TERMINAL")
        || OSC52_TERMS.iter().any(|t| term.contains(t))
}

/// The tmux server's global `set-clipboard` option, if tmux answers.
pub fn tmux_set_clipboard() -> Option<String> {
    let output = Command::new("tmux")
        .args(["show-options", "-gv", "set-clipboard"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
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
    fn osc52_sequence_bytes() {
        let bytes = osc52("curl -X GET 'http://x'\n").into_bytes();
        assert_eq!(&bytes[..7], b"\x1b]52;c;");
        assert_eq!(bytes.last(), Some(&0x07));
        assert_eq!(
            &bytes[7..bytes.len() - 1],
            b"Y3VybCAtWCBHRVQgJ2h0dHA6Ly94Jwo="
        );
    }

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    fn no_tmux() -> Option<String> {
        panic!("tmux is asked only inside tmux")
    }

    #[test]
    fn osc52_support_is_detected_from_the_environment() {
        let supported = |pairs: &[(&str, &str)]| osc52_supported(env(pairs), no_tmux);
        assert!(!supported(&[]));
        assert!(!supported(&[("TERM_PROGRAM", "Apple_Terminal")]));
        assert!(!supported(&[("TERM", "xterm-256color")]));
        assert!(supported(&[("WT_SESSION", "abc")]));
        assert!(supported(&[("TERM_PROGRAM", "iTerm.app")]));
        assert!(supported(&[("LC_TERMINAL", "iTerm2")]), "over SSH");
        assert!(supported(&[("TERM", "xterm-kitty")]));
        assert!(supported(&[("TERM", "dumb"), (OSC52_VAR, "1")]));
        assert!(!supported(&[("TMUX", "x"), (OSC52_VAR, "0")]));
    }

    #[test]
    fn inside_tmux_osc52_needs_set_clipboard_on() {
        let tmux = [
            ("TMUX", "/tmp/tmux-1/default,1,0"),
            ("TERM", "tmux-256color"),
            ("TERM_PROGRAM", "tmux"),
        ];
        assert!(osc52_supported(env(&tmux), || Some("on\n".into())));
        assert!(
            !osc52_supported(env(&tmux), || Some("external\n".into())),
            "tmux's default drops application OSC 52"
        );
        assert!(!osc52_supported(env(&tmux), || Some("off\n".into())));
        assert!(!osc52_supported(env(&tmux), || None), "tmux did not answer");
        let forced = [("TMUX", "x"), (OSC52_VAR, "1")];
        assert!(osc52_supported(env(&forced), no_tmux));
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
