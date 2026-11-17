//! `multipart/form-data` request bodies (RFC 7578).

use std::path::Path;

use bytes::{BufMut, Bytes, BytesMut};
use ring::rand::{SecureRandom, SystemRandom};
use rustman_core::vars::{ResolvedPart, ResolvedPartSource};

use crate::error::RequestError;

/// The encoded parts and the boundary that separates them.
pub(crate) struct Multipart {
    pub boundary: String,
    pub body: Bytes,
}

impl Multipart {
    /// `Content-Type: multipart/form-data; boundary=...`
    pub fn content_type(&self) -> String {
        format!("multipart/form-data; boundary={}", self.boundary)
    }
}

/// Encode `parts`, reading file parts from disk.
pub(crate) async fn encode(parts: &[ResolvedPart]) -> Result<Multipart, RequestError> {
    let boundary = boundary()?;
    let mut body = BytesMut::new();
    for part in parts {
        let (filename, content) = match &part.source {
            ResolvedPartSource::Text(text) => (None, Bytes::from(text.text.clone())),
            ResolvedPartSource::File(file) => {
                let bytes = tokio::fs::read(&file.path).await.map_err(|e| {
                    RequestError::Io(format!("cannot read multipart file {}: {e}", file.raw))
                })?;
                (Some(file_name(&file.path)), Bytes::from(bytes))
            }
        };
        let content_type = part
            .content_type
            .as_deref()
            .or(filename.is_some().then_some("application/octet-stream"));
        body.put_slice(format!("--{boundary}\r\n").as_bytes());
        body.put_slice(disposition(&part.name, filename.as_deref()).as_bytes());
        if let Some(content_type) = content_type {
            body.put_slice(format!("Content-Type: {content_type}\r\n").as_bytes());
        }
        body.put_slice(b"\r\n");
        body.put_slice(&content);
        body.put_slice(b"\r\n");
    }
    body.put_slice(format!("--{boundary}--\r\n").as_bytes());
    Ok(Multipart {
        boundary,
        body: body.freeze(),
    })
}

fn boundary() -> Result<String, RequestError> {
    let mut random = [0u8; 12];
    SystemRandom::new()
        .fill(&mut random)
        .map_err(|_| RequestError::Invalid("cannot generate a multipart boundary".into()))?;
    let hex: String = random.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!("rustman-{hex}"))
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn disposition(name: &str, filename: Option<&str>) -> String {
    let mut line = format!("Content-Disposition: form-data; name=\"{}\"", escape(name));
    if let Some(filename) = filename {
        line.push_str(&format!("; filename=\"{}\"", escape(filename)));
    }
    line.push_str("\r\n");
    line
}

/// Quotes and line breaks percent-encoded, as browsers do (HTML form spec).
fn escape(value: &str) -> String {
    value
        .replace('"', "%22")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustman_core::vars::{Resolved, ResolvedFile};

    fn text(name: &str, value: &str, content_type: Option<&str>) -> ResolvedPart {
        ResolvedPart {
            name: name.into(),
            source: ResolvedPartSource::Text(Resolved::literal(value)),
            content_type: content_type.map(str::to_owned),
        }
    }

    #[tokio::test]
    async fn parts_are_framed_by_the_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.png");
        std::fs::write(&path, [0x89, b'P']).unwrap();
        let file = ResolvedPart {
            name: "f".into(),
            source: ResolvedPartSource::File(ResolvedFile {
                raw: "a.png".into(),
                path,
            }),
            content_type: None,
        };
        let parts = [
            text("a\"b", "x", None),
            text("j", "{}", Some("application/json")),
            file,
        ];
        let encoded = encode(&parts).await.unwrap();
        let b = &encoded.boundary;
        assert!(b.starts_with("rustman-") && b.len() == 32, "{b}");
        let expected = [
            format!("--{b}\r\nContent-Disposition: form-data; name=\"a%22b\"\r\n\r\nx\r\n").into_bytes(),
            format!("--{b}\r\nContent-Disposition: form-data; name=\"j\"\r\nContent-Type: application/json\r\n\r\n{{}}\r\n").into_bytes(),
            format!("--{b}\r\nContent-Disposition: form-data; name=\"f\"; filename=\"a.png\"\r\nContent-Type: application/octet-stream\r\n\r\n").into_bytes(),
            vec![0x89, b'P'],
            format!("\r\n--{b}--\r\n").into_bytes(),
        ]
        .concat();
        assert_eq!(&encoded.body[..], &expected[..]);
        assert_eq!(
            encoded.content_type(),
            format!("multipart/form-data; boundary={b}")
        );
    }

    #[tokio::test]
    async fn a_missing_file_is_an_io_error() {
        let part = ResolvedPart {
            name: "f".into(),
            source: ResolvedPartSource::File(ResolvedFile {
                raw: "nope.bin".into(),
                path: "/no/such/nope.bin".into(),
            }),
            content_type: None,
        };
        let err = encode(&[part]).await.err().unwrap();
        assert!(
            err.to_string()
                .starts_with("cannot read multipart file nope.bin:"),
            "{err}"
        );
    }
}
