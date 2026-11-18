//! The neutral intermediate representation (design D1).
//!
//! Every source parser produces an [`ImportedCollection`]; the shared stages
//! (variable translation, secret extraction, script suggestions, naming and
//! writing) only ever see this representation. It is plain data, serializable
//! with serde, so other producers (a traffic recorder, script migration) can
//! build or inspect it too.

use serde::{Deserialize, Serialize};

/// One imported source: a collection tree, environments and the notes the
/// parser made about what it could not carry over.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ImportedCollection {
    /// The source collection's name (the default destination is its slug).
    pub name: String,
    /// The root folder: collection-level auth, variables and the items.
    pub root: Folder,
    pub environments: Vec<ImportedEnvironment>,
    pub notes: Vec<Note>,
}

/// A folder of requests; the root folder is the collection itself.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Folder {
    pub name: String,
    pub description: Option<String>,
    /// Inherited by requests that set none; `None` inherits from the parent.
    pub auth: Option<ImportedAuth>,
    pub headers: Vec<Pair>,
    pub variables: Vec<Variable>,
    /// Folder-level scripts, preserved as comments in `_collection.yaml`.
    pub scripts: Vec<Script>,
    pub items: Vec<Item>,
    pub location: Location,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Item {
    Folder(Box<Folder>),
    Request(Box<ImportedRequest>),
}

/// One request, with source variable syntax already in `{{name}}` form or
/// still to be translated by [`crate::translate`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ImportedRequest {
    pub name: String,
    pub description: Option<String>,
    pub method: String,
    pub url: String,
    pub query: Vec<Pair>,
    pub headers: Vec<Pair>,
    pub body: Option<ImportedBody>,
    /// `None` inherits the folder's auth; [`ImportedAuth::None`] disables it.
    pub auth: Option<ImportedAuth>,
    /// Request-scoped variables, such as Postman path variables.
    pub variables: Vec<Variable>,
    pub scripts: Vec<Script>,
    /// Declarative checks suggested from scripts or source assertions.
    pub checks: Vec<Check>,
    pub captures: Vec<CaptureSpec>,
    /// The script statements found, and whether each was converted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub statements: Vec<ScriptStatement>,
    pub location: Location,
}

/// One statement of a request's scripts (script migration).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptStatement {
    /// The statement as written, comments removed.
    pub text: String,
    /// Whether it became a declarative assertion or capture (or is a
    /// `pm.response.json()` alias they build on).
    pub mapped: bool,
}

/// A name and value: a header, query parameter or form field.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pair {
    pub name: String,
    pub value: String,
}

impl Pair {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImportedBody {
    /// JSON text, kept byte for byte (it may hold unquoted placeholders).
    Json {
        text: String,
    },
    Text {
        text: String,
    },
    Form {
        fields: Vec<Pair>,
    },
    Multipart {
        parts: Vec<ImportedPart>,
    },
    File {
        path: String,
    },
    /// Written as a `kind: graphql` request file.
    Graphql {
        query: String,
        /// The variables as JSON text, when the source had any.
        variables: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImportedPart {
    pub name: String,
    pub value: PartValue,
    pub content_type: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartValue {
    Text(String),
    File(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImportedAuth {
    /// Auth explicitly disabled.
    None,
    Basic {
        username: String,
        password: String,
    },
    Bearer {
        token: String,
    },
    ApiKey {
        name: String,
        value: String,
        location: ApiKeyIn,
    },
    Oauth2(ImportedOAuth2),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiKeyIn {
    Header,
    Query,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImportedOAuth2 {
    pub grant: Grant,
    pub token_url: String,
    pub auth_url: Option<String>,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub scopes: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Grant {
    ClientCredentials,
    AuthorizationCode,
}

/// A variable of a collection, folder, request or environment.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Variable {
    pub name: String,
    pub value: String,
    /// The source marked it secret (e.g. Postman `type: secret`).
    pub secret: bool,
    /// A path variable (Postman `:id`, Bruno `params:path`): a namespace of
    /// its own, distinct from `{{id}}`, so it is given a name no other
    /// variable uses before planning (see [`crate::translate`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub path: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ImportedEnvironment {
    pub name: String,
    pub variables: Vec<Variable>,
    pub location: Location,
}

/// A source script, kept verbatim and never executed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Script {
    /// When the source ran it, e.g. `prerequest` or `test`.
    pub event: String,
    pub source: String,
    pub location: Location,
}

/// A declarative check the importer can write as an assertion.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Check {
    Status(u16),
    /// A status class such as `2xx`, by its leading digit.
    StatusClass(u8),
    HeaderExists(String),
    HeaderEquals {
        name: String,
        value: String,
    },
    /// The response body contains this text.
    BodyContains(String),
    /// The total response time is below this many milliseconds.
    LatencyBelow(u64),
    /// A JSONPath check of the response body; `value` is JSON text (empty
    /// for [`JsonOp::Exists`]).
    Json {
        path: String,
        op: JsonOp,
        value: String,
    },
}

/// The operator of a [`Check::Json`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JsonOp {
    Equals,
    NotEquals,
    Gt,
    Lt,
    Contains,
    Exists,
    /// The array or string length; `value` is a number.
    Length,
    /// The JSON type; `value` is a JSON string such as `"string"`.
    Type,
}

/// A capture of a response value into `var`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureSpec {
    pub var: String,
    pub from: CaptureFrom,
    /// Keep the value after the run (`persist: true`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub persist: bool,
}

impl CaptureSpec {
    /// A `from: json` capture of `path` into `var`, not persisted.
    pub fn json(var: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            var: var.into(),
            from: CaptureFrom::Json(path.into()),
            persist: false,
        }
    }
}

/// Where a [`CaptureSpec`] reads its value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureFrom {
    /// A JSONPath into the body.
    Json(String),
    /// A response header, by name.
    Header(String),
    /// A response cookie, by name.
    Cookie(String),
}

/// Where something was found in the source: the file, and the item path
/// (`Users / List`) or line number within it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

impl Location {
    pub fn item(file: &str, item: &str) -> Self {
        Self {
            file: file.to_owned(),
            item: (!item.is_empty()).then(|| item.to_owned()),
            line: None,
        }
    }

