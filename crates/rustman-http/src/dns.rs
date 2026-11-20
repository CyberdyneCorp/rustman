//! Name resolution: `hickory-resolver` with the system configuration, falling
//! back to the system resolver (`tokio::net::lookup_host`) when it fails.

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, OnceLock};

use hickory_resolver::TokioResolver;

use crate::error::RequestError;

/// Decides, at connect time, whether a request to `host` may connect to
/// the address it resolved to: `Err(reason)` refuses it (`blocked:
/// <reason>`). It sees every address of the server (never a proxy's), so a
/// name cannot be re-pointed between a check and the connection.
pub type AddressGuard = Arc<dyn Fn(&str, IpAddr) -> Result<(), String> + Send + Sync>;

#[derive(Default)]
pub(crate) struct Dns {
    /// Built on first use, inside the runtime; `None` if the system
    /// configuration cannot be read.
    hickory: OnceLock<Option<TokioResolver>>,
    guard: Option<AddressGuard>,
}

impl Dns {
    /// A resolver whose server addresses `guard` must allow.
    pub fn guarded(guard: AddressGuard) -> Self {
        Self {
            hickory: OnceLock::new(),
            guard: Some(guard),
        }
    }

    /// Refuse the server addresses `addrs` of `host` that the guard does
    /// not allow.
    pub fn check(&self, host: &str, addrs: &[SocketAddr]) -> Result<(), RequestError> {
        let Some(guard) = &self.guard else {
            return Ok(());
        };
        addrs
            .iter()
            .try_for_each(|addr| guard(host, addr.ip()))
            .map_err(RequestError::Blocked)
    }

    /// Every address for `host`, in resolver order.
    pub async fn lookup(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, RequestError> {
        if let Some(addrs) = self.hickory_lookup(host, port).await {
            return Ok(addrs);
        }
        match tokio::net::lookup_host((host, port)).await {
            Ok(addrs) => Ok(addrs.collect()),
            Err(e) => Err(RequestError::Network(format!(
                "dns: cannot resolve {host}: {e}"
            ))),
        }
    }

    async fn hickory_lookup(&self, host: &str, port: u16) -> Option<Vec<SocketAddr>> {
        let resolver = self.hickory.get_or_init(build_resolver).as_ref()?;
        let ips = resolver.lookup_ip(host).await.ok()?;
        let addrs: Vec<_> = ips.iter().map(|ip| SocketAddr::new(ip, port)).collect();
        (!addrs.is_empty()).then_some(addrs)
    }
}

fn build_resolver() -> Option<TokioResolver> {
    TokioResolver::builder_tokio().ok()?.build().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_guard_sees_every_address() {
        let guard: AddressGuard = Arc::new(|host, ip| {
            if ip.is_loopback() {
                Err(format!("{host} resolves to {ip}"))
            } else {
                Ok(())
            }
        });
        let dns = Dns::guarded(guard);
        let public = SocketAddr::from(([93, 184, 216, 34], 80));
        let local = SocketAddr::from(([127, 0, 0, 1], 80));
        assert!(dns.check("a.test", &[public]).is_ok());
        assert_eq!(
            dns.check("a.test", &[public, local])
                .unwrap_err()
                .to_string(),
            "blocked: a.test resolves to 127.0.0.1"
        );
        assert!(Dns::default().check("a.test", &[local]).is_ok());
    }

    #[tokio::test]
    async fn resolves_localhost() {
        let addrs = Dns::default().lookup("localhost", 8080).await.unwrap();
        assert!(!addrs.is_empty());
        assert!(
            addrs
                .iter()
                .all(|a| a.ip().is_loopback() && a.port() == 8080)
        );
    }
}
