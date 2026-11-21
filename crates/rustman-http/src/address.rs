//! Which addresses are globally routable: the one deny-by-default check
//! for connections opened on behalf of someone else (a recording proxy's
//! network clients, a plugin's granted hosts, a plugin download), so they
//! cannot reach this machine, its private networks or a cloud metadata
//! service. Only addresses outside every special-purpose range (the IANA
//! registries, RFC 6890) are public; IPv6 is judged by allowlist (global
//! unicast `2000::/3` only), so a range assigned later stays refused.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// IPv4 ranges that are not globally routable.
const V4_NON_PUBLIC: [(Ipv4Addr, u8); 15] = [
    (Ipv4Addr::new(0, 0, 0, 0), 8),       // "this network"
    (Ipv4Addr::new(10, 0, 0, 0), 8),      // private (RFC 1918)
    (Ipv4Addr::new(100, 64, 0, 0), 10),   // CGNAT (RFC 6598), 100.100.100.200 metadata
    (Ipv4Addr::new(127, 0, 0, 0), 8),     // loopback
    (Ipv4Addr::new(169, 254, 0, 0), 16),  // link-local, 169.254.169.254 metadata
    (Ipv4Addr::new(172, 16, 0, 0), 12),   // private (RFC 1918)
    (Ipv4Addr::new(192, 0, 0, 0), 24),    // IETF protocol assignments
    (Ipv4Addr::new(192, 0, 2, 0), 24),    // documentation (TEST-NET-1)
    (Ipv4Addr::new(192, 88, 99, 0), 24),  // deprecated 6to4 relay anycast
    (Ipv4Addr::new(192, 168, 0, 0), 16),  // private (RFC 1918)
    (Ipv4Addr::new(198, 18, 0, 0), 15),   // benchmarking
    (Ipv4Addr::new(198, 51, 100, 0), 24), // documentation (TEST-NET-2)
    (Ipv4Addr::new(203, 0, 113, 0), 24),  // documentation (TEST-NET-3)
    (Ipv4Addr::new(224, 0, 0, 0), 4),     // multicast
    (Ipv4Addr::new(240, 0, 0, 0), 4),     // reserved, 255.255.255.255 broadcast
];

/// IPv6 global unicast: the only IPv6 range that can be public.
const V6_GLOBAL_UNICAST: (Ipv6Addr, u8) = (Ipv6Addr::new(0x2000, 0, 0, 0, 0, 0, 0, 0), 3);

/// The ranges inside global unicast that are not globally routable. Every
/// range outside it (loopback, unspecified, discard-only `100::/64`,
/// local-use NAT64 `64:ff9b:1::/48`, SRv6 `5f00::/16`, unique local
/// `fc00::/7` with `fd00:ec2::254` metadata, link-local, site-local,
/// multicast) is refused by the allowlist.
const V6_NON_PUBLIC: [(Ipv6Addr, u8); 3] = [
    // IETF protocol assignments: Teredo, benchmarking, ORCHID, ORCHIDv2
    (Ipv6Addr::new(0x2001, 0, 0, 0, 0, 0, 0, 0), 23),
    (Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 0), 32), // documentation
    (Ipv6Addr::new(0x3fff, 0, 0, 0, 0, 0, 0, 0), 20),     // documentation (RFC 9637)
];

/// Whether `ip` is globally routable: not loopback, private, link-local,
/// shared (CGNAT), documentation, benchmarking, multicast, reserved or
/// broadcast, and for IPv6 inside global unicast. An IPv6 address wrapping
/// an IPv4 one (IPv4-mapped, IPv4-compatible, NAT64 `64:ff9b::/96`, 6to4
/// `2002::/16`) is public only when the IPv4 address is.
pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => match embedded_v4(v6) {
            Some(v4) => is_public_v4(v4),
            None => is_public_v6(v6),
        },
    }
}

/// Whether `host` names this machine without resolving it: `localhost` or
/// a `.localhost` name (RFC 6761), in any case, with or without a trailing
/// dot.
pub fn is_localhost_name(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    host == "localhost" || host.ends_with(".localhost")
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    !V4_NON_PUBLIC.iter().any(|&(net, len)| in_v4(ip, net, len))
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    let (global, len) = V6_GLOBAL_UNICAST;
    in_v6(ip, global, len) && !V6_NON_PUBLIC.iter().any(|&(net, len)| in_v6(ip, net, len))
}

