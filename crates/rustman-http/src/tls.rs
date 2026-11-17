//! `TlsSettings` → `rustls::ClientConfig` (design D3), shared by every
//! protocol that speaks TLS.
//!
//! Verification uses the OS trust store through `rustls-platform-verifier`.
//! With a `ca_bundle`, a `WebPkiServerVerifier` trusts the bundle plus the
//! platform roots. With verification off, any certificate is accepted (the
//! handshake signatures are still checked) and the exchange is flagged
//! `insecure: true` by the caller. A [`ClientAuth`] adds a client certificate
//! for mutual TLS.

use std::fmt;
use std::sync::Arc;

use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, ServerName, UnixTime};
use rustman_core::vars::{ResolvedFile, TlsFiles};

use crate::client_auth::ClientAuth;
use crate::error::RequestError;

/// ALPN for HTTP: HTTP/2 first, HTTP/1.1 as the fallback.
pub const ALPN_HTTP: &[&[u8]] = &[b"h2", b"http/1.1"];

/// How a client verifies servers. Hashable, so transports can be cached per
/// distinct setting.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct TlsSettings {
    /// `false` accepts any certificate (`verify_tls: false`, `--insecure`).
    pub verify: bool,
    /// Extra trusted CAs, PEM encoded.
    pub ca_bundle: Option<Vec<u8>>,
    /// The certificate presented when the server asks for one.
    pub client_auth: Option<ClientAuth>,
}

impl Default for TlsSettings {
    fn default() -> Self {
        Self {
            verify: true,
            ca_bundle: None,
            client_auth: None,
        }
    }
}

impl fmt::Debug for TlsSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TlsSettings")
            .field("verify", &self.verify)
            .field("ca_bundle", &self.ca_bundle.as_ref().map(Vec::len))
            .field("client_auth", &self.client_auth)
            .finish()
    }
}

impl TlsSettings {
    /// Settings for a request: `verify`, its `ca_bundle` and its client
    /// certificate (PEM files, or PKCS#12 opened with `pkcs12_password`).
    pub fn load(
        files: &TlsFiles,
        pkcs12_password: Option<&str>,
        verify: bool,
    ) -> Result<Self, RequestError> {
        let ca_bundle = files
            .ca_bundle
            .as_ref()
            .map(|file| read(file, "ca_bundle"))
            .transpose()?;
        let client_auth = load_client_auth(files, pkcs12_password)?;
        Ok(Self {
            verify,
            ca_bundle,
            client_auth,
        })
    }

    /// A client config with the given ALPN protocols, using the `ring` provider.
    pub fn client_config(&self, alpn: &[&[u8]]) -> Result<ClientConfig, RequestError> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let verifier = self.verifier(&provider)?;
        let config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(tls_error)?
            .dangerous()
            .with_custom_certificate_verifier(verifier);
        let mut config = match &self.client_auth {
            None => config.with_no_client_auth(),
            Some(auth) => config
                .with_client_auth_cert(auth.chain(), auth.key())
                .map_err(|e| RequestError::Tls(format!("tls: invalid client certificate: {e}")))?,
        };
        config.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
        Ok(config)
    }

    fn verifier(
        &self,
        provider: &Arc<CryptoProvider>,
    ) -> Result<Arc<dyn ServerCertVerifier>, RequestError> {
        if !self.verify {
            return Ok(Arc::new(AcceptAnyCertificate(
                provider.signature_verification_algorithms,
            )));
        }
        match &self.ca_bundle {
            None => rustls_platform_verifier::Verifier::new(provider.clone())
                .map(|v| Arc::new(v) as Arc<dyn ServerCertVerifier>)
                .map_err(tls_error),
            Some(pem) => bundle_verifier(pem, provider),
        }
    }
}

/// The client certificate named by `files`, if any.
fn load_client_auth(
    files: &TlsFiles,
    pkcs12_password: Option<&str>,
) -> Result<Option<ClientAuth>, RequestError> {
    let pem = (&files.client_cert, &files.client_key);
    match (pem, &files.client_pkcs12) {
        ((None, None), None) => Ok(None),
        ((None, None), Some(bundle)) => {
            let der = read(bundle, "client_pkcs12")?;
            ClientAuth::from_pkcs12(&der, pkcs12_password.unwrap_or_default()).map(Some)
        }
        ((Some(cert), key), None) => {
            let cert_pem = read(cert, "client_cert")?;
            // Without `client_key`, the key is read from the certificate file.
            let key_pem = match key {
                Some(key) => read(key, "client_key")?,
                None => cert_pem.clone(),
            };
            ClientAuth::from_pem(&cert_pem, &key_pem).map(Some)
        }
        ((None, Some(_)), None) => Err(RequestError::Tls(
            "tls: client_key is set without client_cert".into(),
        )),
        (_, Some(_)) => Err(RequestError::Tls(
            "tls: set client_cert/client_key or client_pkcs12, not both".into(),
        )),
    }
}

