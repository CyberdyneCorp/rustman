//! Leaf certificates minted per intercepted host (design D2): signed by
//! the local CA, valid for 7 days, generated in memory and cached for the
//! session in an LRU of 1000 entries. Leaf keys never touch disk.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use rcgen::{
    CertificateParams, DnType, ExtendedKeyUsagePurpose, Issuer, KeyPair, KeyUsagePurpose,
    PKCS_ECDSA_P256_SHA256,
};
use rustls::ServerConfig;
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use time::{Duration, OffsetDateTime};

use super::{BACKDATE, CaError, LocalCa, random_serial};

/// How long a leaf certificate is valid.
pub const LEAF_VALIDITY: Duration = Duration::days(7);

/// How many hosts' certificates are cached.
pub const LEAF_CACHE_SIZE: usize = 1000;

/// ALPN offered to intercepted clients: HTTP/2 first, then HTTP/1.1.
const ALPN: [&[u8]; 2] = [b"h2", b"http/1.1"];

/// A leaf certificate and its key.
pub struct Leaf {
    pub cert: CertificateDer<'static>,
    pub key: PrivateKeyDer<'static>,
}

/// Mints and caches the TLS server configs of intercepted hosts.
pub struct Minter {
    issuer: Issuer<'static, KeyPair>,
    cache: Mutex<Lru>,
}

impl std::fmt::Debug for Minter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Minter").finish_non_exhaustive()
    }
}

impl Minter {
    /// A minter signing with `ca`, with an empty cache.
    pub fn new(ca: &LocalCa) -> Result<Self, CaError> {
        let key = KeyPair::from_pem(ca.key_pem()).map_err(CaError::invalid)?;
        let issuer = Issuer::from_ca_cert_der(ca.cert_der(), key).map_err(CaError::invalid)?;
        Ok(Self {
            issuer,
            cache: Mutex::new(Lru::new(LEAF_CACHE_SIZE)),
        })
    }

    /// A new leaf certificate for `host` (a DNS name or an IP address);
    /// anything else, such as a wildcard (`*.example.com`), is refused.
    pub fn mint(&self, host: &str) -> Result<Leaf, CaError> {
        if !is_certifiable_host(host) {
            return Err(CaError::invalid(format!(
                "{host:?} is neither a DNS name nor an IP address"
            )));
        }
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).map_err(CaError::invalid)?;
        let mut params = CertificateParams::new(vec![host.to_owned()]).map_err(CaError::invalid)?;
        params.distinguished_name.push(DnType::CommonName, host);
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        params.use_authority_key_identifier_extension = true;
        let now = OffsetDateTime::now_utc();
        params.not_before = now - BACKDATE;
        params.not_after = now + LEAF_VALIDITY;
        params.serial_number = Some(random_serial()?);
        let cert = params
            .signed_by(&key, &self.issuer)
            .map_err(CaError::invalid)?;
        Ok(Leaf {
            cert: cert.der().clone(),
            key: PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
        })
    }

    /// The TLS server config presenting a leaf for `host`, from the cache
    /// or newly minted.
    pub fn server_config(&self, host: &str) -> Result<Arc<ServerConfig>, CaError> {
        let host = host.to_ascii_lowercase();
        if let Some(config) = self.lock().get(&host) {
            return Ok(config);
        }
        let config = Arc::new(server_config(self.mint(&host)?)?);
        self.lock().insert(host, Arc::clone(&config));
        Ok(config)
    }

    /// How many hosts are cached.
    pub fn cached(&self) -> usize {
        self.lock().map.len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Lru> {
        self.cache.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Whether a leaf may be minted for `host`: a valid DNS name (no
/// wildcard) or an IP address.
pub fn is_certifiable_host(host: &str) -> bool {
    rustls_pki_types::ServerName::try_from(host).is_ok()
}

fn server_config(leaf: Leaf) -> Result<ServerConfig, CaError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(CaError::invalid)?
        .with_no_client_auth()
        .with_single_cert(vec![leaf.cert], leaf.key)
        .map_err(CaError::invalid)?;
    config.alpn_protocols = ALPN.iter().map(|p| p.to_vec()).collect();
    Ok(config)
}

/// A small least-recently-used map; eviction scans, which is cheap at
/// this size and happens only once the cache is full.
struct Lru {
    capacity: usize,
    tick: u64,
    map: HashMap<String, (u64, Arc<ServerConfig>)>,
}

impl Lru {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            tick: 0,
            map: HashMap::new(),
        }
    }

    fn get(&mut self, host: &str) -> Option<Arc<ServerConfig>> {
        self.tick += 1;
        let tick = self.tick;
        self.map.get_mut(host).map(|(used, config)| {
            *used = tick;
            Arc::clone(config)
        })
    }

    fn insert(&mut self, host: String, config: Arc<ServerConfig>) {
        if self.map.len() >= self.capacity && !self.map.contains_key(&host) {
            let oldest = self
                .map
                .iter()
                .min_by_key(|(_, (used, _))| *used)
                .map(|(host, _)| host.clone());
            if let Some(oldest) = oldest {
                self.map.remove(&oldest);
            }
        }
        self.tick += 1;
        self.map.insert(host, (self.tick, config));
    }
}

