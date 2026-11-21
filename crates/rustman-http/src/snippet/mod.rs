//! Code snippets (runner-parity design D7 and D10): a resolved request as
//! code for one of eleven languages, rendered from one `minijinja` template
//! per language over the same redacted view. The curl target is the
//! `export curl` command, with curl's own Digest and SigV4 options.
//!
//! Secrets are redacted unless shown on purpose. In placeholder mode
//! (see [`resolve_placeholders`]) variables stay as `{{name}}` in shell
//! tools and become constants in code; secrets are never read.

mod code;
mod httpie;
mod request;
pub(crate) mod url;
mod view;

use std::fmt;
use std::path::Path;

use minijinja::syntax::SyntaxConfig;
use minijinja::value::Serde;
use minijinja::{Environment, UndefinedBehavior, Value};
use rustman_core::RequestRef;
use rustman_core::collection::RequestEntry;
use rustman_core::model::{Auth, Request, graphql_as_http, jsonrpc_as_http};
pub use rustman_core::vars::resolve_placeholders;
use rustman_core::vars::{PlaceholderVar, ResolvedRequest};

use crate::curl::{CurlOptions, curl_command};
use crate::curl::{NUL_WARNING, nul_refusal};
use crate::shown::Shown;
use code::Syntax;
pub(crate) use code::shell_word;
use view::{Context, View};

pub use request::{RequestSnippet, RequestSnippetError, request_snippet};

/// A snippet language.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Curl,
    Httpie,
    Wget,
    PythonRequests,
    PythonHttpx,
    JsFetch,
    NodeAxios,
    Go,
    RustReqwest,
    Powershell,
    Csharp,
}

/// Other names accepted for a language: the docs generator's ids.
const ALIASES: [(&str, Lang); 3] = [
    ("fetch", Lang::JsFetch),
    ("python", Lang::PythonRequests),
    ("rust", Lang::RustReqwest),
];

impl Lang {
    /// Every language, in the order `--list` prints them.
    pub const ALL: [Lang; 11] = [
        Lang::Curl,
        Lang::Httpie,
        Lang::Wget,
        Lang::PythonRequests,
        Lang::PythonHttpx,
        Lang::JsFetch,
        Lang::NodeAxios,
        Lang::Go,
        Lang::RustReqwest,
        Lang::Powershell,
        Lang::Csharp,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::Curl => "curl",
            Self::Httpie => "httpie",
            Self::Wget => "wget",
            Self::PythonRequests => "python-requests",
            Self::PythonHttpx => "python-httpx",
            Self::JsFetch => "js-fetch",
            Self::NodeAxios => "node-axios",
            Self::Go => "go",
            Self::RustReqwest => "rust-reqwest",
            Self::Powershell => "powershell",
            Self::Csharp => "csharp",
        }
    }

    /// The language with id or alias `id`.
    pub fn parse(id: &str) -> Result<Self, SnippetError> {
        Self::ALL
            .into_iter()
            .find(|l| l.id() == id)
            .or_else(|| ALIASES.iter().find(|(a, _)| *a == id).map(|(_, l)| *l))
            .ok_or_else(|| SnippetError::UnknownLanguage(id.to_owned()))
    }

    fn syntax(self) -> Syntax {
        match self {
            Self::Curl | Self::Httpie | Self::Wget => Syntax::Shell,
            Self::PythonRequests | Self::PythonHttpx => Syntax::Python,
            Self::JsFetch | Self::NodeAxios => Syntax::Js,
            Self::Go => Syntax::Go,
            Self::RustReqwest => Syntax::Rust,
            Self::Powershell => Syntax::PowerShell,
            Self::Csharp => Syntax::CSharp,
        }
    }

    fn comment(self) -> &'static str {
        match self.syntax() {
            Syntax::Shell | Syntax::Python | Syntax::PowerShell => "#",
            _ => "//",
        }
    }

    fn has_digest_helper(self) -> bool {
        matches!(
            self,
            Self::Curl | Self::Httpie | Self::Wget | Self::PythonRequests | Self::PythonHttpx
        )
    }

    /// Whether the language computes signature auth `kind` itself.
    fn has_helper(self, kind: &str) -> bool {
        match kind {
            "digest" => self.has_digest_helper(),
            "aws_sigv4" => self == Self::Curl,
            _ => false,
        }
    }

    fn template(self) -> &'static str {
        match self {
            Self::Curl => "",
            Self::Httpie => include_str!("templates/httpie.j2"),
            Self::Wget => include_str!("templates/wget.j2"),
            Self::PythonRequests => include_str!("templates/python-requests.j2"),
            Self::PythonHttpx => include_str!("templates/python-httpx.j2"),
            Self::JsFetch => include_str!("templates/js-fetch.j2"),
            Self::NodeAxios => include_str!("templates/node-axios.j2"),
            Self::Go => include_str!("templates/go.j2"),
            Self::RustReqwest => include_str!("templates/rust-reqwest.j2"),
            Self::Powershell => include_str!("templates/powershell.j2"),
            Self::Csharp => include_str!("templates/csharp.j2"),
        }
    }
}

