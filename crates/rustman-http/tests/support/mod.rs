//! The in-process test server (design D14) and helpers for the integration
//! tests. Nothing here touches the network beyond the loopback interface.
#![allow(dead_code, unused_imports)]

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
pub mod request;
