//! Building `ResolvedRequest`s for the engine tests.

use std::path::Path;

use rustman_core::model::RequestSettings;
use rustman_core::vars::{
    Resolved, ResolvedBody, ResolvedFile, ResolvedPair, ResolvedRequest, TlsFiles,
};

/// A `GET` of `url` with no headers, body or settings.
pub fn get(url: &str) -> ResolvedRequest {
    ResolvedRequest {
        method: "GET".into(),
        url: Resolved::literal(url),
        query: Vec::new(),
        headers: Vec::new(),
        body: ResolvedBody::None,
        auth: None,
        settings: RequestSettings::default(),
        tls: TlsFiles::default(),
        secrets: Vec::new(),
    }
}

pub fn pair(name: &str, value: &str) -> ResolvedPair {
    ResolvedPair {
        name: Resolved::literal(name),
        value: Resolved::literal(value),
    }
}

/// `contents` written to `dir/name`, as a request file reference.
pub fn file(dir: &Path, name: &str, contents: impl AsRef<[u8]>) -> Option<ResolvedFile> {
    let path = dir.join(name);
    std::fs::write(&path, contents).unwrap();
    Some(ResolvedFile {
        raw: name.into(),
        path,
    })
}

/// The values of header `name` in an `/echo` response.
pub fn echoed_header(echo: &serde_json::Value, name: &str) -> Vec<String> {
    echo["headers"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|h| h[0] == name)
        .map(|h| h[1].as_str().unwrap().to_owned())
        .collect()
}
