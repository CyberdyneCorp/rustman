//! Signature auth: `aws_sigv4` and `jwt` (runner-parity design D4 and D6).
//!
//! Their credentials are computed per request by `rustman-http`; this module
//! only holds the configuration and the checks that can run when the
//! workspace loads (field combinations, `ttl`, and the JWT key type).

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::auth::ApiKeyLocation;
use crate::format::Located;
use crate::secret::Hidden;

/// The JWT lifetime when `ttl` is not set: five minutes.
pub const DEFAULT_JWT_TTL_SECS: u64 = 300;

/// `auth: {type: aws_sigv4}`.
#[derive(Clone, PartialEq)]
pub struct AwsSigV4 {
    pub region: String,
    pub service: String,
    pub credentials: AwsCredentials,
    /// Send `UNSIGNED-PAYLOAD` instead of the body's SHA-256.
    pub unsigned_payload: bool,
}

/// Where SigV4 credentials come from.
#[derive(Clone, PartialEq)]
pub enum AwsCredentials {
    /// `access_key`, `secret_key` and optional `session_token` fields.
    Fields {
        access_key: String,
        secret_key: String,
        session_token: Option<String>,
    },
    /// `from: env`: `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` and
    /// `AWS_SESSION_TOKEN`.
    Env,
    /// `from: profile`: static keys of a profile in `~/.aws/credentials`.
    Profile(String),
}

/// The `from:` field of `aws_sigv4`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AwsCredentialSource {
    Env,
    Profile,
}

impl fmt::Debug for AwsSigV4 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AwsSigV4")
            .field("region", &self.region)
            .field("service", &self.service)
            .field("credentials", &self.credentials)
            .field("unsigned_payload", &self.unsigned_payload)
            .finish()
    }
}

impl fmt::Debug for AwsCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fields {
                access_key,
                session_token,
                ..
            } => f
                .debug_struct("Fields")
                .field("access_key", access_key)
                .field("secret_key", &Hidden("secret_key"))
                .field(
                    "session_token",
                    &session_token.as_ref().map(|_| Hidden("session_token")),
                )
                .finish(),
            Self::Env => f.write_str("Env"),
            Self::Profile(name) => f.debug_tuple("Profile").field(name).finish(),
        }
    }
}

/// A JWT signing algorithm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub enum JwtAlgorithm {
    HS256,
    HS384,
    HS512,
    RS256,
    ES256,
}

/// The kind of private key an asymmetric algorithm needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyKind {
    Rsa,
    Ec,
}

impl JwtAlgorithm {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HS256 => "HS256",
            Self::HS384 => "HS384",
            Self::HS512 => "HS512",
            Self::RS256 => "RS256",
            Self::ES256 => "ES256",
        }
    }

    /// The private key kind it signs with; `None` for the HMAC algorithms,
    /// which take a `secret`.
    pub fn key_kind(self) -> Option<KeyKind> {
        match self {
            Self::HS256 | Self::HS384 | Self::HS512 => None,
            Self::RS256 => Some(KeyKind::Rsa),
            Self::ES256 => Some(KeyKind::Ec),
        }
    }
}

/// `auth: {type: jwt}`: a token signed per request.
#[derive(Clone, PartialEq)]
pub struct Jwt {
    pub algorithm: JwtAlgorithm,
    /// The HMAC secret (HS256/384/512).
    pub secret: Option<String>,
    /// A PEM private key (RS256, ES256), usually from a variable.
    pub private_key: Option<String>,
    /// A PEM private key file, relative to the workspace root.
    pub private_key_file: Option<Located<String>>,
    /// The token's claims; strings are interpolated.
    pub claims: Map<String, Value>,
    /// Extra JOSE header fields, such as `kid`.
    pub header: Map<String, Value>,
    /// `exp = iat + ttl`, in seconds.
    pub ttl_secs: u64,
    /// `in: header` (default) or `in: query`.
    pub location: ApiKeyLocation,
    /// The header (default `Authorization`, as `Bearer <jwt>`) or query
    /// parameter that carries the token.
    pub name: Option<String>,
}

impl fmt::Debug for Jwt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Jwt")
            .field("algorithm", &self.algorithm)
            .field("secret", &self.secret.as_ref().map(|_| Hidden("secret")))
            .field(
                "private_key",
                &self.private_key.as_ref().map(|_| Hidden("private_key")),
            )
            .field("private_key_file", &self.private_key_file)
            .field("claims", &self.claims.keys().collect::<Vec<_>>())
            .field("header", &self.header)
            .field("ttl_secs", &self.ttl_secs)
            .field("location", &self.location)
            .field("name", &self.name)
            .finish()
    }
}

/// Parse a `ttl` such as `30s`, `5m`, `1h` or `90` (seconds).
pub fn parse_ttl(text: &str) -> Result<u64, String> {
    let text = text.trim();
    let invalid = || format!("jwt: invalid ttl '{text}' (expected e.g. 30s, 5m or 1h)");
    let (digits, unit) = match text.char_indices().find(|(_, c)| !c.is_ascii_digit()) {
        Some((at, _)) => text.split_at(at),
        None => (text, "s"),
    };
    let scale = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        _ => return Err(invalid()),
    };
    let value: u64 = digits.parse().map_err(|_| invalid())?;
    value
        .checked_mul(scale)
        .filter(|secs| *secs > 0)
        .ok_or_else(invalid)
}

/// The JWT `claims` or `header` mapping, or the error naming `field`.
pub(crate) fn json_object(field: &str, value: Option<Value>) -> Result<Map<String, Value>, String> {
    match value {
        None => Ok(Map::new()),
        Some(Value::Object(map)) => Ok(map),
        Some(_) => Err(format!("jwt: '{field}' must be a mapping")),
    }
}

