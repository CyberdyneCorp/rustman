//! `auth: {type: aws_sigv4}`: AWS Signature Version 4 with the `aws-sigv4`
//! crate (runner-parity design D4). Every hop is signed as the last step
//! before it is sent, so redirects are re-signed and workspace default
//! headers are part of `SignedHeaders`.

mod credentials;

use std::borrow::Cow;
use std::sync::{Mutex, PoisonError};
use std::time::SystemTime;

use aws_credential_types::Credentials;
use aws_sigv4::http_request::{
    PayloadChecksumKind, PercentEncodingMode, SignableBody, SignableRequest, SigningSettings,
    UriPathNormalizationMode, sign,
};
use aws_sigv4::sign::v4::SigningParams;
use bytes::Bytes;
use http::header::{AUTHORIZATION, PROXY_AUTHORIZATION};
use http::{HeaderName, HeaderValue, Request};
use http_body_util::Full;
use rustman_core::Secret;
use rustman_core::model::{Auth, AwsSigV4};
use rustman_core::vars::UsedSecret;

use crate::error::RequestError;
use crate::exchange::Exchange;
pub use credentials::{Keys, env_keys, profile_keys};

/// Headers a signature sets; any already on a hop is replaced.
const SIGNATURE_HEADERS: [&str; 4] = [
    "authorization",
    "x-amz-date",
    "x-amz-content-sha256",
    "x-amz-security-token",
];

/// AWS error codes that mean the request time is too far off.
const SKEW_ERRORS: [&str; 2] = ["RequestTimeTooSkewed", "Signature expired"];

/// Signs each hop of one request.
pub(crate) struct Signer {
    region: String,
    service: String,
    keys: Keys,
    unsigned_payload: bool,
    /// The `X-Amz-Date` of the last hop signed.
    signed_at: Mutex<Option<String>>,
}

impl Signer {
    /// The signer for `auth`, with its credentials resolved (`from: env`
    /// and `from: profile` read the process environment and `~/.aws`).
    pub fn for_auth(auth: Option<&Auth>) -> Result<Option<Self>, RequestError> {
        let Some(Auth::AwsSigV4(config)) = auth else {
            return Ok(None);
        };
        Self::new(config, &|name| std::env::var(name).ok()).map(Some)
    }

    fn new(config: &AwsSigV4, env: &dyn Fn(&str) -> Option<String>) -> Result<Self, RequestError> {
        Ok(Self {
            region: config.region.clone(),
            service: config.service.clone(),
            keys: credentials::resolve(&config.credentials, env)?,
            unsigned_payload: config.unsigned_payload,
            signed_at: Mutex::new(None),
        })
    }

    /// The secret key and session token, for every sink to redact.
    pub fn secrets(&self) -> Vec<UsedSecret> {
        let mut secrets = vec![Secret::new("aws_secret_key", self.keys.secret_key.clone())];
        if let Some(token) = &self.keys.session_token {
            secrets.push(Secret::new("aws_session_token", token.clone()));
        }
        secrets
    }

    /// Sign `request` at `time`, replacing any earlier signature.
    pub fn sign(
        &self,
        request: &mut Request<Full<Bytes>>,
        time: SystemTime,
    ) -> Result<(), RequestError> {
        for name in SIGNATURE_HEADERS {
            request.headers_mut().remove(name);
        }
        let headers = header_pairs(request)?;
        let body = request.body().clone();
        let bytes = http_body_util::BodyExt::collect(body);
        let bytes = futures_util::FutureExt::now_or_never(bytes)
            .and_then(Result::ok)
            .map(|b| b.to_bytes())
            .unwrap_or_default();
        let payload = if self.unsigned_payload {
            SignableBody::UnsignedPayload
        } else {
            SignableBody::Bytes(&bytes)
        };
        let parts = Parts {
            method: request.method().as_str(),
            uri: &request.uri().to_string(),
            headers: &headers,
            body: payload,
        };
        let settings = self.settings();
        let signed = signed_headers(&parts, &self.keys, self.scope(), settings, time)?;
        for (name, value) in signed.headers {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|e| RequestError::Invalid(format!("aws_sigv4: {e}")))?;
            let value = HeaderValue::from_str(&value)
                .map_err(|e| RequestError::Invalid(format!("aws_sigv4: {e}")))?;
            request.headers_mut().insert(name, value);
        }
        let date = request
            .headers()
            .get("x-amz-date")
            .and_then(|v| v.to_str().ok());
        *self
            .signed_at
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = date.map(str::to_owned);
        Ok(())
    }

    fn scope(&self) -> Scope<'_> {
        Scope {
            region: &self.region,
            service: &self.service,
        }
    }

    /// S3 signs the path as sent (single encoding, no normalization), every
    /// other service as the SigV4 default does. `X-Amz-Content-Sha256` is
    /// always sent; `Proxy-Authorization` is never signed.
    fn settings(&self) -> SigningSettings {
        let s3 = self.service == "s3";
        let mut settings = SigningSettings::default();
        settings.payload_checksum_kind = PayloadChecksumKind::XAmzSha256;
        if s3 {
            settings.percent_encoding_mode = PercentEncodingMode::Single;
            settings.uri_path_normalization_mode = UriPathNormalizationMode::Disabled;
        }
        let excluded = settings.excluded_headers.get_or_insert_with(Vec::new);
        excluded.push(Cow::Borrowed(PROXY_AUTHORIZATION.as_str()));
        settings
    }

    /// The clock-skew hint for an exchange AWS rejected because of the
    /// request time.
    pub fn skew_hint(&self, exchange: &Exchange) -> Option<String> {
        if exchange.status < 400 {
            return None;
        }
        let body = String::from_utf8_lossy(&exchange.body);
        if !SKEW_ERRORS.iter().any(|e| body.contains(e)) {
            return None;
        }
        let at = self
            .signed_at
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let at = at.as_deref().unwrap_or("an unknown time");
        Some(format!(
            "check the system clock; request was signed at {at}"
        ))
    }
}

