//! `auth: {type: jwt}`: a token signed for every request with
//! `jsonwebtoken` on ring (runner-parity design D6).

use jsonwebtoken::{Algorithm, EncodingKey, Header};
use rustman_core::model::signed_auth::jwt_key_error;
use rustman_core::model::{Jwt, JwtAlgorithm};
use serde_json::{Map, Value};

use crate::error::RequestError;

/// The redaction name of a generated token: `«redacted:jwt»`.
pub(crate) const SECRET_NAME: &str = "jwt";

/// The JOSE header fields `header:` may set.
const HEADER_FIELDS: &[&str] = &[
    "typ", "cty", "jku", "jwk", "kid", "x5u", "x5c", "x5t", "x5t#S256",
];

/// Sign a token for `config` at `now` (seconds since the Unix epoch).
/// `iat` and `exp = iat + ttl` are added unless the claims set them.
pub(crate) fn sign(config: &Jwt, now: u64) -> Result<String, RequestError> {
    let header = header(config)?;
    let claims = claims(config, now);
    let key = key(config)?;
    jsonwebtoken::encode(&header, &claims, &key).map_err(|e| auth(format!("cannot sign: {e}")))
}

/// Seconds since the Unix epoch.
pub(crate) fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn auth(message: String) -> RequestError {
    RequestError::Auth(format!("jwt: {message}"))
}

fn algorithm(algorithm: JwtAlgorithm) -> Algorithm {
    match algorithm {
        JwtAlgorithm::HS256 => Algorithm::HS256,
        JwtAlgorithm::HS384 => Algorithm::HS384,
        JwtAlgorithm::HS512 => Algorithm::HS512,
        JwtAlgorithm::RS256 => Algorithm::RS256,
        JwtAlgorithm::ES256 => Algorithm::ES256,
    }
}

/// `{"typ": "JWT", "alg": ...}` with the configured fields on top.
fn header(config: &Jwt) -> Result<Header, RequestError> {
    let base = Header::new(algorithm(config.algorithm));
    if config.header.is_empty() {
        return Ok(base);
    }
    if let Some(field) = config
        .header
        .keys()
        .find(|k| !HEADER_FIELDS.contains(&k.as_str()))
    {
        return Err(auth(format!("unsupported header field '{field}'")));
    }
    let mut merged = match serde_json::to_value(&base) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    };
    merged.extend(config.header.clone());
    serde_json::from_value(Value::Object(merged)).map_err(|e| auth(format!("invalid header: {e}")))
}

fn claims(config: &Jwt, now: u64) -> Map<String, Value> {
    let mut claims = config.claims.clone();
    let iat = claims.get("iat").and_then(Value::as_u64).unwrap_or(now);
    claims.entry("iat").or_insert(Value::from(now));
    claims
        .entry("exp")
        .or_insert(Value::from(iat.saturating_add(config.ttl_secs)));
    claims
}

fn key(config: &Jwt) -> Result<EncodingKey, RequestError> {
    let Some(kind) = config.algorithm.key_kind() else {
        let secret = config.secret.as_deref().unwrap_or_default();
        if secret.is_empty() {
            return Err(auth(format!(
                "{} secret is empty",
                config.algorithm.as_str()
            )));
        }
        return Ok(EncodingKey::from_secret(secret.as_bytes()));
    };
    let pem = pem(config)?;
    if let Some(message) = jwt_key_error(config.algorithm, &pem) {
        return Err(RequestError::Auth(message));
    }
    let parsed = match kind {
        rustman_core::model::signed_auth::KeyKind::Rsa => EncodingKey::from_rsa_pem(pem.as_bytes()),
        rustman_core::model::signed_auth::KeyKind::Ec => EncodingKey::from_ec_pem(pem.as_bytes()),
    };
    parsed.map_err(|e| {
        auth(format!(
            "invalid {} private key (PEM, PKCS#8 for EC keys): {e}",
            config.algorithm.as_str()
        ))
    })
}

