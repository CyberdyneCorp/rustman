//! Export (task group 5, design D6): a collection folder or request file
//! of a workspace, through the neutral IR, as a Postman v2.1 collection or
//! a `.http` file. Request files only hold `{{name}}` placeholders for
//! secrets, and placeholders are exported as they are, so no secret value
//! is ever read or written.

mod http;
mod postman;
mod workspace;

use std::path::Path;

use rustman_core::Workspace;

pub use postman::SCHEMA as POSTMAN_SCHEMA;
pub use workspace::collect;

use crate::error::ImportError;

/// What to export to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Postman,
    Http,
}

impl Target {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Postman => "postman",
            Self::Http => "http",
        }
    }
}

/// An export: the file text, and one warning per feature the target
/// cannot hold.
#[derive(Debug)]
pub struct Exported {
    pub text: String,
    pub warnings: Vec<String>,
}

/// Export the collection folder or request file at `rel` (relative to the
/// workspace root) of `ws` to `target`.
pub fn export(ws: &Workspace, rel: &Path, target: Target) -> Result<Exported, ImportError> {
    let collection = collect(ws, rel)?;
    let (text, notes) = match target {
        Target::Postman => postman::write(collection),
        Target::Http => http::write(collection),
    };
    let warnings = notes
        .iter()
        .map(|n| format!("export: {}: {}", n.location.display(), n.message))
        .collect();
    Ok(Exported { text, warnings })
}
