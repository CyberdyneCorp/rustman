//! The `docs:` key of a request file, read by `rustman docs`.

use serde::Deserialize;

/// How a request appears in generated documentation. Every field is
/// optional, so a file keeps exactly the keys it had when saved again.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocsMeta {
    /// Publish the latest recorded exchange as an example (opt-in).
    pub example: Option<bool>,
    /// Leave the request out of the generated site.
    pub hidden: Option<bool>,
    /// Extra CommonMark shown after the request's own description.
    pub description: Option<String>,
}

impl DocsMeta {
    pub fn is_hidden(&self) -> bool {
        self.hidden == Some(true)
    }

    pub fn wants_example(&self) -> bool {
        self.example == Some(true)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::DocsMeta;
    use crate::format::write::request_to_yaml;
    use crate::model::{KindRegistry, Request};

    const DOCS: &str =
        "docs:\n  example: true\n  hidden: false\n  description: Lists *active* users.\n";

    fn load(registry: &KindRegistry, text: &str) -> Request {
        registry.load(Path::new("r.rman.yaml"), text).unwrap()
    }

    fn expected() -> DocsMeta {
        DocsMeta {
            example: Some(true),
            hidden: Some(false),
            description: Some("Lists *active* users.".into()),
        }
    }

    #[test]
    fn every_kind_reads_the_docs_key() {
        let head = |kind: &str| format!("version: 1\nkind: {kind}\nname: N\n{DOCS}");
        let files = [
            format!("{}url: http://x/\n", head("http")),
            format!(
                "{}endpoint: http://x/\ndocument: '{{ a }}'\n",
                head("graphql")
            ),
            format!("{}endpoint: http://x/\nmethod: m\n", head("jsonrpc")),
            format!("{}server: s\noperation: tools/list\n", head("mcp")),
            format!(
                "{}target: localhost:1\nmethod: a.B/C\nmessage: {{}}\n",
                head("grpc")
            ),
            format!("{}url: ws://x/\n", head("ws")),
            format!("{}url: http://x/\n", head("sse")),
            format!("{}topic: t\n", head("kafka")),
        ];
        let registry = KindRegistry::default().with_opaque_fallback();
        for text in files {
            let request = load(&registry, &text);
            assert_eq!(request.docs(), Some(&expected()), "{text}");
        }
    }

    #[test]
    fn the_docs_key_survives_a_load_write_round_trip() {
        let text = format!("version: 1\nkind: http\nname: N\n{DOCS}url: http://x/\n");
        let registry = KindRegistry::default();
        let request = load(&registry, &text);
        let written = request_to_yaml(&request);
        assert!(
            written.contains("docs:\n  example: true\n  hidden: false\n"),
            "{written}"
        );
        assert_eq!(load(&registry, &written), request);
        let plain = load(
            &registry,
            "version: 1\nkind: http\nname: N\nurl: http://x/\n",
        );
        assert_eq!(plain.docs(), None);
        assert!(!request_to_yaml(&plain).contains("docs"));
    }

    #[test]
    fn unknown_docs_keys_are_rejected() {
        let text = "version: 1\nkind: http\nname: N\ndocs:\n  hide: true\nurl: http://x/\n";
        let err = KindRegistry::default()
            .load(Path::new("r.rman.yaml"), text)
            .unwrap_err();
        assert!(
            err.message.contains("unknown field `hide`"),
            "{}",
            err.message
        );
    }

    #[test]
    fn flags_default_to_off() {
        let meta = DocsMeta::default();
        assert!(!meta.is_hidden() && !meta.wants_example());
        assert!(expected().wants_example());
    }
}
