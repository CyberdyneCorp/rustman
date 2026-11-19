//! Opening the authorization URL in the system browser, for front ends that
//! log in to MCP servers. Without a browser the URL they show is the way
//! in, so failures are ignored.

use std::process::{Command, Stdio};

/// Open `url` in the system browser, without waiting for it.
pub fn open_browser(url: &str) {
    let mut command = if cfg!(target_os = "macos") {
        Command::new("open")
    } else if cfg!(windows) {
        let mut command = Command::new("rundll32");
        command.arg("url.dll,FileProtocolHandler");
        command
    } else {
        Command::new("xdg-open")
    };
    let _ = command
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}