impl fmt::Display for Lang {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

/// Why a snippet cannot be made.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SnippetError {
    #[error("snippet: unknown language '{0}' (supported: {list})", list = language_list())]
    UnknownLanguage(String),
    #[error("snippet: kind '{0}' has no snippet export")]
    UnsupportedKind(String),
    /// A `kind: graphql` or `kind: jsonrpc` file that cannot be read as a
    /// POST.
    #[error("{0}")]
    Invalid(String),
    /// A custom template that cannot be parsed or rendered.
    #[error("snippet: template: {0}")]
    Template(String),
}

/// The HTTP request a snippet of `target` is made from: `None` for a
/// `kind: http` request (use it as it is), its POST for `kind: graphql` and
/// for `kind: jsonrpc` calls over HTTP. Other kinds have no snippet export.
pub fn http_entry(target: &RequestRef<'_>) -> Result<Option<RequestEntry>, SnippetError> {
    let file = target.root.join(&target.entry.path);
    let http = match target.request() {
        Request::Http(_) => return Ok(None),
        Request::Graphql(graphql) => {
            let read = |raw: &str| {
                let path = target.contain_file(raw).map_err(|e| e.to_string())?;
                std::fs::read_to_string(&path).map_err(|e| format!("cannot read {raw}: {e}"))
            };
            graphql_as_http(&file, graphql, read)
        }
        Request::Jsonrpc(jsonrpc) => jsonrpc_as_http(&file, jsonrpc),
        other @ (Request::Mcp(_)
        | Request::Grpc(_)
        | Request::Ws(_)
        | Request::Sse(_)
        | Request::Messaging(_)
        | Request::Evm(_)
        | Request::Opaque(_)) => {
            return Err(SnippetError::UnsupportedKind(other.kind().to_owned()));
        }
    };
    let http = http.map_err(|e| SnippetError::Invalid(e.to_string()))?;
    Ok(Some(RequestEntry {
        path: target.entry.path.clone(),
        request: Request::Http(Box::new(http)),
    }))
}

fn language_list() -> String {
    Lang::ALL.map(Lang::id).join(", ")
}

/// How a snippet is made.
#[derive(Clone, Copy, Debug, Default)]
pub struct SnippetOptions<'a> {
    /// Show secret values instead of `«redacted:<name>»`.
    pub show_secrets: bool,
    /// Placeholder mode: the variables [`resolve_placeholders`] returned.
    pub placeholders: Option<&'a [PlaceholderVar]>,
    /// The workspace root, so files are named relative to it.
    pub root: Option<&'a Path>,
}

/// A snippet and what it could not include.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snippet {
    pub code: String,
    pub warnings: Vec<String>,
}

