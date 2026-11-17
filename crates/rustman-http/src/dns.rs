//! Name resolution: `hickory-resolver` with the system configuration, falling
//! back to the system resolver (`tokio::net::lookup_host`) when it fails.

use std::net::SocketAddr;
use std::sync::OnceLock;

use hickory_resolver::TokioResolver;

use crate::error::RequestError;

#[derive(Default)]
pub(crate) struct Dns {
    /// Built on first use, inside the runtime; `None` if the system
    /// configuration cannot be read.
    hickory: OnceLock<Option<TokioResolver>>,
}

impl Dns {
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
