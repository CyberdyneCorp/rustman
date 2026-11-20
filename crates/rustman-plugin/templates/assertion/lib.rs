//! The `@@NAME@@` assertion plugin. Use it in a request as
//! `assertions: [{plugin: {fn: @@NAME@@/status-2xx}}]`: it passes when the
//! response status is 2xx.
//!
//! Grants: none.

#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use rustman_plugin_sdk::assertion::{Exchange, Guest, Verdict, export};

struct Plugin;

impl Guest for Plugin {
    fn functions() -> Vec<String> {
        vec!["status-2xx".to_string()]
    }

    /// `args` is the assertion's other keys as a JSON object.
    fn check(function: String, exchange: Exchange, _args: String) -> Verdict {
        match function.as_str() {
            "status-2xx" if (200..300).contains(&exchange.status) => Verdict::Pass,
            "status-2xx" => Verdict::Fail(format!("status {} is not 2xx", exchange.status)),
            other => Verdict::Error(format!("unknown function {other}")),
        }
    }
}

export!(Plugin);