/// The version of the context a custom template receives, as the global
/// `docs_context_version`. Raised whenever the view a template sees
/// changes, so `[docs].template_dirs` templates written for another
/// version fall back to the built-in template instead of breaking.
pub const TEMPLATE_CONTEXT_VERSION: u32 = 1;

/// `request` as a `lang` snippet.
pub fn snippet(request: &ResolvedRequest, lang: Lang, options: &SnippetOptions<'_>) -> Snippet {
    make(request, lang, options, None).expect("built-in templates render")
}

/// `request` as a `lang` snippet rendered by `template`, a custom template
/// (`[docs].template_dirs`), instead of the built-in one. It sees the same
/// redacted view and filters as the built-in templates.
pub fn custom_snippet(
    request: &ResolvedRequest,
    lang: Lang,
    options: &SnippetOptions<'_>,
    template: &str,
) -> Result<Snippet, SnippetError> {
    make(request, lang, options, Some(template))
}

fn make(
    request: &ResolvedRequest,
    lang: Lang,
    options: &SnippetOptions<'_>,
    custom: Option<&str>,
) -> Result<Snippet, SnippetError> {
    let show_secrets = options.show_secrets && options.placeholders.is_none();
    let mut warnings = Vec::new();
    if let Some(kind) = request.auth.as_ref().and_then(signature_kind)
        && !lang.has_helper(kind)
    {
        warnings.push(format!(
            "snippet: {kind} auth cannot be precomputed for {lang}"
        ));
    }
    let code = match custom {
        None if lang == Lang::Curl => curl(request, options, show_secrets, &mut warnings),
        _ => templated(request, lang, options, custom, &mut warnings)?,
    };
    Ok(Snippet { code, warnings })
}

/// The snippet of a language rendered from a template: the built-in one,
/// or `custom`.
fn templated(
    request: &ResolvedRequest,
    lang: Lang,
    options: &SnippetOptions<'_>,
    custom: Option<&str>,
    warnings: &mut Vec<String>,
) -> Result<String, SnippetError> {
    let context = Context {
        lang,
        syntax: lang.syntax(),
        shown: Shown::new(
            request,
            options.show_secrets && options.placeholders.is_none(),
        ),
        placeholders: options.placeholders,
        root: options.root,
    };
    let mut view = context.view(request);
    warnings.extend(limitations(&view, lang));
    if lang == Lang::Httpie {
        if !httpie::sends_method(&view.method) {
            warnings.push(format!(
                "snippet: httpie cannot send method {}",
                view.method
            ));
            return Ok(httpie_refusal(&view.method, view.auth_comment.as_deref()));
        }
        let (items, left_out) = httpie::items(&view);
        view.items = items;
        warnings.extend(left_out);
    }
    let source = custom.unwrap_or(lang.template());
    let code = render(lang, &view, options.placeholders.is_some(), source)
        .map_err(|e| SnippetError::Template(e.to_string()))?;
    if lang.syntax() == Syntax::Shell && code.contains('\0') {
        warnings.push(NUL_WARNING.replace("{tool}", lang.id()));
        return Ok(nul_refusal());
    }
    Ok(code)
}

/// The httpie snippet of a request whose method httpie cannot send: a
/// comment, since httpie would read the method as the URL and the URL as a
/// request item. A method is an HTTP token, so it fits on a comment line.
fn httpie_refusal(method: &str, auth_comment: Option<&str>) -> String {
    let mut code = String::new();
    if let Some(comment) = auth_comment {
        code.push_str(&format!("# {comment}\n"));
    }
    code.push_str(&format!(
        "# httpie sends only methods made of letters, not {method}: use curl"
    ));
    code
}

/// The signature auth type of `auth`, if it is one.
fn signature_kind(auth: &Auth) -> Option<&'static str> {
    match auth {
        Auth::Digest { .. } => Some("digest"),
        Auth::AwsSigV4(_) => Some("aws_sigv4"),
        Auth::Jwt(_) => Some("jwt"),
        _ => None,
    }
}

