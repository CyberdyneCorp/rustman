//! Protocol version negotiation (mcp-client spec "Initialization and
//! version negotiation"): Rustman offers the latest revision that has an
//! `initialize` handshake and accepts the server's answer when it is that
//! one or an earlier supported revision.

use rmcp::model::ProtocolVersion;

use super::error::ClientError;

/// The version sent in `initialize`.
pub const LATEST: ProtocolVersion = ProtocolVersion::V_2025_11_25;

/// Every revision a session may use, oldest first.
pub const SUPPORTED: &[ProtocolVersion] = &[
    ProtocolVersion::V_2024_11_05,
    ProtocolVersion::V_2025_03_26,
    ProtocolVersion::V_2025_06_18,
    ProtocolVersion::V_2025_11_25,
];

/// Accept the version the server answered, or fail with
/// `mcp: unsupported protocol version <v>`.
pub fn negotiate(answered: &ProtocolVersion) -> Result<ProtocolVersion, ClientError> {
    if SUPPORTED.contains(answered) {
        Ok(answered.clone())
    } else {
        Err(ClientError::UnsupportedVersion(answered.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_supported_versions_are_used() {
        let older = ProtocolVersion::V_2025_03_26;
        assert_eq!(negotiate(&older).unwrap(), older);
        assert_eq!(negotiate(&LATEST).unwrap(), LATEST);
    }

    #[test]
    fn unknown_versions_fail() {
        let unknown: ProtocolVersion = serde_json::from_str("\"1999-01-01\"").unwrap();
        let err = negotiate(&unknown).unwrap_err();
        assert_eq!(
            err.to_string(),
            "mcp: unsupported protocol version 1999-01-01"
        );
        let err = negotiate(&ProtocolVersion::V_2026_07_28).unwrap_err();
        assert_eq!(
            err.to_string(),
            "mcp: unsupported protocol version 2026-07-28"
        );
    }
}