/// Why `pem` cannot sign with `algorithm`, if it cannot: an RSA key for
/// `RS256`, an EC key for `ES256`.
pub fn jwt_key_error(algorithm: JwtAlgorithm, pem: &str) -> Option<String> {
    let wanted = algorithm.key_kind()?;
    if pem_key_kind(pem) == Some(wanted) {
        return None;
    }
    let article = match wanted {
        KeyKind::Rsa => "an RSA",
        KeyKind::Ec => "an EC",
    };
    Some(format!(
        "jwt: {} requires {article} private key",
        algorithm.as_str()
    ))
}

/// The kind of the private key in PEM text, from its label or, for PKCS#8,
/// from the algorithm identifier it holds.
pub fn pem_key_kind(pem: &str) -> Option<KeyKind> {
    if pem.contains("-----BEGIN RSA PRIVATE KEY-----") {
        return Some(KeyKind::Rsa);
    }
    if pem.contains("-----BEGIN EC PRIVATE KEY-----") {
        return Some(KeyKind::Ec);
    }
    let der = pkcs8_der(pem)?;
    const RSA_ENCRYPTION: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x01];
    const EC_PUBLIC_KEY: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x02, 0x01];
    let holds = |oid: &[u8]| der.windows(oid.len()).any(|w| w == oid);
    if holds(RSA_ENCRYPTION) {
        Some(KeyKind::Rsa)
    } else if holds(EC_PUBLIC_KEY) {
        Some(KeyKind::Ec)
    } else {
        None
    }
}

/// The DER bytes of a `PRIVATE KEY` (PKCS#8) PEM block.
fn pkcs8_der(pem: &str) -> Option<Vec<u8>> {
    let (_, rest) = pem.split_once("-----BEGIN PRIVATE KEY-----")?;
    let (body, _) = rest.split_once("-----END PRIVATE KEY-----")?;
    let base64: String = body.split_whitespace().collect();
    STANDARD.decode(base64).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ttls_take_a_unit_or_seconds() {
        assert_eq!(parse_ttl("5m"), Ok(300));
        assert_eq!(parse_ttl("30s"), Ok(30));
        assert_eq!(parse_ttl("1h"), Ok(3600));
        assert_eq!(parse_ttl("90"), Ok(90));
        for bad in ["", "0s", "5d", "m", "-1m"] {
            assert_eq!(
                parse_ttl(bad).unwrap_err(),
                format!("jwt: invalid ttl '{bad}' (expected e.g. 30s, 5m or 1h)")
            );
        }
    }

    #[test]
    fn key_kinds_come_from_the_label_or_the_pkcs8_oid() {
        let rsa = "-----BEGIN RSA PRIVATE KEY-----\nAAAA\n-----END RSA PRIVATE KEY-----";
        let ec = "-----BEGIN EC PRIVATE KEY-----\nAAAA\n-----END EC PRIVATE KEY-----";
        assert_eq!(pem_key_kind(rsa), Some(KeyKind::Rsa));
        assert_eq!(pem_key_kind(ec), Some(KeyKind::Ec));
        let pkcs8 = |oid: &[u8]| {
            let mut der = vec![0x30, 0x10, 0x06, oid.len() as u8];
            der.extend_from_slice(oid);
            let body = STANDARD.encode(der);
            format!("-----BEGIN PRIVATE KEY-----\n{body}\n-----END PRIVATE KEY-----\n")
        };
        let rsa_oid = [0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x01];
        let ec_oid = [0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x02, 0x01];
        assert_eq!(pem_key_kind(&pkcs8(&rsa_oid)), Some(KeyKind::Rsa));
        assert_eq!(pem_key_kind(&pkcs8(&ec_oid)), Some(KeyKind::Ec));
        assert_eq!(pem_key_kind(&pkcs8(&[0x2B, 0x65, 0x70])), None);
        assert_eq!(pem_key_kind("not a key"), None);
    }

    #[test]
    fn mismatched_keys_are_named() {
        let ec = "-----BEGIN EC PRIVATE KEY-----\nAAAA\n-----END EC PRIVATE KEY-----";
        assert_eq!(
            jwt_key_error(JwtAlgorithm::RS256, ec).unwrap(),
            "jwt: RS256 requires an RSA private key"
        );
        let rsa = ec.replace("EC", "RSA");
        assert_eq!(
            jwt_key_error(JwtAlgorithm::ES256, &rsa).unwrap(),
            "jwt: ES256 requires an EC private key"
        );
        assert_eq!(jwt_key_error(JwtAlgorithm::ES256, ec), None);
        assert_eq!(jwt_key_error(JwtAlgorithm::HS256, "anything"), None);
    }

    #[test]
    fn debug_hides_keys_and_secrets() {
        let sigv4 = AwsSigV4 {
            region: "us-east-1".into(),
            service: "s3".into(),
            credentials: AwsCredentials::Fields {
                access_key: "AKID".into(),
                secret_key: "sk-x".into(),
                session_token: Some("st-x".into()),
            },
            unsigned_payload: false,
        };
        let jwt = Jwt {
            algorithm: JwtAlgorithm::HS256,
            secret: Some("hs-x".into()),
            private_key: Some("pk-x".into()),
            private_key_file: None,
            claims: Map::new(),
            header: Map::new(),
            ttl_secs: 300,
            location: ApiKeyLocation::Header,
            name: None,
        };
        let debug = format!("{sigv4:?} {jwt:?}");
        for hidden in ["sk-x", "st-x", "hs-x", "pk-x"] {
            assert!(!debug.contains(hidden), "{debug}");
        }
        assert!(debug.contains("AKID"), "{debug}");
    }
}
