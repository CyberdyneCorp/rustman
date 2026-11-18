//! Importing third-party API collections into a Rustman workspace
//! (capability `collection-import`).
//!
//! The pipeline (design D1) is a library so every front-end, and other
//! producers of the neutral IR (a traffic recorder, script migration), share
//! one implementation: a source parser builds an [`ir::ImportedCollection`];
//! [`plan::plan`] translates variables, extracts secrets, suggests
//! assertions and lays out files; [`apply::apply`] stores the secrets and
//! writes the files. Secret values never reach a planned file or the report.

pub mod apply;
pub mod bruno;
pub mod detect;
pub mod error;
pub mod export;
pub mod har;
pub mod http;
pub mod insomnia;
pub mod ir;
pub mod migration;
pub mod pathvars;
pub mod plan;
pub mod postman;
pub mod render;
pub mod report;
pub mod scripts;
pub mod secrets;
pub mod session;
pub mod slug;
pub mod source;
pub mod translate;

mod common;
mod envs;

use std::io::Read;
use std::path::Path;

pub use apply::{IMPORT_LOCK, apply, lock_workspace};
pub use detect::{Format, detect, detect_dir};
pub use error::ImportError;
pub use har::HarFilter;
pub use plan::{OnConflict, Plan, PlanOptions, PlannedFile, plan};
pub use report::Report;
pub use source::{SourceOptions, load};

/// Sources larger than this are refused unless `--max-size` allows them.
pub const DEFAULT_MAX_SIZE: u64 = 100 << 20;

/// Read the source file at `path`, refusing one larger than `max_size`
/// bytes before reading it.
pub fn read_source(path: &Path, max_size: u64) -> Result<String, ImportError> {
    let io = |e| ImportError::io(path, e);
    let file = std::fs::File::open(path).map_err(io)?;
    let len = file.metadata().map_err(io)?.len();
    if len > max_size {
        return Err(ImportError::TooLarge { limit: max_size });
    }
    let mut bytes = Vec::new();
    file.take(max_size + 1)
        .read_to_end(&mut bytes)
        .map_err(io)?;
    if bytes.len() as u64 > max_size {
        return Err(ImportError::TooLarge { limit: max_size });
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| ImportError::invalid(&path.display().to_string(), "not UTF-8 text"))?;
    Ok(text
        .strip_prefix('\u{feff}')
        .map(str::to_owned)
        .unwrap_or(text))
}

/// The format `format` (or, for `None`, the one detected from the content)
/// of the source `text`, shown as `shown` in messages.
pub fn resolve_format(
    format: Option<Format>,
    text: &str,
    shown: &str,
) -> Result<Format, ImportError> {
    match format {
        Some(format) => Ok(format),
        None => detect(text).ok_or_else(|| ImportError::CannotDetect(shown.to_owned())),
    }
}