/// The IPv4 address an IPv6 address carries: IPv4-mapped `::ffff:0:0/96`,
/// IPv4-compatible `::/96` (`::` and `::1` included, as `0.0.0.0/8`),
/// NAT64 `64:ff9b::/96` (the last 32 bits) and 6to4 `2002::/16` (bits 16
/// to 48).
fn embedded_v4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    let bits = u128::from(ip);
    let low = Ipv4Addr::from(bits as u32);
    let wrapping = [
        Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0, 0),
        Ipv6Addr::UNSPECIFIED,
        Ipv6Addr::new(0x64, 0xff9b, 0, 0, 0, 0, 0, 0),
    ];
    if wrapping.iter().any(|&net| in_v6(ip, net, 96)) {
        return Some(low);
    }
    let six_to_four = Ipv6Addr::new(0x2002, 0, 0, 0, 0, 0, 0, 0);
    in_v6(ip, six_to_four, 16).then(|| Ipv4Addr::from((bits >> 80) as u32))
}

fn in_v4(ip: Ipv4Addr, net: Ipv4Addr, len: u8) -> bool {
    let mask = u32::MAX.checked_shl(32 - u32::from(len)).unwrap_or(0);
    u32::from(ip) & mask == u32::from(net) & mask
}

fn in_v6(ip: Ipv6Addr, net: Ipv6Addr, len: u8) -> bool {
    let mask = u128::MAX.checked_shl(128 - u32::from(len)).unwrap_or(0);
    u128::from(ip) & mask == u128::from(net) & mask
}

#[cfg(test)]
mod tests {
    use super::*;

    fn public(text: &str) -> bool {
        is_public_ip(text.parse().unwrap())
    }

    /// One address per non-public range, with the first public address
    /// past it where the boundary matters.
    #[test]
    fn every_special_purpose_ipv4_range_is_not_public() {
        let ranges = [
            ("this network", "0.1.2.3"),
            ("unspecified", "0.0.0.0"),
            ("RFC 1918 10/8", "10.0.0.5"),
            ("CGNAT", "100.64.0.1"),
            ("CGNAT end", "100.127.255.254"),
            ("Alibaba metadata", "100.100.100.200"),
            ("loopback", "127.8.9.10"),
            ("link-local metadata", "169.254.169.254"),
            ("RFC 1918 172.16/12", "172.31.255.1"),
            ("IETF protocol assignments", "192.0.0.170"),
            ("TEST-NET-1", "192.0.2.10"),
            ("6to4 relay anycast", "192.88.99.1"),
            ("RFC 1918 192.168/16", "192.168.1.2"),
            ("benchmarking", "198.18.0.1"),
            ("benchmarking end", "198.19.255.255"),
            ("TEST-NET-2", "198.51.100.7"),
            ("TEST-NET-3", "203.0.113.9"),
            ("multicast", "224.0.0.251"),
            ("multicast end", "239.255.255.250"),
            ("reserved", "240.0.0.1"),
            ("broadcast", "255.255.255.255"),
        ];
        for (name, ip) in ranges {
            assert!(!public(ip), "{name}: {ip}");
        }
        for ip in [
            "8.8.8.8",
            "1.1.1.1",
            "100.63.255.255",
            "100.128.0.0",
            "172.15.255.255",
            "172.32.0.0",
            "192.0.1.1",
            "198.17.255.255",
            "198.20.0.0",
            "223.255.255.255",
        ] {
            assert!(public(ip), "{ip}");
        }
    }

    #[test]
    fn every_special_purpose_ipv6_range_is_not_public() {
        let ranges = [
            ("unspecified", "::"),
            ("loopback", "::1"),
            ("local-use NAT64", "64:ff9b:1::1"),
            ("discard-only", "100::1"),
            ("Teredo", "2001::1"),
            ("benchmarking", "2001:2::1"),
            ("ORCHID", "2001:10::1"),
            ("ORCHIDv2", "2001:20::1"),
            ("documentation", "2001:db8::1"),
            ("documentation RFC 9637", "3fff::1"),
            ("SRv6", "5f00::1"),
            ("ULA", "fc00::1"),
            ("ULA fd", "fd12:3456::1"),
            ("AWS metadata", "fd00:ec2::254"),
            ("link-local", "fe80::1"),
            ("site-local", "fec0::1"),
            ("multicast", "ff02::1"),
        ];
        for (name, ip) in ranges {
            assert!(!public(ip), "{name}: {ip}");
        }
        for ip in [
            "2606:4700:4700::1111",
            "2a00:1450:4001::200e",
            "2001:4860::8888",
        ] {
            assert!(public(ip), "{ip}");
        }
    }

