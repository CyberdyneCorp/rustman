//! "Copy as curl" (spec: TUI webhook pane): a capture as the curl command
//! that replays it, with the same header hygiene as replay.

use bytes::Bytes;
use rustman_core::Redactor;
use rustman_core::vars::{ResolvedBody, ResolvedFile};
use rustman_http::{Exported, RequestError, export_curl};

use crate::capture::Capture;
use crate::replay::ReplayRequest;

/// `capture` as a curl command sending it to `url`, every value redacted
/// with `redactor`. A body that is not UTF-8 is referenced as
/// `@capture-<id>.body.bin`, a file the user writes themselves (a warning
/// says so).
pub fn copy_as_curl(
    capture: &Capture,
    url: &str,
    redactor: &Redactor,
) -> Result<Exported, RequestError> {
    let binary = std::str::from_utf8(&capture.body).is_err();
    let mut warnings = Vec::new();
    let request = if binary {
        // Nothing is staged on disk: the command names a file instead.
        let mut headers_only = capture.clone();
        headers_only.body = Bytes::new();
        let mut request = ReplayRequest::new(&headers_only, url)?.request;
        let name = format!("capture-{}.body.bin", capture.id);
        warnings.push(format!(
            "the body is binary: save it as {name} next to where the command runs"
        ));
        request.body = ResolvedBody::File(ResolvedFile {
            raw: name.clone(),
            path: name.into(),
        });
        request
    } else {
        ReplayRequest::new(capture, url)?.request
    };
    let exported = export_curl(&request, false);
    warnings.extend(exported.warnings);
    Ok(Exported {
        command: redactor.text(&exported.command).into_owned(),
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use rustman_core::Secret;

    use super::*;
    use crate::capture::HeaderEntry;
    use crate::capture::tests::capture;

    #[test]
    fn a_capture_becomes_a_curl_command_without_framing_headers() {
        let mut c = capture("/hooks/github", b"{\"a\":1}");
        c.headers.push(HeaderEntry::new("Host", "127.0.0.1:9090"));
        c.headers.push(HeaderEntry::new("Content-Length", "7"));
        c.headers.push(HeaderEntry::new("X-GitHub-Event", "push"));
        let exported = copy_as_curl(&c, "http://localhost:3000/hook", &Redactor::new()).unwrap();
        let command = exported.command;
        assert!(
            command.starts_with("curl 'http://localhost:3000/hook'"),
            "{command}"
        );
        assert!(command.contains("X-GitHub-Event: push"), "{command}");
        assert!(command.contains(r#"{"a":1}"#), "{command}");
        assert!(!command.contains("127.0.0.1:9090"), "{command}");
        assert!(!command.contains("Content-Length"), "{command}");
        assert!(exported.warnings.is_empty());
    }

    #[test]
    fn binary_bodies_and_secrets() {
        let mut c = capture("/h", b"");
        c.id = 3;
        c.body = Bytes::from_static(&[0xff, 0x00]);
        c.headers.push(HeaderEntry::new("X-Echo", "canary-91c2"));
        let mut redactor = Redactor::new();
        redactor.add(&Secret::new("hook", "canary-91c2".to_owned()));
        let exported = copy_as_curl(&c, "http://x/h", &redactor).unwrap();
        assert!(
            exported.command.contains("@capture-3.body.bin"),
            "{}",
            exported.command
        );
        assert!(!exported.command.contains("canary-91c2"));
        assert_eq!(exported.warnings.len(), 1);
    }
}
