//! Reading the local files a contract command takes (Pact files, run
//! reports) with the same size limit as OpenAPI documents, so a huge file
//! is refused instead of being held in memory.

use std::io::{self, Read};
use std::path::Path;

pub use rustman_explore::openapi::source::MAX_DOCUMENT_BYTES as MAX_INPUT_BYTES;

/// The UTF-8 text of `path`, refused when it is larger than `limit` bytes.
pub fn read_text(path: &Path, limit: u64) -> io::Result<String> {
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("larger than the {limit}-byte limit"),
        ));
    }
    String::from_utf8(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "not UTF-8 text"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_over_the_limit_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.json");
        std::fs::write(&path, "0123456789").unwrap();
        assert_eq!(read_text(&path, 10).unwrap(), "0123456789");
        let err = read_text(&path, 9).unwrap_err();
        assert_eq!(err.to_string(), "larger than the 9-byte limit");
        std::fs::write(&path, [0xff, 0xfe]).unwrap();
        assert_eq!(
            read_text(&path, 10).unwrap_err().to_string(),
            "not UTF-8 text"
        );
    }
}
