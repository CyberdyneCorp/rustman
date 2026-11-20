//! The `@@NAME@@` transformer plugin. Use it in a request as
//! `transform: {request: [@@NAME@@/stamp], response: [@@NAME@@/strip-xssi]}`:
//! `stamp` adds an `X-Stamped` header to the request, and `strip-xssi`
//! removes the `)]}'` line some JSON APIs put before their response body.
//!
//! Grants: none.

#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use rustman_plugin_sdk::transformer::{Guest, Header, Message, export};

struct Plugin;

const XSSI_PREFIX: &[u8] = b")]}'";

impl Guest for Plugin {
    fn request_transformers() -> Vec<String> {
        vec!["stamp".to_string()]
    }

    fn response_transformers() -> Vec<String> {
        vec!["strip-xssi".to_string()]
    }

    fn transform_request(name: String, _url: String, message: Message) -> Result<Message, String> {
        if name != "stamp" {
            return Err(format!("unknown request transformer {name}"));
        }
        let mut message = message;
        message.headers.push(Header {
            name: "X-Stamped".to_string(),
            value: "@@NAME@@".to_string(),
        });
        Ok(message)
    }

    fn transform_response(name: String, _status: u16, message: Message) -> Result<Message, String> {
        if name != "strip-xssi" {
            return Err(format!("unknown response transformer {name}"));
        }
        let mut message = message;
        if let Some(rest) = message.body.strip_prefix(XSSI_PREFIX) {
            let start = rest.iter().position(|b| *b == b'\n').map_or(rest.len(), |at| at + 1);
            message.body = rest[start..].to_vec();
        }
        Ok(message)
    }
}

export!(Plugin);
