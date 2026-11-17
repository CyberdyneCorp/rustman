//! The in-process test server (design D14, from `rustman-test-support`) and
//! helpers for the integration tests. Nothing here touches the network
//! beyond the loopback interface.
#![allow(dead_code, unused_imports)]

pub mod collection;
pub mod recorder;
pub mod request;

pub use rustman_test_support::{
    OAuthServer, TestCerts, TestProxy, TestServer, Tls, certs, oauth, proxy, raw, routes, server,
};
