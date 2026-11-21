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
    /// A decoded output of an EVM call (add-evm-toolkit).
    EvmOutput,
    /// A field of a decoded EVM event.
    EvmEvent,
    /// A field of an EVM transaction receipt.
    EvmReceipt,
}

/// The receipt fields `from: evm_receipt` captures.
pub const EVM_RECEIPT_FIELDS: [&str; 4] = ["contractAddress", "blockNumber", "status", "gasUsed"];

impl CaptureSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Header => "header",
            Self::Status => "status",
            Self::BodyRegex => "body_regex",
            Self::Cookie => "cookie",
            Self::Metadata => "metadata",
            Self::EvmOutput => "evm_output",
            Self::EvmEvent => "evm_event",
            Self::EvmReceipt => "evm_receipt",
        }
    }

    fn fields(self) -> (&'static [&'static str], &'static [&'static str]) {
        match self {
            Self::Json => (&["path"], &[]),
            Self::Header | Self::Cookie | Self::Metadata => (&["name"], &[]),
            Self::Status => (&[], &[]),
            Self::BodyRegex => (&["pattern"], &["group"]),
            Self::EvmOutput => (&["output"], &[]),
            Self::EvmEvent => (&["event", "field"], &[]),
            Self::EvmReceipt => (&["field"], &[]),
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
    /// The output name or index, for `from: evm_output`.
    pub output: Option<super::evm_check::OutputRef>,
    /// The event signature, for `from: evm_event`.
    pub event: Option<String>,
    /// The event or receipt field, for `from: evm_event` and
    /// `from: evm_receipt`.
    pub field: Option<String>,
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
            ("output", given(self.output.is_some())),
            ("event", given(self.event.is_some())),
            ("field", given(self.field.is_some())),
        ];
        let what = format!("capture from '{}'", self.from.as_str());
        check_fields(&what, &present, required, optional)?;
        let receipt_field = self.field.as_deref().unwrap_or_default();
        if self.from == CaptureSource::EvmReceipt && !EVM_RECEIPT_FIELDS.contains(&receipt_field) {
            return Err(format!(
                "capture from 'evm_receipt': field must be one of {}",
                EVM_RECEIPT_FIELDS.join(", ")
            ));
        }
        Ok(())
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

    #[test]
    fn evm_captures_load_and_are_checked() {
        let ok = captures(
            "- {var: owner, from: evm_output, output: 0}\n- {var: to, from: evm_event, event: \"Transfer(address,address,uint256)\", field: to}\n- {var: block, from: evm_receipt, field: blockNumber}\n",
        )
        .unwrap();
        assert_eq!(ok[0].from, CaptureSource::EvmOutput);
        let err = captures("- {var: t, from: evm_event, event: X()}\n").unwrap_err();
        assert_eq!(err.message, "capture from 'evm_event' requires 'field'");
        let err = captures("- {var: t, from: evm_receipt, field: logs}\n").unwrap_err();
        assert_eq!(
            err.message,
            "capture from 'evm_receipt': field must be one of contractAddress, blockNumber, status, gasUsed"
        );
        let err = captures("- {var: t, from: json, path: $.a, output: 1}\n").unwrap_err();
        assert_eq!(err.message, "capture from 'json' does not accept 'output'");
    }
}
