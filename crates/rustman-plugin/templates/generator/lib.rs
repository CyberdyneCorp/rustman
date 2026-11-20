//! The `@@NAME@@` generator plugin. Use it in a request as
//! `{{$@@NAME@@.slug Hello World}}`: it joins its arguments into a
//! lowercase, dash-separated slug (`hello-world`).
//!
//! Grants: none, so what it returns depends only on its arguments.

#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use rustman_plugin_sdk::generator::{Guest, export};

struct Plugin;

impl Guest for Plugin {
    fn names() -> Vec<String> {
        vec!["slug".to_string()]
    }

    fn generate(name: String, args: Vec<String>) -> Result<String, String> {
        match name.as_str() {
            "slug" => Ok(slug(&args)),
            other => Err(format!("unknown generator {other}")),
        }
    }
}

fn slug(words: &[String]) -> String {
    let mut out = String::new();
    for c in words.join(" ").chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

export!(Plugin);
