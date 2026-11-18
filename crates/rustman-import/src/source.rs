//! Reading a source: one file, or a folder (a Bruno collection, or `.http`
//! files with their environment files), within the size limit.

use std::path::Path;

use crate::detect::{Format, detect, detect_dir};
use crate::error::ImportError;
use crate::har::HarFilter;
use crate::ir::ImportedCollection;
use crate::{DEFAULT_MAX_SIZE, bruno, har, http, read_source};

/// How a source is read.
#[derive(Clone, Debug)]
pub struct SourceOptions {
    /// The most bytes read from the source, all files together.
    pub max_size: u64,
    /// Which HAR entries to import (HAR sources only).
    pub har: HarFilter,
}

impl Default for SourceOptions {
    fn default() -> Self {
        Self {
            max_size: DEFAULT_MAX_SIZE,
            har: HarFilter::default(),
        }
    }
}

/// Reads the files of a source, counting their bytes against one limit.
#[derive(Debug)]
pub struct Reader {
    limit: u64,
    used: u64,
}

impl Reader {
    pub fn new(limit: u64) -> Self {
        Self { limit, used: 0 }
    }

    /// The text of `path`; refused when it would take the source over the
    /// limit.
    pub fn read(&mut self, path: &Path) -> Result<String, ImportError> {
        let len = std::fs::metadata(path)
            .map_err(|e| ImportError::io(path, e))?
            .len();
        if self.used.saturating_add(len) > self.limit {
            return Err(ImportError::TooLarge { limit: self.limit });
        }
        let text = read_source(path, self.limit - self.used).map_err(|e| match e {
            ImportError::TooLarge { .. } => ImportError::TooLarge { limit: self.limit },
            other => other,
        })?;
        self.used += text.len() as u64;
        Ok(text)
    }
}

/// Read and parse the source at `path` (shown as `shown`) in `format`, or
/// in the format detected from its content when `None`.
pub fn load(
    format: Option<Format>,
    path: &Path,
    shown: &str,
    o: &SourceOptions,
) -> Result<(Format, ImportedCollection), ImportError> {
    let mut reader = Reader::new(o.max_size);
    if path.is_dir() {
        let format = match format {
            Some(f) => f,
            None => detect_dir(path).ok_or_else(|| ImportError::CannotDetect(shown.to_owned()))?,
        };
        let collection = match format {
            Format::Bruno => bruno::parse_dir(path, shown, &mut reader)?,
            Format::Http => http::parse_dir(path, shown, &mut reader)?,
            other => {
                let message = format!("is a folder; {} sources are files", other.as_str());
                return Err(ImportError::invalid(shown, message));
            }
        };
        return Ok((format, collection));
    }
    let text = reader.read(path)?;
    let format = match format {
        Some(f) => f,
        None => detect(&text).ok_or_else(|| ImportError::CannotDetect(shown.to_owned()))?,
    };
    let collection = match format {
        Format::Har => har::parse(&text, shown, &o.har)?,
        Format::Http => http::parse_file_with_environments(&text, path, shown, &mut reader)?,
        other => other.parse(&text, shown)?,
    };
    Ok((format, collection))
}
