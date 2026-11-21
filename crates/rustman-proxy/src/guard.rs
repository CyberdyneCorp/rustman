//! Guards against misuse of the proxy: clients on the network (with
//! `--allow-remote`) reaching anything but the public internet through it,
//! such as this machine's own services, its private networks or a cloud
//! metadata service, and a reverse proxy forwarding to itself.

use std::net::{IpAddr, SocketAddr};

use rustman_http::address::is_public_ip;

/// The answer a network client gets for a non-public target.
pub(crate) const INTERNAL_REFUSED: &str = "rustman record: requests from the network to this \
     machine, private, link-local or other non-public addresses are refused\n";

/// `host` without IPv6 brackets or a trailing dot, in lowercase.
fn bare(host: &str) -> String {
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_ascii_lowercase()
}

/// Whether `host` (a name or an IP literal) is refused to network
/// clients without resolving it: `localhost` names and literal addresses
/// that are not globally routable. Other names are checked after DNS
/// with [`is_refused_ip`].
pub(crate) fn is_refused_host(host: &str) -> bool {
    let host = bare(host);
    if host == "localhost" || host.ends_with(".localhost") {
        return true;
    }
    host.parse::<IpAddr>().is_ok_and(is_refused_ip)
}

/// Whether a network client may not reach `ip`: every address that is
/// not globally routable (loopback, private, CGNAT, unique local,
/// link-local, documentation, multicast, reserved, and IPv6 forms
/// wrapping such an IPv4 address); see [`is_public_ip`].
pub(crate) fn is_refused_ip(ip: IpAddr) -> bool {
    !is_public_ip(ip)
}

/// Whether `host:port` is the listen address `addr` of the proxy itself:
/// that address, or a loopback name or address (all of `127.0.0.0/8`,
/// `::1`, `::ffff:127.x.y.z`, `localhost` and `*.localhost`) when the
/// proxy listens on loopback or on every interface.
pub(crate) fn is_own_address(host: &str, port: u16, addr: SocketAddr) -> bool {
    if port != addr.port() {
        return false;
    }
    let host = bare(host);
    if host == "localhost" || host.ends_with(".localhost") {
        return addr.ip().is_loopback() || addr.ip().is_unspecified();
    }
    host.parse::<IpAddr>().is_ok_and(|ip| {
        let ip = ip.to_canonical();
        ip == addr.ip()
            || (ip.is_loopback() && addr.ip().is_loopback())
            || (addr.ip().is_unspecified() && (ip.is_loopback() || ip.is_unspecified()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refused_hosts_are_localhost_names_and_non_public_literals() {
        for host in [
            "localhost",
            "LOCALHOST.",
            "api.localhost",
            "127.0.0.1",
            "0.0.0.0",
            "169.254.169.254",
            "[::1]",
            "fe80::1",
            "::ffff:127.0.0.1",
            "100.100.100.200",
            "[fd00:ec2::254]",
            "10.0.0.5",
            "172.16.0.1",
            "192.168.1.2",
            "100.64.0.1",
            "192.0.0.170",
            "198.18.0.1",
            "192.0.2.1",
            "224.0.0.1",
            "240.0.0.1",
            "255.255.255.255",
            "fc00::1",
            "::ffff:10.0.0.1",
            "::10.0.0.1",
            "64:ff9b::a00:1",
            "2002:c0a8:101::1",
            "2001:db8::1",
        ] {
            assert!(is_refused_host(host), "{host}");
        }
        for host in [
            "example.com",
            "8.8.8.8",
            "[2606:4700:4700::1111]",
            "::ffff:8.8.8.8",
        ] {
            assert!(!is_refused_host(host), "{host}");
        }
    }

    #[test]
    fn the_own_address_is_matched_by_port_and_loopback() {
        let loopback: SocketAddr = "127.0.0.1:27464".parse().unwrap();
        assert!(is_own_address("127.0.0.1", 27464, loopback));
        assert!(is_own_address("localhost", 27464, loopback));
        assert!(is_own_address("127.0.0.2", 27464, loopback));
        assert!(is_own_address("[::1]", 27464, loopback));
        assert!(is_own_address("127.255.0.9", 27464, loopback));
        assert!(is_own_address("[::ffff:127.0.0.1]", 27464, loopback));
        assert!(is_own_address("api.localhost", 27464, loopback));
        assert!(!is_own_address("127.0.0.1", 27465, loopback));
        assert!(!is_own_address("api.example.com", 27464, loopback));
        let any: SocketAddr = "0.0.0.0:9000".parse().unwrap();
        assert!(is_own_address("127.0.0.1", 9000, any));
        let lan: SocketAddr = "192.168.1.2:9000".parse().unwrap();
        assert!(is_own_address("192.168.1.2", 9000, lan));
        assert!(!is_own_address("localhost", 9000, lan));
    }
}