fn curl(
    request: &ResolvedRequest,
    options: &SnippetOptions<'_>,
    show_secrets: bool,
    warnings: &mut Vec<String>,
) -> String {
    let exported = curl_command(
        request,
        &CurlOptions {
            show_secrets,
            signature_helpers: true,
            keep_markers: options.placeholders.is_some(),
            root: options.root,
        },
    );
    warnings.extend(exported.warnings);
    match view::auth_comment(request.auth.as_ref(), Lang::Curl) {
        Some(comment) => format!("# {comment}\n{}", exported.command),
        None => exported.command,
    }
}

/// What `lang` cannot express of `view`.
fn limitations(view: &View, lang: Lang) -> Vec<String> {
    let mut warnings = Vec::new();
    if view.body.kind == "multipart" && lang == Lang::Wget {
        warnings.push("snippet: wget cannot send multipart bodies".to_owned());
    }
    let typed_text_part = view
        .body
        .parts
        .iter()
        .any(|p| !p.is_file && p.content_type.is_some());
    if typed_text_part && lang == Lang::Httpie {
        warnings.push("snippet: httpie sends text parts without their content type".to_owned());
    }
    let non_ascii = view.headers.iter().find(|h| code::needs_bytes(&h.value));
    if let Some(header) = non_ascii
        && matches!(lang, Lang::Powershell | Lang::Csharp)
    {
        warnings.push(format!(
            "snippet: {lang} may refuse the non-ASCII value of header {}",
            header.name
        ));
    }
    if !view.verify_tls && matches!(lang, Lang::JsFetch | Lang::NodeAxios) {
        warnings.push(format!(
            "snippet: TLS verification cannot be turned off in {lang}"
        ));
    }
    warnings
}

