//! `rustman docs build`: workspace → page model → files → leak scan →
//! `--out`.

use std::path::{Component, Path, PathBuf};

use rustman_core::discovery::find_root;
use rustman_core::model::KindRegistry;
use rustman_core::{Secret, SecretStore, Workspace};

use crate::error::DocsError;
use crate::examples::{ExampleSource, Examples};
use crate::html::{self, Layouts, Logo};
use crate::leak;
use crate::markdown;
use crate::model::DocSite;
use crate::openapi::OpenApiIndex;
use crate::output::{self, Files};
use crate::publish::published_variables;
use crate::rendered::DocsRedactor;
use crate::samples::Samples;
use crate::site::{SiteInput, build_site};

/// What `--format` asks for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Format {
    #[default]
    Html,
    Markdown,
    Both,
}

/// The options of one build.
#[derive(Clone, Debug)]
pub struct BuildOptions {
    /// The output folder.
    pub out: PathBuf,
    /// How messages name it, with a final `/` (`site/`).
    pub shown_out: String,
    pub format: Format,
    /// The environment whose publishable values are shown (`--env`).
    pub env: Option<String>,
    pub examples: ExampleSource,
    pub force: bool,
}

impl BuildOptions {
    /// Options writing HTML into `out`, named as given.
    pub fn new(out: impl Into<PathBuf>) -> Self {
        let out = out.into();
        let mut shown_out = out.to_string_lossy().replace('\\', "/");
        if !shown_out.ends_with('/') {
            shown_out.push('/');
        }
        Self {
            out,
            shown_out,
            format: Format::Html,
            env: None,
            examples: ExampleSource::History,
            force: false,
        }
    }
}

/// What a build wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildReport {
    /// Paths written under `--out`, sorted.
    pub files: Vec<String>,
    /// Ids of the requests that published a recorded example.
    pub published_examples: Vec<String>,
    /// Problems that did not stop the build (a custom template written for
    /// another context version).
    pub warnings: Vec<String>,
}

/// Build the docs of `workspace`. `secrets` are the values of every
/// environment's secrets ([`workspace_secrets`]): they never reach a page.
pub fn build(
    workspace: &Workspace,
    secrets: &[Secret<String>],
    options: &BuildOptions,
) -> Result<BuildReport, DocsError> {
    let mut warnings = Vec::new();
    let redactor = DocsRedactor::for_workspace(workspace, secrets);
    let variables = published_variables(workspace, options.env.as_deref(), &redactor)?;
    output::check_target(&options.out, &options.shown_out, options.force)?;
    let examples = Examples::load(workspace, options.examples)?;
    let openapi = OpenApiIndex::load(workspace);
    let samples = Samples::load(workspace, &mut warnings)?;
    let site = build_site(SiteInput {
        workspace,
        redactor: &redactor,
        examples: &examples,
        openapi: &openapi,
        samples: &samples,
        variables,
    });
    if let Some(err) = samples.take_error() {
        return Err(err);
    }
    let mut files = render(workspace, &site, options.format)?;
    let leaks = leak::scan(&files, &redactor);
    for name in &leaks {
        files.remove(name);
    }
    output::write(&options.out, &files)?;
    for name in &leaks {
        output::remove_file(&options.out, name)?;
    }
    if !leaks.is_empty() {
        let shown = leaks.iter().map(|n| format!("{}{n}", options.shown_out));
        return Err(DocsError::Leak(shown.collect()));
    }
    Ok(BuildReport {
        files: files.into_keys().collect(),
        published_examples: site.published_examples,
        warnings,
    })
}

/// The files of `site` in `format`. With `both`, the HTML and Markdown
/// files sit side by side (`index.html` and `README.md`).
fn render(workspace: &Workspace, site: &DocSite, format: Format) -> Result<Files, DocsError> {
    let mut files = Files::new();
    if format != Format::Markdown {
        let settings = &workspace.settings.docs;
        let layouts = Layouts::load(&workspace.root, &settings.template_dirs)?;
        let logo = settings
            .logo
            .as_deref()
            .map(|path| load_logo(&workspace.root, path))
            .transpose()?;
        files.extend(html::render(site, &layouts, logo.as_ref())?);
    }
    if format != Format::Html {
        files.extend(markdown::render(site));
    }
    Ok(files)
}

/// The workspace found from `start` (or at `explicit`). Request files of
/// kinds this build has no loader for are read as opaque requests, so they
/// still get a generic page instead of failing the build.
pub fn load_workspace(start: &Path, explicit: Option<&Path>) -> Result<Workspace, DocsError> {
    let root = find_root(start, explicit)?;
    let registry = KindRegistry::default().with_opaque_fallback();
    Ok(Workspace::load_with(&root, &registry)?)
}

/// The values of every environment's secrets held in `store`, and the
/// store's warnings. Missing values are skipped.
pub fn workspace_secrets(
    workspace: &Workspace,
    store: &SecretStore,
) -> (Vec<Secret<String>>, Vec<String>) {
    let mut secrets = Vec::new();
    let mut warnings = Vec::new();
    for env in &workspace.environments {
        for name in &env.secrets {
            match store.get(&env.name, name) {
                Ok(Some(secret)) => secrets.push(secret),
                Ok(None) => {}
                Err(err) => warnings.push(format!("secret '{name}' of '{}': {err}", env.name)),
            }
        }
    }
    warnings.extend(store.take_warnings());
    (secrets, warnings)
}

/// The image types `[docs].logo` may have. SVG is refused: opened on its
/// own from a published site, an SVG runs its scripts on the docs origin.
const LOGO_EXTENSIONS: [&str; 6] = ["png", "jpg", "jpeg", "gif", "webp", "ico"];

/// `[docs].logo`, a raster image inside the workspace.
fn load_logo(root: &Path, path: &str) -> Result<Logo, DocsError> {
    let relative = Path::new(path);
    let inside = relative
        .components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
    if !inside {
        return Err(DocsError::Usage(format!(
            "[docs].logo '{path}' must be a path inside the workspace"
        )));
    }
    let extension = relative
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .filter(|e| LOGO_EXTENSIONS.contains(&e.as_str()))
        .ok_or_else(|| {
            DocsError::Usage(format!(
                "[docs].logo '{path}' must be a PNG, JPEG, GIF, WebP or ICO image (SVG is refused: it can run scripts on the published site)"
            ))
        })?;
    let full = root.join(relative);
    let bytes = std::fs::read(&full).map_err(|e| DocsError::io(&full, e))?;
    Ok(Logo { extension, bytes })
}
