//! The `@@NAME@@` auth plugin. Use it in a request as
//! `auth: {type: plugin, scheme: @@NAME@@/v1, key_var: hmac_key}`: it signs
//! `<unix seconds>\n<method>\n<url>` with HMAC-SHA256 under the secret named
//! by `key_var` and sets the `X-Signature` and `X-Timestamp` headers.
//!
//! Grants: `clock` and `secrets:read:<key_var>`.

#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use hmac::{Hmac, KeyInit, Mac};
use rustman_plugin_sdk::auth::{AuthOutput, AuthRequest, Guest, Header, export};
use rustman_plugin_sdk::{clock, describe, secrets};
use sha2::Sha256;

struct Plugin;

impl Guest for Plugin {
    fn schemes() -> Vec<String> {
        vec!["v1".to_string()]
    }

    fn sign(request: AuthRequest) -> Result<AuthOutput, String> {
        let key_var = request
            .params
            .iter()
            .find(|param| param.name == "key_var")
            .map_or("hmac_key", |param| param.value.as_str());
        let key = secrets::lookup(key_var)
            .ok_or_else(|| format!("no secret {key_var}"))?
            .reveal()
            .map_err(|e| describe(&e))?;
        let now = clock::now().map_err(|e| describe(&e))?.seconds;
        let message = format!("{now}\n{}\n{}", request.method, request.url);
        let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes())
            .map_err(|_| "invalid key".to_string())?;
        mac.update(message.as_bytes());
        let signature = hex(&mac.finalize().into_bytes());
        Ok(AuthOutput {
            set_headers: vec![
                header("X-Signature", signature),
                header("X-Timestamp", now.to_string()),
            ],
            add_query: Vec::new(),
            url: None,
        })
    }
}

fn header(name: &str, value: String) -> Header {
    Header {
        name: name.to_string(),
        value,
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

export!(Plugin);
