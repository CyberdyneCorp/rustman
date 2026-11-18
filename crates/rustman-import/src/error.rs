//! Why an import could not run. Messages never contain secret values.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("import: cannot detect format of {0} (expected postman, insomnia, bruno, har, http)")]
    CannotDetect(String),
    #[error("import: {file}: {message}")]
    Invalid { file: String, message: String },
    #[error("import: source exceeds {} (use --max-size)", size_text(*.limit))]
    TooLarge { limit: u64 },
    #[error("import: --into must be inside the workspace")]
    OutsideWorkspace,
    /// A sensitive field reached the writer with a literal value.
    #[error("import: refusing to write {path}: a secret field has a literal value")]
    LiteralSecret { path: String },
    #[error("import: {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("import: {0}")]
    Secrets(#[from] rustman_core::SecretError),
    /// A workspace path that cannot be exported.
    #[error("export: {0}")]
    Export(String),
}

impl ImportError {
    pub fn invalid(file: &str, message: impl Into<String>) -> Self {
        Self::Invalid {
            file: file.to_owned(),
            message: message.into(),
        }
    }

    pub fn io(path: &std::path::Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

/// `100 MiB`, `1 KiB` or `12 bytes`.
pub fn size_text(bytes: u64) -> String {
    const UNITS: [(u64, &str); 3] = [(1 << 30, "GiB"), (1 << 20, "MiB"), (1 << 10, "KiB")];
    for (unit, name) in UNITS {
        if bytes >= unit && bytes.is_multiple_of(unit) {
            return format!("{} {name}", bytes / unit);
        }
    }
    format!("{bytes} bytes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_follow_the_spec() {
        assert_eq!(
            ImportError::CannotDetect("data.json".into()).to_string(),
            "import: cannot detect format of data.json (expected postman, insomnia, bruno, har, http)"
        );
        assert_eq!(
            ImportError::TooLarge { limit: 100 << 20 }.to_string(),
            "import: source exceeds 100 MiB (use --max-size)"
        );
        assert_eq!(size_text(1536), "1536 bytes");
        assert_eq!(size_text(2 << 30), "2 GiB");
    }
}
