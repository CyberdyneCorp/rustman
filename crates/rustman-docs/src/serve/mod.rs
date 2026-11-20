//! `rustman docs serve` (api-docs-generation "Live-reload preview", design
//! D6): the site is built into a temporary folder and served on
//! `127.0.0.1` only. A workspace change rebuilds it into a fresh folder,
//! which replaces the served one at once; open pages reload through an SSE
//! endpoint. The reload script and the error overlay are added to pages as
//! they are served, never to the files a build writes.

mod inject;
mod respond;
mod source;
mod state;
mod watch;

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::error::DocsError;

pub use source::{OpenStore, PreviewSource};
pub use state::{BuildOutcome, Reporter, SiteBuilder};
pub use watch::{DEBOUNCE, PreviewWatcher};

/// The SSE endpoint open pages listen on for reloads.
pub const RELOAD_PATH: &str = "/__rustman/reload";

/// A bound preview server, not yet accepting.
pub struct Preview {
    listener: TcpListener,
    addr: SocketAddr,
    shared: Arc<state::Shared>,
}

impl Preview {
    /// Bind `127.0.0.1:port` (`0` picks a free port) to serve what
    /// `builder` builds; `reporter` hears about every build.
    pub async fn bind(
        port: u16,
        builder: Arc<dyn SiteBuilder>,
        reporter: Reporter,
    ) -> Result<Self, DocsError> {
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let bind_error = |err: std::io::Error| DocsError::Bind {
            addr: addr.to_string(),
            reason: bind_reason(&err),
        };
        let listener = TcpListener::bind(addr).await.map_err(bind_error)?;
        let addr = listener.local_addr().map_err(bind_error)?;
        Ok(Self {
            listener,
            addr,
            shared: Arc::new(state::Shared::new(builder, reporter)),
        })
    }

    /// The bound address, with the actual port when `0` was asked.
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Build the site now and serve the result (or show its error).
    pub async fn rebuild(&self) {
        self.shared.rebuild().await;
    }

    /// How many builds finished so far.
    pub fn generation(&self) -> u64 {
        self.shared.snapshot().generation
    }

    /// Rebuild after changes under `root` settle for [`DEBOUNCE`]. Must be
    /// called inside a Tokio runtime; dropping the watcher stops it.
    pub fn watch(&self, root: &std::path::Path) -> Result<PreviewWatcher, DocsError> {
        watch::watch(root, Arc::clone(&self.shared))
    }

    /// Accept connections until `stop` is cancelled.
    pub async fn serve(self, stop: CancellationToken) {
        loop {
            let accepted = tokio::select! {
                () = stop.cancelled() => return,
                accepted = self.listener.accept() => accepted,
            };
            match accepted {
                Ok((stream, _)) => {
                    let _ = stream.set_nodelay(true);
                    tokio::spawn(connection(stream, Arc::clone(&self.shared)));
                }
                // Out of file descriptors and the like: back off briefly.
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
    }
}

async fn connection(stream: tokio::net::TcpStream, shared: Arc<state::Shared>) {
    let service = service_fn(move |request| respond::respond(Arc::clone(&shared), request));
    let _ = http1::Builder::new()
        .serve_connection(TokioIo::new(stream), service)
        .await;
}

/// The reason of a failed bind, without OS error numbers.
fn bind_reason(err: &std::io::Error) -> String {
    match err.kind() {
        std::io::ErrorKind::AddrInUse => "address in use".to_owned(),
        std::io::ErrorKind::PermissionDenied => "permission denied".to_owned(),
        _ => {
            let text = err.to_string();
            match text.find(" (os error") {
                Some(at) => text[..at].to_owned(),
                None => text,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_reasons_drop_os_error_numbers() {
        let in_use = std::io::Error::from(std::io::ErrorKind::AddrInUse);
        assert_eq!(bind_reason(&in_use), "address in use");
        let other = std::io::Error::other("boom (os error 99)");
        assert_eq!(bind_reason(&other), "boom");
    }
}
