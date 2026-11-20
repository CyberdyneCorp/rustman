//! Code samples (api-docs-generation "Code samples", design D7): the
//! shared snippet engine of `rustman-http` in placeholder mode, so a page
//! shows the same code as `rustman export snippet --placeholders`.
//! `[docs].samples` selects and orders the languages; a `<id>.tmpl` file
//! in a `[docs].template_dirs` folder replaces a language's built-in
//! template for docs only.
//!
//! Samples never read a value of an environment or a secret: variables
//! stay as `{{name}}` (constants in code), names declared secret in any
//! environment become `«redacted:name»`, and dynamic values such as
//! `{{$uuid}}` are kept as written so builds are reproducible.

use std::cell::RefCell;
use std::path::Path;

use rustman_core::settings::Settings;
use rustman_core::vars::{Layer, Lookup, NoSecrets, Resolver, VarSource, resolve_placeholders};
use rustman_core::{RequestRef, Workspace};
use rustman_http::snippet::{
    Lang, SnippetOptions, TEMPLATE_CONTEXT_VERSION, custom_snippet, http_entry, snippet,
};

use crate::error::DocsError;
use crate::model::Sample;
use crate::rendered::DocsRedactor;

/// The languages of a page when `[docs].samples` is empty.
pub const DEFAULT_SAMPLES: [&str; 5] = ["curl", "httpie", "fetch", "python", "rust"];

/// The marker a custom template declares its context version with, on
/// its first line: `{# docs_context_version = 1 #}`.
const VERSION_KEY: &str = "docs_context_version";

/// The configured languages and their templates.
pub struct Samples {
    langs: Vec<SampleLang>,
    /// The names declared secret in any environment.
    secrets: Vec<String>,
    /// The first custom template that failed to render.
    error: RefCell<Option<DocsError>>,
}

struct SampleLang {
    id: String,
    lang: Lang,
    /// The template file's name, for error messages.
    template_name: String,
    /// A custom template from `[docs].template_dirs`.
    template: Option<String>,
}

impl Samples {
    /// The languages of `[docs].samples` with their custom templates. A
    /// template written for another context version is skipped with a
    /// warning.
    pub fn load(workspace: &Workspace, warnings: &mut Vec<String>) -> Result<Self, DocsError> {
        let settings = &workspace.settings.docs;
        let ids: Vec<&str> = if settings.samples.is_empty() {
            DEFAULT_SAMPLES.to_vec()
        } else {
            settings.samples.iter().map(String::as_str).collect()
        };
        let mut langs = Vec::new();
        for id in ids {
            let lang = Lang::parse(id).map_err(|e| {
                DocsError::Usage(format!(
                    "[docs].samples: {}",
                    e.to_string().replacen("snippet: ", "", 1)
                ))
            })?;
            let found = find_template(&workspace.root, &settings.template_dirs, id)?;
            let (template_name, template) = match found {
                Some((name, text)) => {
                    let checked = check_version(&name, &text, lang);
                    if let Err(warning) = &checked {
                        warnings.push(warning.clone());
                    }
                    (name, checked.ok())
                }
                None => (String::new(), None),
            };
            langs.push(SampleLang {
                id: id.to_owned(),
                lang,
                template_name,
                template,
            });
        }
        let mut secrets: Vec<String> = workspace
            .environments
            .iter()
            .flat_map(|e| e.secrets.iter().cloned())
            .collect();
        secrets.sort();
        secrets.dedup();
        Ok(Self {
            langs,
            secrets,
            error: RefCell::new(None),
        })
    }

    /// The first custom template error of the build, if any.
    pub fn take_error(&self) -> Option<DocsError> {
        self.error.borrow_mut().take()
    }

    /// The samples of `request`; none for kinds without an HTTP form. A
    /// custom template that fails is recorded for [`Samples::take_error`].
    pub fn for_request(
        &self,
        request: &RequestRef<'_>,
        settings: &Settings,
        r: &DocsRedactor,
    ) -> Vec<Sample> {
        match self.try_for_request(request, settings, r) {
            Ok(samples) => samples,
            Err(err) => {
                self.error.borrow_mut().get_or_insert(err);
                Vec::new()
            }
        }
    }

