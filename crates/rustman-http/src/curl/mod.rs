//! Curl interop (design D13): import a curl command as a request file, and
//! export a resolved request as a curl command.

mod export;
mod import;
mod options;

pub use export::{Exported, export_curl};
pub use import::{COMPRESSED_ACCEPT_ENCODING, CurlError, Imported, import_curl, import_curl_words};
