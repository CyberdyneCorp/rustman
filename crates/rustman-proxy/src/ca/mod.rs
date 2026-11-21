//! The per-workspace local CA (spec: Local CA generation and storage, CA is
//! never trusted automatically, CA rotation; design D2, D3).
//!
//! The CA is ECDSA P-256, `CA:true, pathlen:0`, `keyCertSign`, extended key
//! usage `serverAuth`, valid for one year. Its private key lives only in
//! the OS keychain ([`store`]); leaf certificates are minted in memory
//! ([`leaf`]); trust stores are changed only on explicit commands
//! ([`trust`]).

pub mod leaf;
pub mod store;
pub mod trust;

use std::fmt;
use std::path::PathBuf;

use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    KeyPair, KeyUsagePurpose, PKCS_ECDSA_P256_SHA256, PublicKeyData, SerialNumber,
};
use ring::digest;
use ring::rand::{SecureRandom, SystemRandom};
use rustls_pki_types::CertificateDer;
use rustls_pki_types::pem::PemObject;
use time::{Duration, OffsetDateTime};
use zeroize::Zeroizing;

/// How long the CA is valid.
pub const CA_VALIDITY: Duration = Duration::days(365);

/// How far back validity starts, for clocks slightly behind.
pub(crate) const BACKDATE: Duration = Duration::hours(1);

/// Why a CA operation failed. Messages never contain key material.
#[derive(Debug, thiserror::Error)]
pub enum CaError {
    #[error("record: a keychain is required to store the CA private key")]
    NoKeychain,
    #[error("record: keychain error: {0}")]
    Keychain(String),
    #[error(
        "record: this workspace already has a local CA (SHA-256 {0}); \
         use 'rustman record ca rotate' to replace it"
    )]
    AlreadyExists(String),
    #[error("record: this workspace has no local CA; run 'rustman record ca init'")]
    NotFound,
    #[error(
        "record: the local CA certificate and its keychain key do not match; \
         run 'rustman record ca rotate'"
    )]
    Mismatch,
    #[error("record: invalid local CA: {0}")]
    Invalid(String),
    #[error("record: no user config directory (set HOME, or APPDATA on Windows)")]
    NoConfigDir,
    #[error(
        "record: installing the CA is not supported on this system; trust {} manually",
        .0.display()
    )]
    Unsupported(PathBuf),
    #[error("record: `{command}` failed: {reason}")]
    Command { command: String, reason: String },
    #[error("record: {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl CaError {
    fn invalid(err: impl fmt::Display) -> Self {
        Self::Invalid(err.to_string())
    }
}

/// What `ca install`, `ca export` and `ca rotate` print about a CA.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaInfo {
    /// `CN=..., O=...`.
    pub subject: String,
    /// SHA-256 of the DER certificate, `AA:BB:...`.
    pub sha256: String,
    /// SHA-1 of the DER certificate in hex, as trust store tools name it.
    pub sha1: String,
    pub not_after: OffsetDateTime,
}

impl CaInfo {
    /// The expiry date, `YYYY-MM-DD` (UTC).
    pub fn expires(&self) -> String {
        self.not_after.date().to_string()
    }
}

/// A local CA: its certificate and, kept in memory only, its private key.
pub struct LocalCa {
    cert_pem: String,
    cert_der: CertificateDer<'static>,
    key_pem: Zeroizing<String>,
}

impl fmt::Debug for LocalCa {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalCa")
            .field("cert_pem", &self.cert_pem.len())
            .finish_non_exhaustive()
    }
}

impl LocalCa {
    /// A new CA named `common_name`.
    pub fn generate(common_name: &str) -> Result<Self, CaError> {
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).map_err(CaError::invalid)?;
        let cert = ca_params(common_name)?
            .self_signed(&key)
            .map_err(CaError::invalid)?;
        Ok(Self {
            cert_pem: cert.pem(),
            cert_der: cert.der().clone(),
            key_pem: Zeroizing::new(key.serialize_pem()),
        })
    }

    /// The CA stored as `cert_pem` with its key `key_pem`, which must match.
    pub fn from_pem(cert_pem: &str, key_pem: &str) -> Result<Self, CaError> {
        let cert_der = parse_certificate(cert_pem)?;
        let key = KeyPair::from_pem(key_pem).map_err(CaError::invalid)?;
        if key.subject_public_key_info() != spki(&cert_der)? {
            return Err(CaError::Mismatch);
        }
        Ok(Self {
            cert_pem: cert_pem.to_owned(),
            cert_der,
            key_pem: Zeroizing::new(key_pem.to_owned()),
        })
    }

    /// The public certificate, PEM encoded.
    pub fn cert_pem(&self) -> &str {
        &self.cert_pem
    }

    pub fn cert_der(&self) -> &CertificateDer<'static> {
        &self.cert_der
    }

    /// The private key, PEM encoded; written only to the keychain.
    pub(crate) fn key_pem(&self) -> &str {
        &self.key_pem
    }

    pub fn info(&self) -> Result<CaInfo, CaError> {
        info(&self.cert_der)
    }
}

/// The parameters of a new CA certificate.
fn ca_params(common_name: &str) -> Result<CertificateParams, CaError> {
    let mut params = CertificateParams::new(Vec::<String>::new()).map_err(CaError::invalid)?;
    let mut name = DistinguishedName::new();
    name.push(DnType::CommonName, common_name);
    name.push(DnType::OrganizationName, "Rustman");
    params.distinguished_name = name;
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let now = OffsetDateTime::now_utc();
    params.not_before = now - BACKDATE;
    params.not_after = now + CA_VALIDITY;
    params.serial_number = Some(random_serial()?);
    Ok(params)
}