#[cfg(test)]
mod tests {
    use x509_parser::extensions::GeneralName;

    use super::*;

    #[test]
    fn leaves_are_signed_by_the_ca_for_seven_days() {
        let ca = LocalCa::generate("Rustman Local CA").unwrap();
        let minter = Minter::new(&ca).unwrap();
        let leaf = minter.mint("api.example.com").unwrap();
        let (_, cert) = x509_parser::parse_x509_certificate(&leaf.cert).unwrap();
        let (_, root) = x509_parser::parse_x509_certificate(ca.cert_der()).unwrap();
        assert_eq!(cert.issuer(), root.subject());
        cert.verify_signature(Some(root.public_key())).unwrap();
        let validity = cert.validity();
        let days =
            (validity.not_after.to_datetime() - validity.not_before.to_datetime()).whole_days();
        assert_eq!(days, 7);
        let names = cert.subject_alternative_name().unwrap().unwrap();
        assert!(matches!(
            names.value.general_names.as_slice(),
            [GeneralName::DNSName("api.example.com")]
        ));
        assert!(!cert.is_ca());
    }

    #[test]
    fn ip_hosts_get_an_ip_san() {
        let minter = Minter::new(&LocalCa::generate("a").unwrap()).unwrap();
        let leaf = minter.mint("127.0.0.1").unwrap();
        let (_, cert) = x509_parser::parse_x509_certificate(&leaf.cert).unwrap();
        let names = cert.subject_alternative_name().unwrap().unwrap();
        assert!(matches!(
            names.value.general_names.as_slice(),
            [GeneralName::IPAddress([127, 0, 0, 1])]
        ));
    }

    #[test]
    fn hosts_that_are_neither_dns_names_nor_ips_are_refused() {
        let minter = Minter::new(&LocalCa::generate("a").unwrap()).unwrap();
        for host in ["*.example.com", "*", "exa mple.com", "a..b", "-"] {
            assert!(!is_certifiable_host(host), "{host}");
            assert!(minter.mint(host).is_err(), "{host}");
            assert!(minter.server_config(host).is_err(), "{host}");
        }
        assert_eq!(minter.cached(), 0);
        for host in ["api.example.com", "localhost", "127.0.0.1", "::1"] {
            assert!(is_certifiable_host(host), "{host}");
        }
    }

    #[test]
    fn configs_are_cached_per_host() {
        let minter = Minter::new(&LocalCa::generate("a").unwrap()).unwrap();
        let first = minter.server_config("A.test").unwrap();
        let again = minter.server_config("a.test").unwrap();
        assert!(Arc::ptr_eq(&first, &again));
        assert_eq!(first.alpn_protocols, [b"h2".to_vec(), b"http/1.1".to_vec()]);
        assert_eq!(minter.cached(), 1);
    }

    #[test]
    fn the_least_recently_used_host_is_evicted() {
        let minter = Minter::new(&LocalCa::generate("a").unwrap()).unwrap();
        let config = minter.server_config("a.test").unwrap();
        let mut lru = Lru::new(2);
        lru.insert("a".into(), Arc::clone(&config));
        lru.insert("b".into(), Arc::clone(&config));
        assert!(lru.get("a").is_some());
        lru.insert("c".into(), config);
        assert!(lru.get("b").is_none());
        assert!(lru.get("a").is_some() && lru.get("c").is_some());
        assert_eq!(LEAF_CACHE_SIZE, 1000);
    }
}