/// Render `source`, a template for `lang`, over `view`.
fn render(
    lang: Lang,
    view: &View,
    placeholders: bool,
    source: &str,
) -> Result<String, minijinja::Error> {
    let syntax = lang.syntax();
    let mut env = Environment::new();
    env.set_syntax(
        SyntaxConfig::builder()
            .trim_blocks(true)
            .lstrip_blocks(true)
            .build()
            .expect("the default delimiters are valid"),
    );
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    env.add_filter("q", move |text: String| {
        syntax.expression(&text, placeholders, false)
    });
    env.add_filter("qp", move |text: String| {
        syntax.expression(&text, placeholders, true)
    });
    env.add_filter("lit", move |text: String| syntax.literal(&text));
    env.add_filter("hv", move |text: String| {
        syntax.header_value(&text, placeholders)
    });
    env.add_global("comment", lang.comment());
    env.add_global("docs_context_version", TEMPLATE_CONTEXT_VERSION);
    let template = env.template_from_str(source)?;
    let code = template.render(Value::from(Serde(view)))?;
    Ok(code.trim_end().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages_parse_by_id_and_alias() {
        for lang in Lang::ALL {
            assert_eq!(Lang::parse(lang.id()), Ok(lang));
        }
        assert_eq!(Lang::parse("python"), Ok(Lang::PythonRequests));
        assert_eq!(Lang::parse("fetch"), Ok(Lang::JsFetch));
        assert_eq!(Lang::parse("rust"), Ok(Lang::RustReqwest));
        assert_eq!(
            Lang::parse("cobol").unwrap_err().to_string(),
            "snippet: unknown language 'cobol' (supported: curl, httpie, wget, python-requests, \
             python-httpx, js-fetch, node-axios, go, rust-reqwest, powershell, csharp)"
        );
    }

    /// Regression: a shell argument cannot carry a NUL byte (bash and zsh
    /// cut it there), so the command silently sent another body. Shell
    /// snippets refuse such a request with a comment and a warning.
    #[test]
    fn shell_snippets_refuse_a_nul_byte() {
        use rustman_core::vars::{Resolved, ResolvedBody};
        let request = ResolvedRequest {
            method: "POST".into(),
            url: Resolved::literal("http://localhost/x"),
            query: Vec::new(),
            headers: Vec::new(),
            body: ResolvedBody::Text(Resolved::literal("a\0b")),
            auth: None,
            settings: Default::default(),
            tls: Default::default(),
            secrets: Vec::new(),
        };
        for lang in [Lang::Curl, Lang::Httpie, Lang::Wget] {
            let out = snippet(&request, lang, &SnippetOptions::default());
            assert!(!out.code.contains('\0'), "{lang}: {:?}", out.code);
            assert!(out.code.starts_with("# "), "{lang}: {}", out.code);
            assert!(
                out.warnings.iter().any(|w| w.contains("NUL byte")),
                "{lang}: {:?}",
                out.warnings
            );
        }
        let exported = crate::curl::export_curl(&request, false);
        assert!(!exported.command.contains('\0'));
        let python = snippet(&request, Lang::PythonRequests, &SnippetOptions::default());
        assert!(python.code.contains(r"a\u0000b"), "{}", python.code);
    }

    /// A URL that would read as an option (the engine refuses it, but a
    /// snippet is still written) comes after `--` or curl's `--url`.
    #[test]
    fn a_url_never_reads_as_an_option() {
        use rustman_core::vars::{Resolved, ResolvedBody};
        let request = ResolvedRequest {
            method: "GET".into(),
            url: Resolved::literal("-K/etc/passwd"),
            query: Vec::new(),
            headers: Vec::new(),
            body: ResolvedBody::None,
            auth: None,
            settings: Default::default(),
            tls: Default::default(),
            secrets: Vec::new(),
        };
        let code = |lang| snippet(&request, lang, &SnippetOptions::default()).code;
        assert!(
            code(Lang::Curl).contains(" --url -K/etc/passwd"),
            "{}",
            code(Lang::Curl)
        );
        assert!(
            code(Lang::Httpie).contains(" -- GET -K/etc/passwd"),
            "{}",
            code(Lang::Httpie)
        );
        assert!(
            code(Lang::Wget).contains("  -- -K/etc/passwd"),
            "{}",
            code(Lang::Wget)
        );
    }

    #[test]
    fn a_custom_template_sees_the_same_view_and_filters() {
        use rustman_core::vars::{Resolved, ResolvedBody};
        let request = ResolvedRequest {
            method: "POST".into(),
            url: Resolved::literal("http://localhost/x?q=1"),
            query: Vec::new(),
            headers: Vec::new(),
            body: ResolvedBody::Text(Resolved::literal("hi")),
            auth: None,
            settings: Default::default(),
            tls: Default::default(),
            secrets: Vec::new(),
        };
        let template = "{{ comment }} v{{ docs_context_version }}\n{{ method_lower }}({{ url|q }}, {{ body.text|q }})\n";
        let out = custom_snippet(&request, Lang::Go, &SnippetOptions::default(), template).unwrap();
        assert_eq!(out.code, "// v1\npost(\"http://localhost/x?q=1\", \"hi\")");
        let curl = custom_snippet(
            &request,
            Lang::Curl,
            &SnippetOptions::default(),
            "{{ url|q }}",
        )
        .unwrap();
        assert_eq!(curl.code, "'http://localhost/x?q=1'");
        let broken = custom_snippet(&request, Lang::Go, &SnippetOptions::default(), "{{ nope }}");
        assert!(
            matches!(&broken, Err(SnippetError::Template(m)) if m.contains("undefined")),
            "{broken:?}"
        );
    }

    #[test]
    fn every_template_parses() {
        let env = Environment::new();
        for lang in Lang::ALL.into_iter().filter(|l| *l != Lang::Curl) {
            env.template_from_str(lang.template())
                .unwrap_or_else(|e| panic!("{lang}: {e}"));
        }
    }
}