    #[test]
    fn ipv6_wrapping_ipv4_follows_the_ipv4_address() {
        for ip in [
            "::ffff:127.0.0.1",
            "::ffff:10.1.2.3",
            "::ffff:169.254.169.254",
            "::ffff:100.100.100.200",
            "::127.0.0.1",
            "::192.168.1.1",
            "64:ff9b::10.0.0.1",
            "64:ff9b::a9fe:a9fe",
            "2002:c0a8:0101::1",
            "2002:7f00:1::",
            "2002:a9fe:a9fe::1",
        ] {
            assert!(!public(ip), "{ip}");
        }
        for ip in ["::ffff:8.8.8.8", "64:ff9b::808:808", "2002:808:808::1"] {
            assert!(public(ip), "{ip}");
        }
    }

    /// Every address that is not globally routable unicast is refused,
    /// including IPv6 forms wrapping a private IPv4 address.
    #[test]
    fn deny_by_default_covers_every_listed_range() {
        let non_public = [
            // this network, private, loopback, link-local, unspecified
            "0.1.2.3",
            "127.0.0.1",
            "127.255.0.9",
            "10.0.0.1",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "0.0.0.0",
            // shared CGNAT 100.64.0.0/10
            "100.64.0.1",
            "100.127.255.254",
            // IETF protocol assignments 192.0.0.0/24
            "192.0.0.170",
            // documentation
            "192.0.2.1",
            "198.51.100.7",
            "203.0.113.9",
            // 6to4 relay anycast
            "192.88.99.1",
            // benchmarking 198.18.0.0/15
            "198.18.0.1",
            "198.19.255.255",
            // multicast, reserved, broadcast
            "224.0.0.1",
            "239.255.255.250",
            "240.0.0.1",
            "255.255.255.255",
            // IPv6: loopback, unspecified, link-local, unique-local
            "::1",
            "::",
            "fe80::1",
            "fd00::1",
            "fc00::1",
            // site-local (deprecated), multicast, discard
            "fec0::1",
            "ff02::1",
            "100::1",
            // documentation, IETF assignments, Teredo
            "2001:db8::1",
            "3fff::1",
            "2001::1",
            // IPv4-mapped, IPv4-compatible, NAT64, 6to4 of private addresses
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "::127.0.0.1",
            "::10.0.0.1",
            "64:ff9b::a9fe:a9fe",
            "64:ff9b::127.0.0.1",
            "2002:7f00:1::",
            "2002:a9fe:a9fe::1",
            "2002:c0a8:101::",
        ];
        for ip in non_public {
            assert!(!public(ip), "{ip}");
        }
        let routable = [
            "93.184.216.34",
            "172.32.0.1",
            "100.63.255.255",
            "100.128.0.1",
            "198.17.255.255",
            "198.20.0.1",
            "223.255.255.255",
            "8.8.8.8",
            "2606:4700::1111",
            "2a00:1450::1",
            "::ffff:8.8.8.8",
            "64:ff9b::808:808",
            "2002:808:808::",
        ];
        for ip in routable {
            assert!(public(ip), "{ip}");
        }
    }

    /// IPv6 is an allowlist: anything outside global unicast, including
    /// ranges not assigned yet, and the IETF block around Teredo are refused.
    #[test]
    fn ipv6_outside_global_unicast_is_not_public() {
        for ip in ["4000::1", "8000::1", "e000::1", "2001:1::1", "2001:1ff::1"] {
            assert!(!public(ip), "{ip}");
        }
        for ip in ["2001:200::1", "3ffe::1", "2c0f:f248::1"] {
            assert!(public(ip), "{ip}");
        }
    }

    #[test]
    fn localhost_names_are_recognised_without_resolving() {
        for host in ["localhost", "LOCALHOST.", "app.localhost", "a.b.Localhost"] {
            assert!(is_localhost_name(host), "{host}");
        }
        for host in ["localhost.example.com", "notlocalhost", "127.0.0.1", ""] {
            assert!(!is_localhost_name(host), "{host}");
        }
    }
}
