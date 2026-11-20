//! The `@@NAME@@` report plugin. Use it as
//! `rustman report render report.json --format @@NAME@@/text -o report.txt`
//! or `--reporter @@NAME@@/text:report.txt`: it receives the redacted
//! `rustman.report/v1` JSON document and returns the bytes to write.
//!
//! Grants: none.

#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use rustman_plugin_sdk::report::{Guest, export};

struct Plugin;

impl Guest for Plugin {
    fn formats() -> Vec<String> {
        vec!["text".to_string()]
    }

    fn render(format: String, report: String) -> Result<Vec<u8>, String> {
        if format != "text" {
            return Err(format!("unknown format {format}"));
        }
        Ok(format!("rustman report\n\n{report}\n").into_bytes())
    }
}

export!(Plugin);
