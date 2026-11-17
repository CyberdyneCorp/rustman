//! The in-process test server (design D14) and helpers shared by the
//! integration tests of every Rustman crate. Nothing here touches the network
//! beyond the loopback interface. Never published.

pub mod certs;
pub mod oauth;
pub mod proxy;
pub mod raw;
pub mod routes;
pub mod server;

pub use certs::TestCerts;
pub use oauth::OAuthServer;
pub use proxy::TestProxy;
pub use server::{TestServer, Tls};
