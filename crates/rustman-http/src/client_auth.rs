//! Client certificates for mutual TLS (design D3): a PEM certificate chain
//! and key, or a PKCS#12 bundle (`p12-keystore`, pure Rust).
//!
//! PEM is parsed with `rustls-pki-types`; `rustls-pemfile` is deprecated and
//! now only wraps that same parser.

use std::fmt;
use std::hash::{Hash, Hasher};

use p12_keystore::{KeyStore, Pkcs12ImportPolicy};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

use crate::error::RequestError;

/// A certificate chain (leaf first) and its private key.
pub struct ClientAuth {
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
}

impl ClientAuth {
    /// From PEM text: `cert_pem` holds the chain, `key_pem` the private key
    /// (PKCS#8, PKCS#1 or SEC1). They may be the same file.
    pub fn from_pem(cert_pem: &[u8], key_pem: &[u8]) -> Result<Self, RequestError> {
        let chain = CertificateDer::pem_slice_iter(cert_pem)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| invalid("client_cert", e))?;
        if chain.is_empty() {
            return Err(invalid("client_cert", "no certificates found"));
        }
        let key = PrivateKeyDer::from_pem_slice(key_pem).map_err(|e| invalid("client_key", e))?;
        Ok(Self { chain, key })
    }

    /// From a PKCS#12 bundle holding one private key and its chain.
    pub fn from_pkcs12(der: &[u8], password: &str) -> Result<Self, RequestError> {
        let store = KeyStore::from_pkcs12(der, password, Pkcs12ImportPolicy::Strict)
            .map_err(|e| invalid("client_pkcs12", e))?;
        let (_, entry) = store
            .private_key_chain()
            .ok_or_else(|| invalid("client_pkcs12", "no private key with a certificate"))?;
        let chain = entry
            .certs()
            .iter()
            .map(|cert| CertificateDer::from(cert.as_der().to_vec()))
            .collect();
        let key = PrivatePkcs8KeyDer::from(entry.key().as_der().to_vec()).into();
        Ok(Self { chain, key })
    }

    pub(crate) fn chain(&self) -> Vec<CertificateDer<'static>> {
        self.chain.clone()
    }

    pub(crate) fn key(&self) -> PrivateKeyDer<'static> {
        self.key.clone_key()
    }
}

fn invalid(what: &str, err: impl fmt::Display) -> RequestError {
    RequestError::Tls(format!("tls: invalid {what}: {err}"))
}

impl Clone for ClientAuth {
    fn clone(&self) -> Self {
        Self {
            chain: self.chain(),
            key: self.key(),
        }
    }
}

impl PartialEq for ClientAuth {
    fn eq(&self, other: &Self) -> bool {
        self.chain == other.chain && self.key == other.key
    }
}

impl Eq for ClientAuth {}

impl Hash for ClientAuth {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.chain.hash(state);
        self.key.secret_der().hash(state);
    }
}

/// Never prints the key.
impl fmt::Debug for ClientAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientAuth")
            .field("certificates", &self.chain.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pem_without_certificates_is_rejected() {
        let err = ClientAuth::from_pem(b"nothing here", b"").unwrap_err();
        assert_eq!(
            err.to_string(),
            "tls: invalid client_cert: no certificates found"
        );
    }

    #[test]
    fn a_garbage_pkcs12_is_rejected() {
        let err = ClientAuth::from_pkcs12(b"not der", "pw").unwrap_err();
        assert!(
            err.to_string().starts_with("tls: invalid client_pkcs12:"),
            "{err}"
        );
    }
}
