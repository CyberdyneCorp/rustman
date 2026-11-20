//! The `@@NAME@@` request kind plugin. Requests with `kind: echo` and a
//! `message` field run through it: it answers with the request itself,
//! which makes it a starting point for a protocol of your own (granted
//! `net:<host>`, it can call `rustman_plugin_sdk::http::fetch`).
//!
//! Grants: none.

#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use rustman_plugin_sdk::kind::{
    Editor, Guest, KindExchange, KindInfo, Pane, PaneField, ResponseView, export,
};

struct Plugin;

const SCHEMA: &str = r#"{"type":"object","properties":{"message":{"type":"string"}},"required":["message"]}"#;

impl Guest for Plugin {
    fn kinds() -> Vec<KindInfo> {
        vec![KindInfo {
            name: "echo".to_string(),
            schema: SCHEMA.to_string(),
            pane: Pane {
                fields: vec![PaneField {
                    name: "message".to_string(),
                    label: "Message".to_string(),
                    editor: Editor::Text,
                }],
                views: vec![ResponseView::Raw, ResponseView::JsonTree],
            },
        }]
    }

    /// `request` is the resolved request file as JSON.
    fn execute(kind: String, request: String) -> Result<KindExchange, String> {
        if kind != "echo" {
            return Err(format!("unknown kind {kind}"));
        }
        Ok(KindExchange {
            summary: "echo".to_string(),
            request_body: request.clone().into_bytes(),
            status: None,
            headers: Vec::new(),
            body: request.into_bytes(),
        })
    }
}

export!(Plugin);
