//! One import from a loaded source: plan it, apply it unless it is a dry
//! run, and redact what reports show. [`import`] is the core the CLI's
//! `rustman import` and the TUI's "Import…" action share; [`run`] is the
//! whole import as the TUI runs it (detect, lock, plan, apply).

use std::path::Path;

use rustman_core::model::KindRegistry;
use rustman_core::{Redactor, SecretBackend, SecretStore, Workspace};

use crate::error::ImportError;
use crate::ir::ImportedCollection;
use crate::plan::{OnConflict, Plan, PlanOptions, plan};
use crate::secrets::{Existing, is_secret_like};
use crate::source::{SourceOptions, load};
use crate::{apply, lock_workspace};

/// A planned (and, unless a dry run, applied) import.
pub struct Planned {
    pub plan: Plan,
    /// Redacts the secret values the plan moves out of requests.
    pub redactor: Redactor,
}

/// Plan `collection` and, unless `options.dry_run`, apply the plan,
/// storing moved secrets in `store`. The store's warnings stay in it for
/// the caller to take, whether or not the import succeeds.
pub fn import(
    collection: ImportedCollection,
    options: &PlanOptions<'_>,
    store: &SecretStore,
) -> Result<Planned, ImportError> {
    let plan = plan(collection, options)?;
    if !options.dry_run {
        let save = |batch: &[(&str, &_)]| store.set_many(batch);
        apply(&plan, options.root, &save)?;
    }
    let redactor = redactor(&plan);
    Ok(Planned { plan, redactor })
}

/// Reports name secrets only; redacting them as well guards every
/// output. Values that are ordinary words or numbers (`password`,
/// `123456`) are left out: reports show them only as data (URLs, names),
/// which redacting would corrupt.
fn redactor(plan: &Plan) -> Redactor {
    plan.secrets
        .iter()
        .filter(|m| is_secret_like(m.secret.expose()))
        .fold(Redactor::new(), |r, m| r.with(&m.secret))
}

/// What to import, and where.
#[derive(Clone, Debug)]
pub struct Session<'a> {
    /// The workspace root.
    pub root: &'a Path,
    /// The source file or folder; its format is detected.
    pub src: &'a Path,
    /// The source as messages and the summary name it.
    pub shown: &'a str,
    /// The environment that receives secrets moved out of requests; the
    /// caller picks it from the workspace's environments.
    pub environment: Option<&'a str>,
    pub backend: SecretBackend,
    /// Plan only: nothing is written.
    pub dry_run: bool,
}

/// What an import did (or would do).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    /// The report's summary, with secret-like values redacted.
    pub summary: String,
    /// Request files written (or to write).
    pub requests: usize,
    /// Something was unsupported, dropped or unmapped.
    pub has_losses: bool,
}

/// Run `session`. A real import holds the import lock from reading the
/// workspace to writing it; a dry run reads only. The secret store's
/// warnings are added to `warnings`, also when the import fails.
pub fn run(session: &Session<'_>, warnings: &mut Vec<String>) -> Result<Outcome, ImportError> {
    let _lock = if session.dry_run {
        None
    } else {
        Some(lock_workspace(session.root)?)
    };
    let registry = KindRegistry::default().with_opaque_fallback();
    let ws = Workspace::load_with(session.root, &registry)
        .map_err(|e| ImportError::invalid(&session.root.display().to_string(), e.to_string()))?;
    let shown = session.shown.to_owned();
    let (format, collection) = load(None, session.src, &shown, &SourceOptions::default())?;
    let store = SecretStore::open(&ws.root, session.backend);
    let lookup = |env: &str, name: &str| store.get(env, name).ok().flatten();
    let options = PlanOptions {
        root: &ws.root,
        source: shown,
        format,
        destination: None,
        on_conflict: OnConflict::Skip,
        environment: session.environment.map(str::to_owned),
        existing: Existing {
            environments: &ws.environments,
            lookup: &lookup,
        },
        dry_run: session.dry_run,
    };
    let imported = import(collection, &options, &store);
    warnings.extend(store.take_warnings());
    let Planned { plan, redactor } = imported?;
    Ok(Outcome {
        summary: redactor.text(&plan.report.summary_text()).into_owned(),
        requests: plan.report.summary.requests,
        has_losses: plan.report.has_losses(),
    })
}
