//! Certificates generated with `rcgen` for each test run: a CA, a server
//! certificate for `localhost` signed by it, a client certificate for mTLS,
//! and an unrelated self-signed server certificate.

use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, DnType, ExtendedKeyUsagePurpose, IsCa,
    KeyPair,
};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};

pub struct TestCerts {
    /// The CA that signed the server and client certificates, PEM encoded.
    pub ca_pem: String,
    pub server_cert_pem: String,
    pub server_key_pem: String,
    pub client_cert_pem: String,
    pub client_key_pem: String,
    /// A self-signed `localhost` certificate no CA vouches for.
    pub self_signed_cert_pem: String,
    pub self_signed_key_pem: String,
}

impl TestCerts {
    pub fn generate() -> Self {
        let ca = certified_ca();
        let (server_cert_pem, server_key_pem) =
            leaf(&ca, "localhost", ExtendedKeyUsagePurpose::ServerAuth);
        let (client_cert_pem, client_key_pem) =
            leaf(&ca, "rustman-client", ExtendedKeyUsagePurpose::ClientAuth);
        let key = KeyPair::generate().unwrap();
        let self_signed = server_params("localhost").self_signed(&key).unwrap();
        Self {
            ca_pem: ca.pem(),
            server_cert_pem,
            server_key_pem,
            client_cert_pem,
            client_key_pem,
            self_signed_cert_pem: self_signed.pem(),
            self_signed_key_pem: key.serialize_pem(),
        }
    }
}

fn certified_ca() -> CertifiedIssuer<'static, KeyPair> {
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params
        .distinguished_name
        .push(DnType::CommonName, "Rustman Test CA");
    CertifiedIssuer::self_signed(params, KeyPair::generate().unwrap()).unwrap()
}

fn server_params(name: &str) -> CertificateParams {
    let mut params = CertificateParams::new(vec![name.to_owned(), "127.0.0.1".to_owned()]).unwrap();
    params.distinguished_name.push(DnType::CommonName, name);
    params
}

fn leaf(
    ca: &CertifiedIssuer<'static, KeyPair>,
    name: &str,
    usage: ExtendedKeyUsagePurpose,
) -> (String, String) {
    let mut params = server_params(name);
    params.extended_key_usages = vec![usage];
    let key = KeyPair::generate().unwrap();
    let cert = params.signed_by(&key, ca).unwrap();
    (cert.pem(), key.serialize_pem())
}

impl TestCerts {
    /// The client certificate and key as a PKCS#12 bundle.
    pub fn client_pkcs12(&self, password: &str) -> Vec<u8> {
        use p12_keystore::{
            Certificate, KeyStore, KeyStoreEntry, LocalKeyId, PrivateKey, PrivateKeyChain,
        };
        let cert = &certs_from_pem(&self.client_cert_pem)[0];
        let key = key_from_pem(&self.client_key_pem);
        let chain = PrivateKeyChain::new(
            LocalKeyId(vec![1, 2, 3, 4]),
            PrivateKey::from_der(key.secret_der()).unwrap(),
            [Certificate::from_der(cert.as_ref()).unwrap()],
        );
        let mut store = KeyStore::new();
        store.add_entry("client", KeyStoreEntry::PrivateKeyChain(chain));
        store.writer(password).write().unwrap()
    }
}

pub fn certs_from_pem(pem: &str) -> Vec<CertificateDer<'static>> {
    CertificateDer::pem_slice_iter(pem.as_bytes())
        .collect::<Result<_, _>>()
        .unwrap()
}

pub fn key_from_pem(pem: &str) -> PrivateKeyDer<'static> {
    PrivateKeyDer::from_pem_slice(pem.as_bytes()).unwrap()
}