/// The PEM text of `private_key`, or of `private_key_file` (already
/// resolved and contained in the workspace).
fn pem(config: &Jwt) -> Result<String, RequestError> {
    if let Some(pem) = &config.private_key {
        return Ok(pem.clone());
    }
    let file = config
        .private_key_file
        .as_deref()
        .map_or("", String::as_str);
    std::fs::read_to_string(file)
        .map_err(|e| RequestError::Io(format!("jwt: cannot read private_key_file {file}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{DecodingKey, Validation, decode};
    use rustman_core::model::ApiKeyLocation;
    use serde_json::json;

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/jwt");

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!("{FIXTURES}/{name}")).unwrap()
    }

    fn config(algorithm: JwtAlgorithm) -> Jwt {
        Jwt {
            algorithm,
            secret: None,
            private_key: None,
            private_key_file: None,
            claims: json!({"sub": "42", "scope": "read"})
                .as_object()
                .unwrap()
                .clone(),
            header: Map::new(),
            ttl_secs: 300,
            location: ApiKeyLocation::Header,
            name: None,
        }
    }

    fn verify(token: &str, key: &DecodingKey, algorithm: Algorithm) -> Value {
        let mut validation = Validation::new(algorithm);
        validation.validate_exp = false;
        validation.required_spec_claims.clear();
        decode::<Value>(token, key, &validation).unwrap().claims
    }

    #[test]
    fn the_jwt_io_example_signature_is_reproduced() {
        // The HS256 example token of jwt.io: header and payload as given,
        // signed with `your-256-bit-secret`.
        let message = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.\
            eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiaWF0IjoxNTE2MjM5MDIyfQ";
        let key = EncodingKey::from_secret(b"your-256-bit-secret");
        let signature = jsonwebtoken::crypto::sign(message.as_bytes(), &key, Algorithm::HS256);
        assert_eq!(
            signature.unwrap(),
            "SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c"
        );
    }

    #[test]
    fn the_rfc_7515_hs256_example_signature_is_reproduced() {
        // RFC 7515 appendix A.1.
        let message = "eyJ0eXAiOiJKV1QiLA0KICJhbGciOiJIUzI1NiJ9.\
            eyJpc3MiOiJqb2UiLA0KICJleHAiOjEzMDA4MTkzODAsDQogImh0dHA6Ly9leGFtcGxlLmNvbS9pc19yb290Ijp0cnVlfQ";
        let secret = "AyM1SysPpbyDfgZld3umj1qzKObwVMkoqQ-EstJQLr_T-1qS0gZH75aKtMN3Yj0iPS4hcgUuTwjAzZr1Z9CAow";
        let secret =
            base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, secret)
                .unwrap();
        let key = EncodingKey::from_secret(&secret);
        let signature = jsonwebtoken::crypto::sign(message.as_bytes(), &key, Algorithm::HS256);
        assert_eq!(
            signature.unwrap(),
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"
        );
    }

    #[test]
    fn hmac_tokens_verify_with_their_secret() {
        for (alg, jose) in [
            (JwtAlgorithm::HS256, Algorithm::HS256),
            (JwtAlgorithm::HS384, Algorithm::HS384),
            (JwtAlgorithm::HS512, Algorithm::HS512),
        ] {
            let mut config = config(alg);
            config.secret = Some("hmac-secret".into());
            let token = sign(&config, 1_700_000_000).unwrap();
            let claims = verify(&token, &DecodingKey::from_secret(b"hmac-secret"), jose);
            assert_eq!(
                claims,
                json!({"sub": "42", "scope": "read", "iat": 1_700_000_000u64, "exp": 1_700_000_300u64})
            );
        }
    }

    #[test]
    fn rsa_and_ec_tokens_verify_with_the_public_key() {
        let mut rs = config(JwtAlgorithm::RS256);
        rs.private_key = Some(fixture("rsa.pem"));
        let token = sign(&rs, 10).unwrap();
        let public = DecodingKey::from_rsa_pem(fixture("rsa.pub.pem").as_bytes()).unwrap();
        assert_eq!(verify(&token, &public, Algorithm::RS256)["sub"], "42");

        let mut es = config(JwtAlgorithm::ES256);
        es.private_key_file = Some(format!("{FIXTURES}/ec.pem").into());
        let token = sign(&es, 10).unwrap();
        let public = DecodingKey::from_ec_pem(fixture("ec.pub.pem").as_bytes()).unwrap();
        assert_eq!(verify(&token, &public, Algorithm::ES256)["exp"], 310);
    }

    #[test]
    fn explicit_iat_and_exp_are_kept_and_header_fields_added() {
        let mut config = config(JwtAlgorithm::HS256);
        config.secret = Some("s".into());
        config.claims.insert("iat".into(), json!(100));
        config.header.insert("kid".into(), json!("key-1"));
        let token = sign(&config, 5_000).unwrap();
        let header = jsonwebtoken::decode_header(&token).unwrap();
        assert_eq!(header.kid.as_deref(), Some("key-1"));
        assert_eq!(header.typ.as_deref(), Some("JWT"));
        let claims = verify(&token, &DecodingKey::from_secret(b"s"), Algorithm::HS256);
        assert_eq!(
            (claims["iat"].clone(), claims["exp"].clone()),
            (json!(100), json!(400))
        );
    }

    #[test]
    fn an_empty_hmac_secret_is_an_error() {
        for secret in [None, Some(String::new())] {
            let mut hs = config(JwtAlgorithm::HS384);
            hs.secret = secret;
            let err = sign(&hs, 0).unwrap_err();
            assert_eq!(err.to_string(), "jwt: HS384 secret is empty");
        }
    }

    #[test]
    fn bad_keys_and_header_fields_are_reported() {
        let mut rs = config(JwtAlgorithm::RS256);
        rs.private_key = Some(fixture("ec.pem"));
        let err = sign(&rs, 0).unwrap_err();
        assert_eq!(err.to_string(), "jwt: RS256 requires an RSA private key");
        let mut hs = config(JwtAlgorithm::HS256);
        hs.header.insert("foo".into(), json!(1));
        let err = sign(&hs, 0).unwrap_err();
        assert_eq!(err.to_string(), "jwt: unsupported header field 'foo'");
        let mut missing = config(JwtAlgorithm::ES256);
        missing.private_key_file = Some("/no/such/key.pem".to_owned().into());
        let err = sign(&missing, 0).unwrap_err();
        assert!(
            err.to_string()
                .starts_with("jwt: cannot read private_key_file /no/such/key.pem"),
            "{err}"
        );
    }
}
