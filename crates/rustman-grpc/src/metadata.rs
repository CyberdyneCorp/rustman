//! Request and response metadata (spec "Metadata"). Request metadata is an
//! ordered list of name/value pairs; names ending in `-bin` take base64 and
//! are sent as binary metadata. Response headers and trailers are recorded
//! separately, binary values shown as base64.

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use http::HeaderMap;
use serde::Serialize;
use tonic::metadata::{
    AsciiMetadataKey, AsciiMetadataValue, BinaryMetadataKey, BinaryMetadataValue, KeyAndValueRef,
    MetadataMap,
};

use crate::error::GrpcError;

/// One metadata entry as recorded: binary values in base64.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MetadataEntry {
    pub name: String,
    pub value: String,
}

impl MetadataEntry {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

/// Whether `name` is binary metadata.
pub fn is_binary(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with("-bin")
}

/// Decode a `-bin` value written in base64, padded or not.
pub fn decode_binary(name: &str, value: &str) -> Result<Vec<u8>, GrpcError> {
    let trimmed = value.trim();
    STANDARD
        .decode(trimmed)
        .or_else(|_| STANDARD_NO_PAD.decode(trimmed.trim_end_matches('=')))
        .map_err(|e| GrpcError::Metadata {
            name: name.to_owned(),
            message: format!("binary metadata takes base64: {e}"),
        })
}

/// The request metadata `entries`, checked and ready to send.
pub fn request_metadata(entries: &[MetadataEntry]) -> Result<MetadataMap, GrpcError> {
    let mut map = MetadataMap::new();
    for entry in entries {
        let name = entry.name.to_ascii_lowercase();
        let invalid = |message: &str| GrpcError::Metadata {
            name: entry.name.clone(),
            message: message.to_owned(),
        };
        if is_binary(&name) {
            let key = BinaryMetadataKey::from_bytes(name.as_bytes())
                .map_err(|_| invalid("invalid name"))?;
            let bytes = decode_binary(&entry.name, &entry.value)?;
            map.append_bin(key, BinaryMetadataValue::from_bytes(&bytes));
        } else {
            let key = AsciiMetadataKey::from_bytes(name.as_bytes())
                .map_err(|_| invalid("invalid name"))?;
            let value = AsciiMetadataValue::try_from(entry.value.as_str())
                .map_err(|_| invalid("values must be printable ASCII"))?;
            map.append(key, value);
        }
    }
    Ok(map)
}

/// The entries of `map`, in order, binary values in base64. The
/// pseudo-entries gRPC itself uses for status are kept.
pub fn entries(map: &MetadataMap) -> Vec<MetadataEntry> {
    map.iter()
        .map(|entry| match entry {
            KeyAndValueRef::Ascii(key, value) => MetadataEntry::new(
                key.as_str(),
                String::from_utf8_lossy(value.as_encoded_bytes()),
            ),
            KeyAndValueRef::Binary(key, value) => {
                let bytes = value.to_bytes().map(|b| b.to_vec()).unwrap_or_default();
                MetadataEntry::new(key.as_str(), STANDARD.encode(bytes))
            }
        })
        .collect()
}

/// The entries of an HTTP header map (gRPC-Web), in order; binary values
/// are already base64 on the wire.
pub fn header_entries(headers: &HeaderMap) -> Vec<MetadataEntry> {
    headers
        .iter()
        .map(|(name, value)| {
            MetadataEntry::new(name.as_str(), String::from_utf8_lossy(value.as_bytes()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_values_are_decoded_from_base64() {
        let map = request_metadata(&[
            MetadataEntry::new("trace-bin", "AQID"),
            MetadataEntry::new("x-tenant", "acme"),
        ])
        .unwrap();
        let bin = map.get_bin("trace-bin").unwrap().to_bytes().unwrap();
        assert_eq!(bin.as_ref(), [1, 2, 3]);
        assert_eq!(map.get("x-tenant").unwrap(), "acme");
        let shown = entries(&map);
        assert_eq!(
            shown,
            [
                MetadataEntry::new("trace-bin", "AQID"),
                MetadataEntry::new("x-tenant", "acme"),
            ]
        );
    }

    #[test]
    fn unpadded_base64_is_accepted() {
        assert_eq!(decode_binary("a-bin", "AQI").unwrap(), [1, 2]);
        assert_eq!(decode_binary("a-bin", "AQI=").unwrap(), [1, 2]);
    }

    #[test]
    fn invalid_metadata_names_the_entry_only() {
        let err = request_metadata(&[MetadataEntry::new("k-bin", "s3cr3t!!")]).unwrap_err();
        assert!(
            err.to_string()
                .starts_with("grpc: invalid metadata 'k-bin': binary metadata takes base64"),
            "{err}"
        );
        assert!(!err.to_string().contains("s3cr3t"));
        let err = request_metadata(&[MetadataEntry::new("bad name", "v")]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "grpc: invalid metadata 'bad name': invalid name"
        );
        let err = request_metadata(&[MetadataEntry::new("x", "line\nbreak")]).unwrap_err();
        assert_eq!(err.exit_code(), 2);
    }
}
