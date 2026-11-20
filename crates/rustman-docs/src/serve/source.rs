//! The preview's builds: the workspace is loaded again for each one, its
//! secrets read from the store, and the HTML site built as `docs build`
//! would. Error and warning messages pass the redaction step before they
//! reach stderr or the page overlay.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use rustman_core::{ConfigError, SecretStore};

use super::state::{BuildOutcome, SiteBuilder};
use crate::build::{BuildOptions, BuildReport, Format, build, load_workspace, workspace_secrets};
use crate::error::DocsError;
use crate::examples::ExampleSource;
use crate::rendered::DocsRedactor;

/// Opens the secret store of a workspace root.
pub type OpenStore = Box<dyn Fn(&Path) -> Result<SecretStore, ConfigError> + Send + Sync>;

/// Builds the docs of the workspace at `root`.
pub struct PreviewSource {
    root: PathBuf,
    env: Option<String>,
    examples: ExampleSource,
    open_store: OpenStore,
    /// The redactor of the last workspace that loaded, for the messages of
    /// a build whose workspace no longer loads.
    last: Mutex<DocsRedactor>,
}

impl PreviewSource {
    pub fn new(
        root: PathBuf,
        env: Option<String>,
        examples: ExampleSource,
        open_store: OpenStore,
    ) -> Self {
        Self {
            root,
            env,
            examples,
            open_store,
            last: Mutex::new(DocsRedactor::default()),
        }
    }

    /// The workspace folder, which the preview watches.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn try_build(&self, out: &Path, warnings: &mut Vec<String>) -> Result<BuildReport, DocsError> {
        let workspace = load_workspace(&self.root, Some(&self.root))?;
        warnings.extend(workspace.warnings.iter().cloned());
        let store = (self.open_store)(&workspace.root)?;
        let (secrets, store_warnings) = workspace_secrets(&workspace, &store);
        warnings.extend(store_warnings);
        *self.last.lock().unwrap_or_else(PoisonError::into_inner) =
            DocsRedactor::for_workspace(&workspace, &secrets);
        let mut options = BuildOptions::new(out);
        options.shown_out = String::new();
        options.format = Format::Html;
        options.env.clone_from(&self.env);
        options.examples = self.examples;
        let mut report = build(&workspace, &secrets, &options)?;
        warnings.append(&mut report.warnings);
        Ok(report)
    }
}

impl SiteBuilder for PreviewSource {
    fn build_into(&self, out: &Path) -> BuildOutcome {
        let mut warnings = Vec::new();
        let result = self.try_build(out, &mut warnings);
        let redactor = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        let redact = |text: &str| redactor.render(text).as_str().to_owned();
        BuildOutcome {
            result: result.map_err(|e| redact(&e.to_string())),
            warnings: warnings.iter().map(|w| redact(w)).collect(),
        }
    }
}
