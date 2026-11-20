//! Opening a URL in the system browser, for every front end: OAuth logins
//! and trace links. Nothing waits for the browser.

use std::io;
use std::process::{Command, Stdio};

/// Open `url` in the system browser, without waiting for it. Only an
/// absolute `http://` or `https://` URL is opened: the openers also run
/// files and take options, and the URL may come from a workspace. An error
/// means the URL was refused or the opener could not start; callers show
/// the URL instead.
pub fn open_browser(url: &str) -> io::Result<()> {
    if !is_web_url(url) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "only http:// and https:// URLs are opened",
        ));
    }
    command(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(drop)
}

fn is_web_url(url: &str) -> bool {
    url::Url::parse(url)
        .is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some())
}

/// The platform's opener for `url`: `open`, `rundll32` or `xdg-open`.
fn command(url: &str) -> Command {
    let mut command = if cfg!(target_os = "macos") {
        Command::new("open")
    } else if cfg!(windows) {
        let mut command = Command::new("rundll32");
        command.arg("url.dll,FileProtocolHandler");
        command
    } else {
        Command::new("xdg-open")
    };
    command.arg(url);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_web_urls_are_opened() {
        for url in [
            "file:///tmp/x.command",
            "-a Calculator",
            "javascript:alert(1)",
            "x.command",
        ] {
            let err = open_browser(url).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{url}");
        }
        assert!(is_web_url("https://ui/trace/abc"));
        assert!(is_web_url("http://localhost:16686/trace/abc"));
    }

    #[test]
    fn the_url_is_the_last_argument_of_the_platform_opener() {
        let command = command("http://localhost:16686/trace/abc");
        let program = command.get_program().to_string_lossy().into_owned();
        let expected = if cfg!(target_os = "macos") {
            "open"
        } else if cfg!(windows) {
            "rundll32"
        } else {
            "xdg-open"
        };
        assert_eq!(program, expected);
        let last = command
            .get_args()
            .last()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(last, "http://localhost:16686/trace/abc");
    }
}
