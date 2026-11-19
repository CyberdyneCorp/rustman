//! In-process gRPC servers (add-grpc-client): the `shop.v1` test schema in
//! `protos/`, compiled with protox, served with reflection (v1, v1alpha,
//! both or none), the standard health service and the `shop.v1.Catalog`
//! service, on a loopback port. The server speaks h2c or TLS (optionally
//! requiring a client certificate) and also answers gRPC-Web over HTTP/1.1.

mod catalog;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use prost::Message as _;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::{Certificate, Identity, Server, ServerTlsConfig};
pub use tonic_health::ServingStatus;
use tonic_health::server::HealthReporter;
use tonic_web::GrpcWebLayer;

pub use catalog::{
    CREATED_AT_SECONDS, CatalogLog, ChatMessage, ENDLESS, GetProductRequest, Product, ReceivedCall,
    SESSION_COOKIE, SESSION_TOKEN, SLOW_DELAY, UploadSummary,
};
use catalog::{CatalogServer, SharedLog, lock};

use crate::certs::TestCerts;

/// A target nothing can ever listen on (port 0): dialing it fails at once
/// on every OS, so it is the unreachable server of tests. Dialing the freed
/// port of a stopped server is not reliable: another listener may take
/// the port, or the client may connect to itself through it.
pub const UNREACHABLE_TARGET: &str = "grpc://127.0.0.1:0";

/// The fully qualified name of the test service.
pub const CATALOG_SERVICE: &str = "shop.v1.Catalog";

/// Which reflection services a [`GrpcServer`] exposes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reflection {
    V1,
    V1Alpha,
    Both,
    None,
}

/// How a [`GrpcServer`] secures its connections.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ServerTls {
    /// Plaintext HTTP/2 (h2c) and HTTP/1.1 for gRPC-Web.
    #[default]
    Plain,
    /// TLS with the `localhost` certificate of the test CA.
    Ca,
    /// TLS with a self-signed certificate no CA vouches for.
    SelfSigned,
    /// TLS that requires a client certificate signed by the test CA.
    Mutual,
}

/// The folder holding the test `.proto` files (`shop/v1/*.proto`).
pub fn protos_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("protos")
}

/// The test schema as an encoded `FileDescriptorSet`, imports included.
pub fn descriptor_set() -> Vec<u8> {
    let dir = protos_dir();
    let set = protox::Compiler::new([&dir])
        .and_then(|mut c| {
            c.include_imports(true);
            c.open_files([dir.join("shop/v1/catalog.proto")])?;
            Ok(c.file_descriptor_set())
        })
        .expect("the test protos compile");
    set.encode_to_vec()
}

/// A gRPC server on `127.0.0.1`, stopped when dropped.
pub struct GrpcServer {
    addr: SocketAddr,
    tls: bool,
    health: HealthReporter,
    log: SharedLog,
    task: JoinHandle<()>,
}

impl Drop for GrpcServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl GrpcServer {
    /// Start a plaintext server exposing `reflection` and the health
    /// service, which reports `SERVING` for the whole server and for
    /// `shop.v1.Catalog`.
    pub async fn start(reflection: Reflection) -> Self {
        Self::start_with(reflection, ServerTls::Plain, None).await
    }

    /// Start a plaintext server like [`Self::start`] but without the
    /// health service: `Health/Check` answers `UNIMPLEMENTED`.
    pub async fn start_without_health(reflection: Reflection) -> Self {
        Self::start_serving(reflection, ServerTls::Plain, None, false).await
    }

    /// Start a server with `tls`, using `certs` for its certificates.
    pub async fn start_tls(reflection: Reflection, tls: ServerTls, certs: &TestCerts) -> Self {
        Self::start_with(reflection, tls, Some(certs)).await
    }

    async fn start_with(reflection: Reflection, tls: ServerTls, certs: Option<&TestCerts>) -> Self {
        Self::start_serving(reflection, tls, certs, true).await
    }

    async fn start_serving(
        reflection: Reflection,
        tls: ServerTls,
        certs: Option<&TestCerts>,
        with_health: bool,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (health, health_service) = tonic_health::server::health_reporter();
        health
            .set_service_status(CATALOG_SERVICE, ServingStatus::Serving)
            .await;
        let log = SharedLog::default();
        let mut server = Server::builder().accept_http1(true);
        if let Some(config) = certs.and_then(|c| tls_config(tls, c)) {
            server = server.tls_config(config).unwrap();
        }
        let catalog = CatalogServer {
            log: Arc::clone(&log),
        };
        let mut router = server
            .layer(GrpcWebLayer::new())
            .add_optional_service(with_health.then_some(health_service))
            .add_service(catalog);
        let set = descriptor_set();
        let builder = || {
            tonic_reflection::server::Builder::configure()
                .register_encoded_file_descriptor_set(&set)
        };
        if matches!(reflection, Reflection::V1 | Reflection::Both) {
            router = router.add_service(builder().build_v1().unwrap());
        }
        if matches!(reflection, Reflection::V1Alpha | Reflection::Both) {
            router = router.add_service(builder().build_v1alpha().unwrap());
        }
        let incoming = TcpListenerStream::new(listener);
        let task = tokio::spawn(async move {
            let _ = router.serve_with_incoming(incoming).await;
        });
        Self {
            addr,
            tls: tls != ServerTls::Plain,
            health,
            log,
            task,
        }
    }

    /// The calls the catalog received, in order.
    pub fn calls(&self) -> Vec<ReceivedCall> {
        lock(&self.log).calls.clone()
    }

    /// Messages received by each `UploadProducts` call that half-closed.
    pub fn uploads(&self) -> Vec<usize> {
        lock(&self.log).uploads.clone()
    }

    /// Endless `ListProducts` streams the client cancelled.
    pub fn cancelled_streams(&self) -> usize {
        lock(&self.log).cancelled_streams
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The target: `grpc://127.0.0.1:<port>`, or `grpcs://localhost:<port>`
    /// with TLS (the name the certificates are issued for).
    pub fn target(&self) -> String {
        if self.tls {
            format!("grpcs://localhost:{}", self.addr.port())
        } else {
            format!("grpc://{}", self.addr)
        }
    }

    /// The base URL of gRPC-Web calls: `http://127.0.0.1:<port>`.
    pub fn web_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Set the health status reported for `service`.
    pub async fn set_health(&self, service: &str, status: ServingStatus) {
        self.health.set_service_status(service, status).await;
    }

    /// Report `NOT_SERVING` for `service`.
    pub async fn set_not_serving(&self, service: &str) {
        self.set_health(service, ServingStatus::NotServing).await;
    }

    /// Close the listening socket: new connections are refused.
    pub async fn stop(mut self) {
        self.task.abort();
        let _ = (&mut self.task).await;
    }
}

fn tls_config(tls: ServerTls, certs: &TestCerts) -> Option<ServerTlsConfig> {
    let identity = |cert: &str, key: &str| Identity::from_pem(cert, key);
    let config = ServerTlsConfig::new();
    Some(match tls {
        ServerTls::Plain => return None,
        ServerTls::Ca => config.identity(identity(&certs.server_cert_pem, &certs.server_key_pem)),
        ServerTls::SelfSigned => config.identity(identity(
            &certs.self_signed_cert_pem,
            &certs.self_signed_key_pem,
        )),
        ServerTls::Mutual => config
            .identity(identity(&certs.server_cert_pem, &certs.server_key_pem))
            .client_ca_root(Certificate::from_pem(&certs.ca_pem)),
    })
}
