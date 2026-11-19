//! What the server answers from: the document's model, its compiled
//! routes, request checks and stateful resources, and the hand-written
//! overrides, built once per load (spec: Document loading). The stateful
//! store is not part of it.

use rustman_explore::{ApiModel, ImportError, ImportOptions, Imported, import};

use crate::credentials::Credentials;
use crate::overrides::Overrides;
use crate::router::Router;
use crate::stateful::Resources;
use crate::validate::Checks;

/// An immutable, loaded document with its overrides.
#[derive(Debug)]
pub struct MockState {
    pub model: ApiModel,
    pub router: Router,
    pub checks: Checks,
    pub resources: Resources,
    pub overrides: Overrides,
    /// The credential headers and query parameters the log redacts.
    pub credentials: Credentials,
}

impl MockState {
    pub fn new(model: ApiModel) -> Self {
        Self {
            router: Router::new(&model),
            checks: Checks::new(&model),
            resources: Resources::new(&model),
            overrides: Overrides::default(),
            credentials: Credentials::new(&model),
            model,
        }
    }

    /// This state answering with `overrides` first.
    #[must_use]
    pub fn with_overrides(mut self, overrides: Overrides) -> Self {
        self.overrides = overrides;
        self
    }

    pub fn from_imported(imported: Imported) -> Self {
        Self::new(imported.model)
    }

    /// How many operations are served.
    pub fn operations(&self) -> usize {
        self.model.operations.len()
    }
}

/// Read `source` (a path or URL) with the `openapi-import` parser and `$ref`
/// policy; remote references only with `allow_remote_refs`. Structural
/// errors come back as [`ImportError::Invalid`] with JSON pointers.
pub async fn load(source: &str, allow_remote_refs: bool) -> Result<MockState, ImportError> {
    let options = ImportOptions {
        allow_remote_refs,
        ..ImportOptions::default()
    };
    import(source, &options).await.map(MockState::from_imported)
}
