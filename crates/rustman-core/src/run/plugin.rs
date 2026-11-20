//! What plugins add to a request record (add-plugin-system): the
//! transformers a request went through, with the response body as it was
//! received, and the exchange of a request of a plugin kind.

use serde::{Deserialize, Serialize};

use super::record::HeaderRecord;
use crate::redact::Redactor;

/// The transformers applied to a request (`transform:`). The exchange
/// holds the transformed response; this holds the body as received, so
/// history stores both (plugins spec, "Transformer extension point").
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransformRecord {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub request: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub response: Vec<String>,
    /// The response body before the response transformers; `None` when it
    /// is not text or no response transformer ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_body: Option<String>,
    /// The size of that body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_body_bytes: Option<u64>,
}

impl TransformRecord {
    #[must_use]
    pub fn redacted(mut self, redactor: &Redactor) -> Self {
        self.original_body = self
            .original_body
            .map(|body| redactor.text(&body).into_owned());
        self
    }
}

/// The exchange of a request of a plugin kind, as the plugin reported it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginKindRecord {
    pub kind: String,
    /// What was sent, in one line.
    pub summary: String,
    /// The request body sent, when it is text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_body: Option<String>,
    pub status: Option<u16>,
    pub headers: Vec<HeaderRecord>,
    /// The response body, when it is text.
    pub body: Option<String>,
    pub body_bytes: u64,
}

impl PluginKindRecord {
    #[must_use]
    pub fn redacted(mut self, redactor: &Redactor) -> Self {
        let text = |value: &str| redactor.text(value).into_owned();
        self.summary = redactor.text(&self.summary).into_owned();
        self.request_body = self.request_body.as_deref().map(text);
        self.body = self.body.as_deref().map(text);
        for header in &mut self.headers {
            header.value = redactor.header_value(&header.name, &header.value);
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Secret;

    #[test]
    fn records_are_redacted() {
        let mut redactor = Redactor::new();
        redactor.add(&Secret::new("api_token", "tok-31f9c2".to_owned()));
        let transform = TransformRecord {
            original_body: Some("body tok-31f9c2".into()),
            ..TransformRecord::default()
        }
        .redacted(&redactor);
        assert_eq!(
            transform.original_body.as_deref(),
            Some("body «redacted:api_token»")
        );
        let kind = PluginKindRecord {
            summary: "POST tok-31f9c2".into(),
            request_body: Some("tok-31f9c2".into()),
            headers: vec![HeaderRecord {
                name: "X-Echo".into(),
                value: "tok-31f9c2".into(),
            }],
            body: Some("tok-31f9c2".into()),
            ..PluginKindRecord::default()
        }
        .redacted(&redactor);
        let json = serde_json::to_string(&kind).unwrap();
        assert!(!json.contains("tok-31f9c2"), "{json}");
    }
}