/// A positive random 128-bit serial number.
pub(crate) fn random_serial() -> Result<SerialNumber, CaError> {
    let mut bytes = [0u8; 16];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| CaError::Invalid("no random numbers".into()))?;
    bytes[0] &= 0x7f;
    Ok(SerialNumber::from_slice(&bytes))
}

/// The single certificate in `pem`.
fn parse_certificate(pem: &str) -> Result<CertificateDer<'static>, CaError> {
    CertificateDer::from_pem_slice(pem.as_bytes()).map_err(CaError::invalid)
}

fn spki(der: &CertificateDer<'_>) -> Result<Vec<u8>, CaError> {
    let (_, cert) = x509_parser::parse_x509_certificate(der).map_err(CaError::invalid)?;
    Ok(cert.public_key().raw.to_vec())
}

/// Subject, fingerprints and expiry of the certificate in `pem`.
pub fn info_from_pem(pem: &str) -> Result<CaInfo, CaError> {
    info(&parse_certificate(pem)?)
}

fn info(der: &CertificateDer<'_>) -> Result<CaInfo, CaError> {
    let (_, cert) = x509_parser::parse_x509_certificate(der).map_err(CaError::invalid)?;
    Ok(CaInfo {
        subject: cert.subject().to_string(),
        sha256: fingerprint(digest::digest(&digest::SHA256, der).as_ref(), ":"),
        sha1: fingerprint(
            digest::digest(&digest::SHA1_FOR_LEGACY_USE_ONLY, der).as_ref(),
            "",
        ),
        not_after: cert.validity().not_after.to_datetime(),
    })
}

/// Upper-case hex bytes joined by `separator`.
fn fingerprint(bytes: &[u8], separator: &str) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(separator)
}

#[cfg(test)]
mod tests {
    use x509_parser::extensions::ParsedExtension;

    use super::*;

    fn parsed(ca: &LocalCa) -> Vec<ParsedExtension<'static>> {
        let der: &'static [u8] = Box::leak(ca.cert_der().to_vec().into_boxed_slice());
        let (_, cert) = x509_parser::parse_x509_certificate(der).unwrap();
        cert.extensions()
            .iter()
            .map(|e| e.parsed_extension().clone())
            .collect()
    }

    #[test]
    fn the_ca_is_a_p256_ca_with_pathlen_0_for_one_year() {
        let ca = LocalCa::generate("Rustman Local CA").unwrap();
        let (_, cert) = x509_parser::parse_x509_certificate(ca.cert_der()).unwrap();
        assert_eq!(
            cert.signature_algorithm.algorithm,
            x509_parser::oid_registry::OID_SIG_ECDSA_WITH_SHA256
        );
        let validity = cert.validity();
        let days =
            (validity.not_after.to_datetime() - validity.not_before.to_datetime()).whole_days();
        assert_eq!(days, 365, "valid for one year (plus the backdated hour)");
        let extensions = parsed(&ca);
        let constraints = extensions.iter().find_map(|e| match e {
            ParsedExtension::BasicConstraints(bc) => Some(bc.clone()),
            _ => None,
        });
        let constraints = constraints.unwrap();
        assert!(constraints.ca);
        assert_eq!(constraints.path_len_constraint, Some(0));
        let usage = extensions.iter().find_map(|e| match e {
            ParsedExtension::KeyUsage(ku) => Some(*ku),
            _ => None,
        });
        let usage = usage.unwrap();
        assert!(usage.key_cert_sign());
        assert!(!usage.digital_signature());
        let eku = extensions.iter().find_map(|e| match e {
            ParsedExtension::ExtendedKeyUsage(eku) => Some(eku.clone()),
            _ => None,
        });
        let eku = eku.unwrap();
        assert!(eku.server_auth && !eku.client_auth && !eku.code_signing);
        assert!(
            !extensions
                .iter()
                .any(|e| matches!(e, ParsedExtension::NameConstraints(_)))
        );
    }

    #[test]
    fn info_names_the_subject_fingerprints_and_expiry() {
        let ca = LocalCa::generate("Rustman Local CA (test)").unwrap();
        let info = ca.info().unwrap();
        assert_eq!(info.subject, "CN=Rustman Local CA (test), O=Rustman");
        assert_eq!(info.sha256.split(':').count(), 32);
        assert_eq!(info.sha1.len(), 40);
        let year = OffsetDateTime::now_utc() + CA_VALIDITY;
        assert_eq!(info.expires(), year.date().to_string());
        assert_eq!(info_from_pem(ca.cert_pem()).unwrap(), info);
    }

    #[test]
    fn a_key_must_match_its_certificate() {
        let ca = LocalCa::generate("a").unwrap();
        let other = LocalCa::generate("b").unwrap();
        let again = LocalCa::from_pem(ca.cert_pem(), ca.key_pem()).unwrap();
        assert_eq!(again.info().unwrap(), ca.info().unwrap());
        let err = LocalCa::from_pem(ca.cert_pem(), other.key_pem()).unwrap_err();
        assert!(matches!(err, CaError::Mismatch));
    }

    #[test]
    fn debug_output_never_shows_the_key() {
        let ca = LocalCa::generate("a").unwrap();
        let shown = format!("{ca:?}");
        assert!(!shown.contains("PRIVATE KEY"), "{shown}");
    }
}