fn read(file: &ResolvedFile, what: &str) -> Result<Vec<u8>, RequestError> {
    std::fs::read(&file.path)
        .map_err(|e| RequestError::Io(format!("tls: cannot read {what} {}: {e}", file.raw)))
}

/// The bundle's CAs plus the platform roots, verified by webpki.
fn bundle_verifier(
    pem: &[u8],
    provider: &Arc<CryptoProvider>,
) -> Result<Arc<dyn ServerCertVerifier>, RequestError> {
    let mut roots = RootCertStore::empty();
    let bundle = CertificateDer::pem_slice_iter(pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| RequestError::Tls(format!("tls: invalid ca_bundle: {e}")))?;
    if bundle.is_empty() {
        return Err(RequestError::Tls(
            "tls: invalid ca_bundle: no certificates found".into(),
        ));
    }
    for cert in bundle {
        roots
            .add(cert)
            .map_err(|e| RequestError::Tls(format!("tls: invalid ca_bundle: {e}")))?;
    }
    // Platform roots that fail to parse are skipped, as browsers do.
    roots.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
    WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone())
        .build()
        .map(|v| v as Arc<dyn ServerCertVerifier>)
        .map_err(tls_error)
}

fn tls_error(err: impl fmt::Display) -> RequestError {
    RequestError::Tls(format!("tls: {err}"))
}

/// Accepts any server certificate; handshake signatures are still verified.
#[derive(Debug)]
struct AcceptAnyCertificate(WebPkiSupportedAlgorithms);

impl ServerCertVerifier for AcceptAnyCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.0)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_verifies_with_the_platform_store() {
        let config = TlsSettings::default().client_config(ALPN_HTTP).unwrap();
        assert_eq!(
            config.alpn_protocols,
            [b"h2".to_vec(), b"http/1.1".to_vec()]
        );
    }

    #[test]
    fn insecure_builds_without_roots() {
        let settings = TlsSettings {
            verify: false,
            ..TlsSettings::default()
        };
        let config = settings.client_config(&[b"http/1.1"]).unwrap();
        assert_eq!(config.alpn_protocols, [b"http/1.1".to_vec()]);
    }

    #[test]
    fn an_empty_bundle_is_rejected() {
        let settings = TlsSettings {
            ca_bundle: Some(b"not a certificate".to_vec()),
            ..TlsSettings::default()
        };
        let err = settings.client_config(ALPN_HTTP).unwrap_err();
        assert_eq!(
            err.to_string(),
            "tls: invalid ca_bundle: no certificates found"
        );
    }

    #[test]
    fn debug_shows_the_bundle_size_only() {
        let settings = TlsSettings {
            ca_bundle: Some(vec![1, 2, 3]),
            ..TlsSettings::default()
        };
        assert_eq!(
            format!("{settings:?}"),
            "TlsSettings { verify: true, ca_bundle: Some(3), client_auth: None }"
        );
    }

    fn file(raw: &str) -> Option<ResolvedFile> {
        Some(ResolvedFile {
            raw: raw.into(),
            path: std::path::PathBuf::from("/nonexistent/rustman").join(raw),
        })
    }

    #[test]
    fn client_files_must_be_consistent() {
        let key_only = TlsFiles {
            client_key: file("k.pem"),
            ..TlsFiles::default()
        };
        let err = TlsSettings::load(&key_only, None, true).unwrap_err();
        assert_eq!(
            err.to_string(),
            "tls: client_key is set without client_cert"
        );
        let both = TlsFiles {
            client_cert: file("c.pem"),
            client_pkcs12: file("c.p12"),
            ..TlsFiles::default()
        };
        let err = TlsSettings::load(&both, None, true).unwrap_err();
        assert_eq!(
            err.to_string(),
            "tls: set client_cert/client_key or client_pkcs12, not both"
        );
    }

    #[test]
    fn a_missing_file_names_the_setting() {
        let files = TlsFiles {
            client_pkcs12: file("id.p12"),
            ..TlsFiles::default()
        };
        let err = TlsSettings::load(&files, Some("pw"), true).unwrap_err();
        assert!(matches!(err, RequestError::Io(_)), "{err:?}");
        assert!(
            err.to_string()
                .starts_with("tls: cannot read client_pkcs12 id.p12:"),
            "{err}"
        );
    }
}
