//! Message payloads as bytes (messaging-requests "Message payloads"):
//! text as UTF-8, JSON compactly, base64 decoded, files read from inside
//! the workspace. Received bytes are rendered back as JSON, text or
//! base64 for display and history.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rustman_core::vars::{ResolvedMessage, ResolvedPayload};
use serde::Serialize;
use serde_json::{Value, json};

use crate::error::MessagingError;

/// The bytes of `payload`.
pub async fn encode(payload: &ResolvedPayload) -> Result<Vec<u8>, MessagingError> {
    match payload {
        ResolvedPayload::Text(text) => Ok(text.as_bytes().to_vec()),
        ResolvedPayload::Json(value) => Ok(compact_json(value)),
        ResolvedPayload::Base64(data) => STANDARD.decode(data.trim()).map_err(|e| {
            MessagingError::Invalid(format!("messaging: invalid base64 payload: {e}"))
        }),
        ResolvedPayload::File(file) => tokio::fs::read(&file.path)
            .await
            .map_err(|e| cannot_read(&file.raw, &e)),
    }
}

/// Fail when a file key or value of `message` cannot be read, without
/// reading it.
pub async fn check_files(message: &ResolvedMessage) -> Result<(), MessagingError> {
    let files = [&message.key, &message.value].into_iter().flatten();
    for payload in files {
        let ResolvedPayload::File(file) = payload else {
            continue;
        };
        let meta = tokio::fs::metadata(&file.path)
            .await
            .map_err(|e| cannot_read(&file.raw, &e))?;
        if !meta.is_file() {
            return Err(cannot_read(&file.raw, &std::io::Error::other("not a file")));
        }
    }
    Ok(())
}

fn cannot_read(raw: &str, error: &std::io::Error) -> MessagingError {
    MessagingError::Io(format!("messaging: cannot read {raw}: {error}"))
}

/// `value` serialized compactly as UTF-8.
pub fn compact_json(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_default()
}

/// A message ready to send: key, value and headers as bytes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EncodedMessage {
    pub key: Option<Vec<u8>>,
    pub value: Option<Vec<u8>>,
    pub headers: Vec<(String, Vec<u8>)>,
}

impl EncodedMessage {
    /// Key, value and header bytes together.
    pub fn size(&self) -> usize {
        let headers: usize = self.headers.iter().map(|(n, v)| n.len() + v.len()).sum();
        self.key.as_ref().map_or(0, Vec::len) + self.value.as_ref().map_or(0, Vec::len) + headers
    }
}

/// Encode the key, value and headers of `message`.
pub async fn encode_message(message: &ResolvedMessage) -> Result<EncodedMessage, MessagingError> {
    let key = match &message.key {
        Some(key) => Some(encode(key).await?),
        None => None,
    };
    let value = match &message.value {
        Some(value) => Some(encode(value).await?),
        None => None,
    };
    let headers = message
        .headers
        .iter()
        .map(|(name, value)| (name.clone(), value.as_bytes().to_vec()))
        .collect();
    Ok(EncodedMessage {
        key,
        value,
        headers,
    })
}

/// Received bytes for display: parsed JSON when they are JSON, a string
/// when they are UTF-8 text, else `{"base64": ...}`.
pub fn render(bytes: &[u8]) -> Value {
    if let Ok(value) = serde_json::from_slice::<Value>(bytes) {
        return value;
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => Value::String(text.to_owned()),
        Err(_) => json!({ "base64": STANDARD.encode(bytes) }),
    }
}

/// At most `max` bytes of `bytes`; a cut never splits a UTF-8 character
/// of a text value.
pub fn cut(bytes: &[u8], max: usize) -> &[u8] {
    if bytes.len() <= max {
        return bytes;
    }
    let head = &bytes[..max];
    match std::str::from_utf8(head) {
        Err(e) if e.error_len().is_none() && e.valid_up_to() > 0 => &head[..e.valid_up_to()],
        _ => head,
    }
}

/// A header as recorded: the value as text, or base64 when it is not
/// UTF-8.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Header {
    pub name: String,
    pub value: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub base64: bool,
}

impl Header {
    pub fn from_bytes(name: &str, value: &[u8]) -> Self {
        match std::str::from_utf8(value) {
            Ok(text) => Self {
                name: name.to_owned(),
                value: text.to_owned(),
                base64: false,
            },
            Err(_) => Self {
                name: name.to_owned(),
                value: STANDARD.encode(value),
                base64: true,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use rustman_core::vars::ResolvedFile;

    use super::*;

    fn file(path: std::path::PathBuf) -> ResolvedPayload {
        ResolvedPayload::File(ResolvedFile {
            raw: "blob.bin".into(),
            path,
        })
    }

    #[tokio::test]
    async fn the_key_value_and_headers_scenario_encodes_compact_json() {
        let message = ResolvedMessage {
            key: Some(ResolvedPayload::Text("order-1".into())),
            value: Some(ResolvedPayload::Json(json!({"id": 1, "total": 9.5}))),
            headers: vec![("source".into(), "rustman".into())],
        };
        let encoded = encode_message(&message).await.unwrap();
        assert_eq!(encoded.key.as_deref(), Some(&b"order-1"[..]));
        assert_eq!(
            encoded.value.as_deref(),
            Some(&br#"{"id":1,"total":9.5}"#[..])
        );
        assert_eq!(
            encoded.headers,
            [("source".to_owned(), b"rustman".to_vec())]
        );
        assert_eq!(encoded.size(), 7 + 20 + 13);
    }

    #[tokio::test]
    async fn unreadable_files_are_found_without_reading_them() {
        let dir = tempfile::tempdir().unwrap();
        let present = dir.path().join("blob.bin");
        std::fs::write(&present, b"x").unwrap();
        let message = |value| ResolvedMessage {
            key: Some(ResolvedPayload::Text("k".into())),
            value: Some(value),
            headers: Vec::new(),
        };
        check_files(&message(file(present))).await.unwrap();
        for bad in [dir.path().join("missing.bin"), dir.path().to_path_buf()] {
            let err = check_files(&message(file(bad))).await.unwrap_err();
            assert!(
                err.to_string()
                    .starts_with("messaging: cannot read blob.bin: "),
                "{err}"
            );
        }
    }

    #[tokio::test]
    async fn base64_and_files_are_bytes() {
        let bytes = encode(&ResolvedPayload::Base64("AAE=".into()))
            .await
            .unwrap();
        assert_eq!(bytes, [0, 1]);
        let err = encode(&ResolvedPayload::Base64("***".into()))
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .starts_with("messaging: invalid base64 payload"),
            "{err}"
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob.bin");
        std::fs::write(&path, [7u8, 8, 255]).unwrap();
        assert_eq!(encode(&file(path)).await.unwrap(), [7, 8, 255]);
        let err = encode(&file(dir.path().join("missing.bin")))
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .starts_with("messaging: cannot read blob.bin"),
            "{err}"
        );
    }

    #[test]
    fn received_bytes_render_as_json_text_or_base64() {
        assert_eq!(render(br#"{"id":1}"#), json!({"id": 1}));
        assert_eq!(render(b"plain text"), json!("plain text"));
        assert_eq!(render(&[0, 159, 146, 150]), json!({"base64": "AJ+Slg=="}));
        let header = Header::from_bytes("bin", &[255]);
        assert!(header.base64);
        assert_eq!(header.value, "/w==");
        assert!(!Header::from_bytes("t", b"x").base64);
    }
}