/// The name/value pairs of `request`'s headers; a non-UTF-8 value cannot
/// be signed.
fn header_pairs(request: &Request<Full<Bytes>>) -> Result<Vec<(String, String)>, RequestError> {
    request
        .headers()
        .iter()
        .filter(|(name, _)| **name != AUTHORIZATION)
        .map(|(name, value)| {
            let value = value.to_str().map_err(|_| {
                RequestError::Invalid(format!("aws_sigv4: header '{name}' is not valid text"))
            })?;
            Ok((name.as_str().to_owned(), value.to_owned()))
        })
        .collect()
}

/// The request as signed: the URI with scheme and authority (which give
/// `Host`), headers and payload.
pub(crate) struct Parts<'a> {
    pub method: &'a str,
    pub uri: &'a str,
    pub headers: &'a [(String, String)],
    pub body: SignableBody<'a>,
}

#[derive(Clone, Copy)]
pub(crate) struct Scope<'a> {
    pub region: &'a str,
    pub service: &'a str,
}

/// The headers a signature adds, and the signature itself.
pub(crate) struct Signed {
    pub headers: Vec<(String, String)>,
    /// Checked against the AWS test suite; the header carries it on the wire.
    #[cfg_attr(not(test), allow(dead_code))]
    pub signature: String,
}

/// Sign `parts` with the AWS signer.
pub(crate) fn signed_headers(
    parts: &Parts<'_>,
    keys: &Keys,
    scope: Scope<'_>,
    settings: SigningSettings,
    time: SystemTime,
) -> Result<Signed, RequestError> {
    let failed = |e: &dyn std::fmt::Display| RequestError::Invalid(format!("aws_sigv4: {e}"));
    let credentials = Credentials::new(
        keys.access_key.clone(),
        keys.secret_key.clone(),
        keys.session_token.clone(),
        None,
        "rustman",
    );
    let identity = credentials.into();
    let params = SigningParams::builder()
        .identity(&identity)
        .region(scope.region)
        .name(scope.service)
        .time(time)
        .settings(settings)
        .build()
        .map_err(|e| failed(&e))?
        .into();
    let headers = parts.headers.iter().map(|(n, v)| (n.as_str(), v.as_str()));
    let body = parts.body.clone();
    let request =
        SignableRequest::new(parts.method, parts.uri, headers, body).map_err(|e| failed(&e))?;
    let output = sign(request, &params).map_err(|e| failed(&e))?;
    let (instructions, signature) = output.into_parts();
    let (headers, _) = instructions.into_parts();
    let headers = headers
        .into_iter()
        .map(|h| (h.name().to_owned(), h.value().to_owned()))
        .collect();
    Ok(Signed { headers, signature })
}

/// The settings of an AWS test-suite case. The CRT suite encodes paths
/// once, as it gives them.
#[cfg(test)]
pub(crate) fn suite_settings(
    normalize: bool,
    sign_body: bool,
    omit_token: bool,
) -> SigningSettings {
    use aws_sigv4::http_request::SessionTokenMode;
    let mut settings = SigningSettings::default();
    settings.percent_encoding_mode = PercentEncodingMode::Single;
    settings.uri_path_normalization_mode = if normalize {
        UriPathNormalizationMode::Enabled
    } else {
        UriPathNormalizationMode::Disabled
    };
    settings.payload_checksum_kind = if sign_body {
        PayloadChecksumKind::XAmzSha256
    } else {
        PayloadChecksumKind::NoHeader
    };
    settings.session_token_mode = if omit_token {
        SessionTokenMode::Exclude
    } else {
        SessionTokenMode::Include
    };
    settings
}

#[cfg(test)]
mod suite;
