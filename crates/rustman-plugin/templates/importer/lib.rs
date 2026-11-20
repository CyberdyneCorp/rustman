//! The `@@NAME@@` importer plugin. Use it as
//! `rustman import lines requests.txt`: each `METHOD URL` line of the source
//! becomes a request file, which the host validates and writes.
//!
//! Grants: `fs:read:<dir>` for the folder holding the sources.

#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use rustman_plugin_sdk::importer::{Guest, ImportOutput, ImportedFile, export};

struct Plugin;

impl Guest for Plugin {
    fn formats() -> Vec<String> {
        vec!["lines".to_string()]
    }

    fn convert(format: String, source_path: String, source: Vec<u8>) -> Result<ImportOutput, String> {
        if format != "lines" {
            return Err(format!("unknown format {format}"));
        }
        let text = core::str::from_utf8(&source).map_err(|_| format!("{source_path} is not UTF-8"))?;
        let mut requests = Vec::new();
        for (index, line) in text.lines().map(str::trim).filter(|l| !l.is_empty()).enumerate() {
            let (method, url) = line
                .split_once(' ')
                .ok_or_else(|| format!("{source_path}: '{line}' is not METHOD URL"))?;
            requests.push(request(index + 1, method, url.trim()));
        }
        Ok(ImportOutput {
            requests,
            environments: Vec::new(),
        })
    }
}

fn request(number: usize, method: &str, url: &str) -> ImportedFile {
    let method = method.to_ascii_uppercase();
    ImportedFile {
        path: format!("{number}-{}.rman.yaml", method.to_ascii_lowercase()),
        content: format!(
            "version: 1\nkind: http\nname: request {number}\nmethod: {}\nurl: {}\n",
            quoted(&method),
            quoted(url)
        ),
    }
}

/// `text` as a single-quoted YAML scalar, so `{{variables}}`, `#` or `: `
/// in a URL stay part of it.
fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

export!(Plugin);