    /// `file > item` or `file:line`, for the human summary.
    pub fn display(&self) -> String {
        match (&self.item, self.line) {
            (Some(item), _) => format!("{} > {item}", self.file),
            (None, Some(line)) => format!("{}:{line}", self.file),
            (None, None) => self.file.clone(),
        }
    }
}

/// Something the import could not carry over as is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub kind: NoteKind,
    pub message: String,
    pub location: Location,
}

impl Note {
    pub fn new(kind: NoteKind, message: impl Into<String>, location: &Location) -> Self {
        Self {
            kind,
            message: message.into(),
            location: location.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteKind {
    /// Left out on purpose: disabled headers, parameters or variables.
    Dropped,
    /// A source feature Rustman has no equivalent for.
    Unsupported,
    /// A dynamic variable with no Rustman equivalent, kept unchanged.
    Unmapped,
    /// A script preserved as a comment and not executed.
    Script,
    /// Worth a look, such as a value that looks like a secret but was kept.
    Review,
    /// Where the extracted secrets went.
    Info,
}

impl NoteKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dropped => "dropped",
            Self::Unsupported => "unsupported",
            Self::Unmapped => "unmapped",
            Self::Script => "script",
            Self::Review => "review",
            Self::Info => "info",
        }
    }

    /// Whether `--fail-on-unsupported` turns this note into exit code 1.
    pub fn is_loss(self) -> bool {
        matches!(self, Self::Dropped | Self::Unsupported | Self::Unmapped)
    }
}

/// Visits every string of a structure that may hold `{{variables}}` or
/// secrets: names, URLs, values and bodies (not scripts or locations).
pub trait VisitStrings {
    fn visit_strings(&mut self, f: &mut dyn FnMut(&mut String));
}

impl VisitStrings for Pair {
    fn visit_strings(&mut self, f: &mut dyn FnMut(&mut String)) {
        f(&mut self.name);
        f(&mut self.value);
    }
}

impl VisitStrings for Variable {
    fn visit_strings(&mut self, f: &mut dyn FnMut(&mut String)) {
        f(&mut self.value);
    }
}

impl<T: VisitStrings> VisitStrings for Vec<T> {
    fn visit_strings(&mut self, f: &mut dyn FnMut(&mut String)) {
        for item in self {
            item.visit_strings(f);
        }
    }
}

impl<T: VisitStrings> VisitStrings for Option<T> {
    fn visit_strings(&mut self, f: &mut dyn FnMut(&mut String)) {
        if let Some(inner) = self {
            inner.visit_strings(f);
        }
    }
}

impl VisitStrings for ImportedAuth {
    fn visit_strings(&mut self, f: &mut dyn FnMut(&mut String)) {
        match self {
            Self::None => {}
            Self::Basic { username, password } => {
                f(username);
                f(password);
            }
            Self::Bearer { token } => f(token),
            Self::ApiKey { name, value, .. } => {
                f(name);
                f(value);
            }
            Self::Oauth2(o) => {
                f(&mut o.token_url);
                o.auth_url.iter_mut().for_each(&mut *f);
                f(&mut o.client_id);
                o.client_secret.iter_mut().for_each(&mut *f);
            }
        }
    }
}

impl VisitStrings for ImportedPart {
    fn visit_strings(&mut self, f: &mut dyn FnMut(&mut String)) {
        match &mut self.value {
            PartValue::Text(text) | PartValue::File(text) => f(text),
        }
    }
}

impl VisitStrings for ImportedBody {
    fn visit_strings(&mut self, f: &mut dyn FnMut(&mut String)) {
        match self {
            Self::Json { text } | Self::Text { text } | Self::File { path: text } => f(text),
            Self::Form { fields } => fields.visit_strings(f),
            Self::Multipart { parts } => parts.visit_strings(f),
            Self::Graphql { query, variables } => {
                f(query);
                variables.iter_mut().for_each(f);
            }
        }
    }
}

impl VisitStrings for ImportedRequest {
    fn visit_strings(&mut self, f: &mut dyn FnMut(&mut String)) {
        f(&mut self.url);
        self.query.visit_strings(f);
        self.headers.visit_strings(f);
        self.body.visit_strings(f);
        self.auth.visit_strings(f);
        self.variables.visit_strings(f);
    }
}

impl VisitStrings for Folder {
    fn visit_strings(&mut self, f: &mut dyn FnMut(&mut String)) {
        self.auth.visit_strings(f);
        self.headers.visit_strings(f);
        self.variables.visit_strings(f);
        for item in &mut self.items {
            match item {
                Item::Folder(folder) => folder.visit_strings(f),
                Item::Request(request) => request.visit_strings(f),
            }
        }
    }
}

impl Folder {
    /// Every request below this folder, depth first, in source order.
    pub fn requests_mut(&mut self) -> Vec<&mut ImportedRequest> {
        let mut out = Vec::new();
        for item in &mut self.items {
            match item {
                Item::Folder(folder) => out.extend(folder.requests_mut()),
                Item::Request(request) => out.push(request.as_mut()),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ImportedCollection {
        let request = ImportedRequest {
            name: "List".into(),
            method: "GET".into(),
            url: "{{base}}/users".into(),
            headers: vec![Pair::new("Accept", "application/json")],
            body: Some(ImportedBody::Graphql {
                query: "{ me { id } }".into(),
                variables: Some("{\"a\":1}".into()),
            }),
            auth: Some(ImportedAuth::ApiKey {
                name: "X-Key".into(),
                value: "{{key}}".into(),
                location: ApiKeyIn::Header,
            }),
            checks: vec![Check::Status(200), Check::HeaderExists("ETag".into())],
            captures: vec![CaptureSpec::json("id", "$.id")],
            location: Location::item("c.json", "Users / List"),
            ..ImportedRequest::default()
        };
        let users = Folder {
            name: "Users".into(),
            auth: Some(ImportedAuth::Bearer {
                token: "{{t}}".into(),
            }),
            items: vec![Item::Request(Box::new(request))],
            ..Folder::default()
        };
        ImportedCollection {
            name: "Billing".into(),
            root: Folder {
                variables: vec![Variable {
                    name: "base".into(),
                    value: "https://x".into(),
                    secret: false,
                    path: false,
                }],
                items: vec![Item::Folder(Box::new(users))],
                ..Folder::default()
            },
            environments: vec![ImportedEnvironment {
                name: "Staging".into(),
                ..ImportedEnvironment::default()
            }],
            notes: vec![Note::new(
                NoteKind::Dropped,
                "disabled header X-Debug dropped",
                &Location::item("c.json", "Users / List"),
            )],
        }
    }

    #[test]
    fn the_ir_round_trips_through_serde() {
        let collection = sample();
        let json = serde_json::to_string(&collection).unwrap();
        let back: ImportedCollection = serde_json::from_str(&json).unwrap();
        assert_eq!(back, collection);
    }

    #[test]
    fn visiting_reaches_nested_request_strings() {
        let mut collection = sample();
        let mut seen = Vec::new();
        collection.root.visit_strings(&mut |s| seen.push(s.clone()));
        for expected in [
            "{{base}}/users",
            "{{key}}",
            "{{t}}",
            "https://x",
            "{ me { id } }",
        ] {
            assert!(seen.iter().any(|s| s == expected), "{expected}: {seen:?}");
        }
        assert_eq!(collection.root.requests_mut().len(), 1);
    }

    #[test]
    fn locations_display_item_or_line() {
        assert_eq!(
            Location::item("c.json", "A / B").display(),
            "c.json > A / B"
        );
        let line = Location {
            file: "a.bru".into(),
            item: None,
            line: Some(3),
        };
        assert_eq!(line.display(), "a.bru:3");
    }
}
