//! A workspace request as a snippet, the pipeline `rustman export
//! snippet` and the TUI's "Copy as…" share: a GraphQL request becomes its
//! POST, variables are resolved (or kept as placeholders), provider
//! secrets are shown redacted without being fetched, and the snippet is
//! rendered. The secret store's warnings come with the
//! snippet's own.

use rustman_core::settings::Settings;
use rustman_core::vars::{ResolveError, Resolver, resolve_request};
use rustman_core::{Environment, RequestRef, SecretStore};

use super::{
    Lang, Snippet, SnippetError, SnippetOptions, http_entry, resolve_placeholders, snippet,
};

/// How [`request_snippet`] makes a snippet.
#[derive(Clone, Copy)]
pub struct RequestSnippet<'a> {
    pub settings: &'a Settings,
    pub environment: Option<&'a Environment>,
    pub secrets: &'a SecretStore,
    pub lang: Lang,
    /// Show secret values instead of `«redacted:<name>»`.
    pub show_secrets: bool,
    /// Keep non-secret variables as placeholders.
    pub placeholders: bool,
}

/// Why a request's snippet cannot be made.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RequestSnippetError {
    #[error(transparent)]
    Snippet(#[from] SnippetError),
    #[error(transparent)]
    Resolve(#[from] ResolveError),
}

/// `target` as a snippet. Its warnings start with the secret store's.
pub fn request_snippet(
    target: RequestRef<'_>,
    how: &RequestSnippet<'_>,
) -> Result<Snippet, RequestSnippetError> {
    let converted = http_entry(&target)?;
    let target = match &converted {
        Some(entry) => RequestRef { entry, ..target },
        None => target,
    };
    // A snippet never contacts a secret provider: references are shown as
    // `«redacted:<provider>/<path>[#field]»`.
    let resolver = Resolver::for_request(how.settings, &target, how.environment, how.secrets)
        .redacting_secret_refs();
    let resolved = if how.placeholders {
        resolve_placeholders(&target, &resolver).map(|(r, vars)| (r, Some(vars)))
    } else {
        resolve_request(&target, &resolver).map(|r| (r, None))
    };
    let mut warnings = how.secrets.take_warnings();
    let (resolved, vars) = resolved?;
    let options = SnippetOptions {
        show_secrets: how.show_secrets,
        placeholders: vars.as_deref(),
        root: Some(target.root),
    };
    let made = snippet(&resolved, how.lang, &options);
    warnings.extend(made.warnings);
    Ok(Snippet {
        code: made.code,
        warnings,
    })
}
