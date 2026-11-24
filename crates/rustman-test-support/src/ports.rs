//! Loopback ports for tests that need a connection to be refused.

use std::net::TcpListener;
use std::sync::atomic::{AtomicU32, Ordering};

/// Below every OS's ephemeral range (Linux 32768+, macOS and Windows
/// 49152+): a server bound to port 0 never gets one of these.
const FIRST: u32 = 20_000;
const COUNT: u32 = 10_000;

/// A loopback port nothing listens on: connecting to it is refused.
///
/// Binding a listener on port 0 and dropping it raced with the servers of
/// tests running in parallel: the OS could hand the same ephemeral port to
/// one of them before the refused connection was tried. The ports here come
/// from outside the ephemeral range, so no test server can take them; the
/// bind check skips one some other program listens on.
pub fn closed_port() -> u16 {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let seed = std::process::id().wrapping_mul(7919);
    loop {
        let offset = seed.wrapping_add(NEXT.fetch_add(1, Ordering::Relaxed)) % COUNT;
        let port = u16::try_from(FIRST + offset).expect("port in range");
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return port;
        }
    }
}

/// `http://127.0.0.1:<port>` on a [`closed_port`].
pub fn closed_url() -> String {
    format!("http://127.0.0.1:{}", closed_port())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpStream;

    #[test]
    fn a_closed_port_refuses_connections() {
        let port = closed_port();
        let err = TcpStream::connect(("127.0.0.1", port)).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::ConnectionRefused);
    }

    #[test]
    fn test_servers_never_get_a_closed_port() {
        let range = FIRST..FIRST + COUNT;
        assert!(range.contains(&u32::from(closed_port())));
        let servers: Vec<TcpListener> = (0..256)
            .map(|_| TcpListener::bind("127.0.0.1:0").unwrap())
            .collect();
        for server in &servers {
            let port = u32::from(server.local_addr().unwrap().port());
            assert!(!range.contains(&port), "ephemeral port {port}");
        }
    }
}