    fn try_for_request(
        &self,
        request: &RequestRef<'_>,
        settings: &Settings,
        r: &DocsRedactor,
    ) -> Result<Vec<Sample>, DocsError> {
        let Ok(converted) = http_entry(request) else {
            return Ok(Vec::new());
        };
        let target = match &converted {
            Some(entry) => RequestRef {
                entry,
                ..request.clone()
            },
            None => request.clone(),
        };
        let resolver = Resolver::for_request(settings, &target, None, &NoSecrets)
            .with(SecretNames(&self.secrets))
            .keep_dynamics();
        let Ok((resolved, vars)) = resolve_placeholders(&target, &resolver) else {
            return Ok(Vec::new());
        };
        let options = SnippetOptions {
            show_secrets: false,
            placeholders: Some(&vars),
            root: Some(target.root),
        };
        let mut out = Vec::new();
        for sample in &self.langs {
            let code = match &sample.template {
                Some(template) => {
                    custom_snippet(&resolved, sample.lang, &options, template)
                        .map_err(|e| DocsError::Template {
                            name: sample.template_name.clone(),
                            message: e.to_string(),
                        })?
                        .code
                }
                None => snippet(&resolved, sample.lang, &options).code,
            };
            out.push(Sample {
                id: r.render(&sample.id),
                fence: fence(sample.lang),
                code: r.render(&code),
            });
        }
        Ok(out)
    }
}

/// Answers "secret, not set" for every name declared secret in an
/// environment, so placeholder mode writes `«redacted:name»` for it
/// without reading any store.
struct SecretNames<'a>(&'a [String]);

impl VarSource for SecretNames<'_> {
    fn layer(&self) -> Layer {
        Layer::Environment
    }

    fn lookup(&self, name: &str) -> Option<Lookup> {
        self.0
            .iter()
            .any(|s| s == name)
            .then(|| Lookup::SecretNotSet {
                hint: String::new(),
            })
    }
}

/// The first `<dir>/<id>.tmpl` of `dirs` (relative to `root`), with its
/// name as messages show it.
fn find_template(
    root: &Path,
    dirs: &[String],
    id: &str,
) -> Result<Option<(String, String)>, DocsError> {
    for dir in dirs {
        let name = format!("{}/{id}.tmpl", dir.trim_end_matches(['/', '\\']));
        let path = root.join(&name);
        if path.is_file() {
            let text = std::fs::read_to_string(&path).map_err(|e| DocsError::io(&path, e))?;
            return Ok(Some((name, text.replace("\r\n", "\n"))));
        }
    }
    Ok(None)
}

/// `text` when its first line declares [`TEMPLATE_CONTEXT_VERSION`], else
/// the warning that says the built-in template is used instead.
fn check_version(name: &str, text: &str, lang: Lang) -> Result<String, String> {
    let first = text.lines().next().unwrap_or_default();
    let declared = first.find(VERSION_KEY).map(|at| {
        first[at + VERSION_KEY.len()..]
            .trim_start_matches([' ', '=', ':'])
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
    });
    let expected = TEMPLATE_CONTEXT_VERSION.to_string();
    match declared {
        Some(version) if version == expected => Ok(text.to_owned()),
        Some(version) => Err(format!(
            "docs: template {name} targets {VERSION_KEY} {}, this rustman provides {expected}; using the built-in {lang} template",
            if version.is_empty() { "?" } else { &version }
        )),
        None => Err(format!(
            "docs: template {name} does not declare {VERSION_KEY} on its first line (expected {{# {VERSION_KEY} = {expected} #}}); using the built-in {lang} template"
        )),
    }
}

/// The fenced-code info string of `lang` in Markdown output.
fn fence(lang: Lang) -> &'static str {
    match lang {
        Lang::Curl | Lang::Httpie | Lang::Wget => "sh",
        Lang::PythonRequests | Lang::PythonHttpx => "python",
        Lang::JsFetch | Lang::NodeAxios => "js",
        Lang::Go => "go",
        Lang::RustReqwest => "rust",
        Lang::Powershell => "powershell",
        Lang::Csharp => "csharp",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_versions_are_checked_on_the_first_line() {
        let ok = "{# docs_context_version = 1 #}\n{{ url }}";
        assert_eq!(check_version("t/go.tmpl", ok, Lang::Go).as_deref(), Ok(ok));
        assert!(check_version("t/go.tmpl", "{# docs_context_version: 1 #}\n", Lang::Go).is_ok());
        assert_eq!(
            check_version("t/go.tmpl", "{# docs_context_version = 0 #}\n", Lang::Go).unwrap_err(),
            "docs: template t/go.tmpl targets docs_context_version 0, this rustman provides 1; using the built-in go template"
        );
        let missing = check_version("t/go.tmpl", "{{ url }}\n", Lang::Go).unwrap_err();
        assert!(
            missing.contains("does not declare docs_context_version"),
            "{missing}"
        );
        assert!(check_version("t", "x\n{# docs_context_version = 1 #}", Lang::Go).is_err());
    }

    #[test]
    fn secret_names_are_never_values() {
        let names = vec!["token".to_owned()];
        let source = SecretNames(&names);
        assert!(matches!(
            source.lookup("token"),
            Some(Lookup::SecretNotSet { .. })
        ));
        assert!(source.lookup("base_url").is_none());
    }
}
