//! The reader thread (design D2, D4): decodes the producer's lines from
//! stdin, answers `hello` on stdout, and hands every other message to the
//! UI thread through the [`Inbox`]. End of stdin closes the inbox: the
//! producer is gone, and open windows stay usable.

use std::io::{Read, Write};

use rustman_viz_protocol::{
    HelloAck, HelloReject, LineDecoder, Message, PROTOCOL, View, encode_line, is_compatible,
};

use crate::inbox::Inbox;

/// The views this companion opens.
pub const VIEWS: [View; 4] = [
    View::RunDashboard,
    View::ExplorationDashboard,
    View::ApiGraph,
    View::ResponseInspector,
];

/// `rustman-viz <version>`.
pub fn viewer() -> String {
    format!("rustman-viz {}", env!("CARGO_PKG_VERSION"))
}

/// Read `input` until it ends. `wake` is called after each batch so the UI
/// thread looks at the inbox.
pub fn read_session(mut input: impl Read, output: &mut impl Write, inbox: &Inbox, wake: &dyn Fn()) {
    let mut decoder = LineDecoder::new();
    let mut chunk = vec![0; 64 * 1024];
    let mut rejected = false;
    loop {
        let n = match input.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        for message in decoder.push(&chunk[..n]) {
            match message {
                Ok(Message::Hello(hello)) => {
                    rejected = !is_compatible(&hello.protocol);
                    let reply = if rejected {
                        Message::HelloReject(HelloReject {
                            supported: vec![PROTOCOL.to_owned()],
                        })
                    } else {
                        Message::HelloAck(HelloAck {
                            protocol: PROTOCOL.to_owned(),
                            viewer: viewer(),
                            views: VIEWS.to_vec(),
                        })
                    };
                    reply_with(output, &reply);
                }
                // After a rejection the producer sends nothing more; what
                // it might send would not be understood.
                Ok(_) if rejected => {}
                Ok(Message::Unknown | Message::Close(_)) => {}
                Ok(message) => inbox.push(message),
                Err(e) => eprintln!("rustman-viz: skipped a line: {e}"),
            }
        }
        wake();
    }
    inbox.close();
    wake();
}

fn reply_with(output: &mut impl Write, message: &Message) {
    if let Ok(line) = encode_line(message) {
        let _ = output
            .write_all(line.as_bytes())
            .and_then(|()| output.flush());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(input: &str) -> (String, Vec<Message>, bool) {
        let inbox = Inbox::default();
        let mut out = Vec::new();
        read_session(input.as_bytes(), &mut out, &inbox, &|| {});
        (
            String::from_utf8(out).unwrap(),
            inbox.drain(),
            inbox.is_closed(),
        )
    }

    #[test]
    fn hello_is_answered_and_messages_are_queued() {
        let input = concat!(
            r#"{"type":"hello","protocol":"rustman-viz/1","producer":"rustman 0.1.0"}"#,
            "\n",
            r#"{"type":"open_view","view":"run_dashboard","title":"smoke/"}"#,
            "\n",
            r#"{"type":"run_finished","state":"completed","future":true,"summary":{"total":1,"passed":1,"failed":0,"errored":0,"skipped":0,"duration_ms":3}}"#,
            "\n",
            r#"{"type":"from_the_future","x":1}"#,
            "\n",
            r#"{"type":"close"}"#,
            "\n"
        );
        let (out, messages, closed) = session(input);
        assert!(
            out.starts_with(r#"{"type":"hello_ack","protocol":"rustman-viz/1""#),
            "{out}"
        );
        assert!(out.contains(r#""views":["run_dashboard","exploration_dashboard","api_graph","response_inspector"]"#), "{out}");
        let kinds: Vec<_> = messages.iter().map(|m| m.kind().unwrap()).collect();
        // The unknown field and the unknown type are ignored.
        assert_eq!(kinds, ["open_view", "run_finished"]);
        assert!(closed, "end of stdin closes the inbox");
    }

    #[test]
    fn another_major_version_is_rejected_and_nothing_follows() {
        let input = concat!(
            r#"{"type":"hello","protocol":"rustman-viz/2","producer":"rustman 9"}"#,
            "\n",
            r#"{"type":"open_view","view":"run_dashboard"}"#,
            "\n"
        );
        let (out, messages, _) = session(input);
        assert_eq!(
            out.trim(),
            r#"{"type":"hello_reject","supported":["rustman-viz/1"]}"#
        );
        assert!(messages.is_empty());
    }
}
