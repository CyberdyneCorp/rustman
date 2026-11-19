//! Captures: `{var, from: json|header|status|body_regex|cookie|metadata, ...}`.

use serde::Deserialize;

use super::common::{Validate, check_fields, given};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureSource {
    Json,
    Header,
    Status,
    BodyRegex,
    Cookie,
    /// A gRPC response header or trailer (add-grpc-client).
    Metadata,
}

impl CaptureSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Header => "header",
            Self::Status => "status",
            Self::BodyRegex => "body_regex",
            Self::Cookie => "cookie",
            Self::Metadata => "metadata",
        }
    }

    fn fields(self) -> (&'static [&'static str], &'static [&'static str]) {
        match self {
            Self::Json => (&["path"], &[]),
            Self::Header | Self::Cookie | Self::Metadata => (&["name"], &[]),
            Self::Status => (&[], &[]),
            Self::BodyRegex => (&["pattern"], &["group"]),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capture {
    pub var: String,
    pub from: CaptureSource,
    /// JSONPath, for `from: json`.
    pub path: Option<String>,
    /// Header, cookie or metadata name, for `from: header`, `from: cookie`
    /// and `from: metadata`.
    pub name: Option<String>,
    /// Regular expression, for `from: body_regex`.
    pub pattern: Option<String>,
    /// Capture group of `pattern` (default 1, or the whole match when it has no groups).
    pub group: Option<u32>,
    pub required: Option<bool>,
    pub persist: Option<bool>,
}

impl Validate for Capture {
    fn validate(&self) -> Result<(), String> {
        let (required, optional) = self.from.fields();
        let present = [
            ("path", given(self.path.is_some())),
            ("name", given(self.name.is_some())),
            ("pattern", given(self.pattern.is_some())),
            ("group", given(self.group.is_some())),
        ];
        let what = format!("capture from '{}'", self.from.as_str());
        check_fields(&what, &present, required, optional)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::common::Checked;
    use std::path::Path;

    fn captures(text: &str) -> Result<Vec<Checked<Capture>>, crate::error::LoadError> {
        crate::format::from_yaml(Path::new("c.yaml"), text)
    }

    #[test]
    fn captures_load_and_are_checked() {
        let ok = captures(
            "- {var: token, from: json, path: $.access_token}\n- {var: sid, from: cookie, name: sid, persist: true}\n- {var: code, from: status}\n- {var: id, from: body_regex, pattern: 'id=(\\d+)', group: 1, required: true}\n",
        )
        .unwrap();
        assert_eq!(ok.len(), 4);
        let metadata = captures("- {var: served, from: metadata, name: x-served-by}\n").unwrap();
        assert_eq!(metadata[0].from, CaptureSource::Metadata);
        let err = captures("- {var: t, from: metadata}\n").unwrap_err();
        assert_eq!(err.message, "capture from 'metadata' requires 'name'");
        let err = captures("- {var: t, from: json}\n").unwrap_err();
        assert_eq!(err.message, "capture from 'json' requires 'path'");
        let err = captures("- {var: t, from: status, name: x}\n").unwrap_err();
        assert_eq!(err.message, "capture from 'status' does not accept 'name'");
    }
}
