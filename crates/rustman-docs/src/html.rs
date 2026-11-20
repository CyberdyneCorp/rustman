//! The HTML renderer (design D2, D4, D5): embedded `minijinja` layouts that
//! `[docs].template_dirs` may override by name, a content-hashed stylesheet
//! and script, and the search index. Nothing refers outside the site.

use std::path::Path;

use minijinja::syntax::SyntaxConfig;
use minijinja::{AutoEscape, Environment, Value, context};
use rustman_core::snapshot::sha256_hex;
use serde::Serialize;

use crate::error::DocsError;
use crate::model::DocSite;
use crate::output::Files;
use crate::search;

/// The layout templates, by the name an override uses.
pub const LAYOUTS: [(&str, &str); 4] = [
    ("layout.html", include_str!("templates/layout.html")),
    ("index.html", include_str!("templates/index.html")),
    ("endpoint.html", include_str!("templates/endpoint.html")),
    ("reference.html", include_str!("templates/reference.html")),
];

const CSS: &str = include_str!("assets/app.css");
const JS: &str = include_str!("assets/app.js");

/// The layout templates of a build: the built-in ones, each replaced by a
/// file of the same name in the first template dir that has one.
pub struct Layouts {
    templates: Vec<(&'static str, String)>,
}

impl Layouts {
    /// The built-in layouts with overrides from `dirs` (relative to `root`).
    pub fn load(root: &Path, dirs: &[String]) -> Result<Self, DocsError> {
        let mut templates = Vec::new();
        for (name, builtin) in LAYOUTS {
            let custom = dirs
                .iter()
                .map(|dir| root.join(dir).join(name))
                .find(|path| path.is_file());
            let source = match custom {
                Some(path) => {
                    std::fs::read_to_string(&path).map_err(|e| DocsError::io(&path, e))?
                }
                None => builtin.to_owned(),
            };
            templates.push((name, lf(&source)));
        }
        Ok(Self { templates })
    }

    /// The built-in layouts only.
    pub fn builtin() -> Self {
        let templates = LAYOUTS.iter().map(|(n, s)| (*n, lf(s))).collect();
        Self { templates }
    }

    fn environment(&self) -> Result<Environment<'_>, DocsError> {
        let mut env = Environment::new();
        // A block tag on a line of its own writes nothing, not even the
        // line break (custom layouts render the same way).
        let syntax = SyntaxConfig::builder()
            .trim_blocks(true)
            .lstrip_blocks(true)
            .build()
            .expect("the default delimiters are valid");
        env.set_syntax(syntax);
        env.set_auto_escape_callback(|_| AutoEscape::Html);
        env.set_formatter(format_value);
        for (name, source) in &self.templates {
            env.add_template(name, source)
                .map_err(|e| template_error(name, &e))?;
        }
        Ok(env)
    }
}

/// Strings are escaped with [`escape_html`] (minijinja's own escaping also
/// writes `/` as `&#x2f;`, which makes URL templates unreadable in the
/// source); safe strings and other values are written as minijinja does.
fn format_value(
    out: &mut minijinja::Output,
    state: &mut minijinja::State,
    value: &Value,
) -> Result<(), minijinja::Error> {
    match value.as_str() {
        Some(text) if !value.is_safe() && *state.auto_escape() == AutoEscape::Html => out
            .write_str(&escape_html(text))
            .map_err(|e| minijinja::Error::new(minijinja::ErrorKind::WriteFailure, e.to_string())),
        _ => minijinja::escape_formatter(out, state, value),
    }
}

/// `text` safe in HTML text and in quoted attribute values.
pub fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

/// An image shown in the header, copied into the site.
pub struct Logo {
    /// The file extension, such as `png`.
    pub extension: String,
    pub bytes: Vec<u8>,
}

#[derive(Serialize)]
struct Assets {
    css: String,
    js: String,
    index: String,
    logo: Option<String>,
}

/// Every file of the HTML site.
pub fn render(site: &DocSite, layouts: &Layouts, logo: Option<&Logo>) -> Result<Files, DocsError> {
    let mut files = Files::new();
    let index = search_index_json(site);
    let assets = Assets {
        css: hashed(&mut files, "app", "css", lf(CSS).as_bytes()),
        js: hashed(&mut files, "app", "js", lf(JS).as_bytes()),
        index: hashed(
            &mut files,
            "search-index",
            "js",
            format!("window.RUSTMAN_SEARCH_INDEX = {};\n", index.trim_end()).as_bytes(),
        ),
        logo: logo.map(|l| hashed(&mut files, "logo", &l.extension, &l.bytes)),
    };
    files.insert("search-index.json".to_owned(), index.into_bytes());
    let env = layouts.environment()?;
    let version = rustman_core::VERSION;
    let site_value = Value::from(minijinja::value::Serde(site));
    let assets = Value::from(minijinja::value::Serde(&assets));
    let page =
        context! { site => site_value.clone(), assets => assets.clone(), version, root => "" };
    files.insert(
        "index.html".to_owned(),
        render_page(&env, "index.html", page)?,
    );
    for endpoint in site.endpoints() {
        let name = format!("endpoints/{}.html", endpoint.slug);
        let endpoint = Value::from(minijinja::value::Serde(endpoint));
        let page = context! { site => site_value.clone(), assets => assets.clone(), version, endpoint, root => "../" };
        files.insert(name, render_page(&env, "endpoint.html", page)?);
    }
    for set in &site.references {
        let set_value = Value::from(minijinja::value::Serde(set));
        for reference in &set.pages {
            let name = format!("reference/{}.html", reference.slug);
            let reference = Value::from(minijinja::value::Serde(reference));
            let page = context! { site => site_value.clone(), assets => assets.clone(), version, set => set_value.clone(), reference, root => "../" };
            files.insert(name, render_page(&env, "reference.html", page)?);
        }
    }
    Ok(files)
}

/// The search index as pretty JSON with a final newline. `<`, `>` and `&`
/// are written as `\u` escapes, so the index is inert inside HTML too.
pub fn search_index_json(site: &DocSite) -> String {
    let index = search::index(site);
    let json = serde_json::to_string_pretty(&index).unwrap_or_default();
    let mut json = json
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026");
    json.push('\n');
    json
}

fn render_page(env: &Environment<'_>, name: &str, ctx: Value) -> Result<Vec<u8>, DocsError> {
    let template = env
        .get_template(name)
        .map_err(|e| template_error(name, &e))?;
    let mut html = template.render(ctx).map_err(|e| template_error(name, &e))?;
    html = lf(&html);
    if !html.ends_with('\n') {
        html.push('\n');
    }
    Ok(html.into_bytes())
}

/// Add `bytes` as `assets/<stem>.<hash8>.<extension>` and return that path.
fn hashed(files: &mut Files, stem: &str, extension: &str, bytes: &[u8]) -> String {
    let hash = sha256_hex(bytes);
    let name = format!("assets/{stem}.{}.{extension}", &hash[..8]);
    files.insert(name.clone(), bytes.to_vec());
    name
}

/// `text` with `\n` line endings, whatever the checkout did to it.
fn lf(text: &str) -> String {
    text.replace("\r\n", "\n")
}

fn template_error(name: &str, err: &minijinja::Error) -> DocsError {
    DocsError::Template {
        name: name.to_owned(),
        message: err.to_string(),
    }
}
