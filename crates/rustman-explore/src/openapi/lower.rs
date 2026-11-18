//! Lowering a bundled, 3.1-shaped document into the [`ApiModel`] IR.
//!
//! OpenAPI objects are read with the `oas3` types, one node at a time so a
//! failure is reported at that node's pointer. Schemas, examples and OAuth2
//! flows are kept as raw JSON (design D1).

use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use super::diagnostic::Diagnostic;
use super::model::{
    ApiModel, Link, MediaType, Operation, Parameter, ParameterLocation, RequestBody, Response,
    SecurityRequirement, SecurityScheme, Server, ServerVariable, Tag,
};
use super::pointer;

/// The methods an OpenAPI path item can hold, in the order they are lowered.
pub const METHODS: [&str; 8] = [
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

/// The most `$ref` hops followed from one place.
const MAX_HOPS: usize = 32;

/// Lower `doc`; on failure, the structural errors found.
pub fn lower(doc: &Value) -> Result<ApiModel, Vec<Diagnostic>> {
    let mut lowering = Lowering {
        doc,
        errors: Vec::new(),
    };
    let model = lowering.model();
    if lowering.errors.is_empty() {
        Ok(model)
    } else {
        Err(lowering.errors)
    }
}

/// Follow the `$ref` chain starting at `at`: the pointer and value reached.
pub fn deref<'a>(doc: &'a Value, at: &str) -> Result<(String, &'a Value), String> {
    let mut current = at.to_owned();
    for _ in 0..MAX_HOPS {
        let value = doc
            .pointer(&current)
            .ok_or_else(|| format!("nothing at {}", pointer::display(&current)))?;
        let Some(reference) = value.get("$ref").and_then(Value::as_str) else {
            return Ok((current, value));
        };
        current = reference
            .strip_prefix('#')
            .and_then(pointer::from_fragment)
            .filter(|p| doc.pointer(p).is_some())
            .ok_or_else(|| format!("unresolved reference '{reference}'"))?;
    }
    Err("the reference chain is circular or too long".into())
}

struct Lowering<'a> {
    doc: &'a Value,
    errors: Vec<Diagnostic>,
}

impl Lowering<'_> {
    fn model(&mut self) -> ApiModel {
        let info: Option<oas3::spec::Info> =
            self.typed("/info", &["contact", "license", "termsOfService"], "info");
        let (title, version, description) = info
            .map(|i| (i.title, i.version, i.description))
            .unwrap_or_default();
        ApiModel {
            title,
            version,
            description,
            servers: self.servers(),
            tags: self.tags(),
            operations: self.operations(),
            security: self.requirements("/security").unwrap_or_default(),
            security_schemes: self.security_schemes(),
            components: self
                .doc
                .get("components")
                .cloned()
                .unwrap_or(Value::Object(Map::new())),
        }
    }

    /// The node at `at`, without the keys in `skip`, read as `T`.
    fn typed<T: DeserializeOwned>(&mut self, at: &str, skip: &[&str], what: &str) -> Option<T> {
        let mut value = self.doc.pointer(at)?.clone();
        if let Some(map) = value.as_object_mut() {
            for key in skip {
                map.remove(*key);
            }
        }
        match serde_json::from_value(value) {
            Ok(typed) => Some(typed),
            Err(e) => {
                self.errors
                    .push(Diagnostic::error(at, format!("invalid {what}: {e}")));
                None
            }
        }
    }

    /// The pointer `at` resolves to, recording an error when it does not.
    fn resolve(&mut self, at: &str) -> Option<String> {
        match deref(self.doc, at) {
            Ok((target, _)) => Some(target),
            Err(message) => {
                self.errors.push(Diagnostic::error(at, message));
                None
            }
        }
    }

    fn get(&self, at: &str) -> Option<&Value> {
        self.doc.pointer(at)
    }

    fn keys(&self, at: &str) -> Vec<String> {
        self.get(at)
            .and_then(Value::as_object)
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    }

    fn len(&self, at: &str) -> usize {
        self.get(at).and_then(Value::as_array).map_or(0, Vec::len)
    }

    fn servers(&mut self) -> Vec<Server> {
        (0..self.len("/servers"))
            .filter_map(|i| {
                let at = format!("/servers/{i}");
                let server: oas3::spec::Server = self.typed(&at, &[], "server")?;
                Some(Server {
                    url: server.url,
                    description: server.description,
                    variables: server
                        .variables
                        .into_iter()
                        .map(|(name, v)| ServerVariable {
                            name,
                            default: v.default,
                            values: v.substitutions_enum,
                        })
                        .collect(),
                })
            })
            .collect()
    }

    fn tags(&mut self) -> Vec<Tag> {
        (0..self.len("/tags"))
            .filter_map(|i| {
                let tag: oas3::spec::Tag =
                    self.typed(&format!("/tags/{i}"), &["externalDocs"], "tag")?;
                Some(Tag {
                    name: tag.name,
                    description: tag.description,
                })
            })
            .collect()
    }

    fn requirements(&mut self, at: &str) -> Option<Vec<SecurityRequirement>> {
        let list: Vec<oas3::spec::SecurityRequirement> =
            self.typed(at, &[], "security requirement")?;
        Some(
            list.into_iter()
                .map(|r| r.0.into_iter().collect())
                .collect(),
        )
    }

    fn security_schemes(&mut self) -> Vec<(String, SecurityScheme)> {
        let base = "/components/securitySchemes";
        self.keys(base)
            .into_iter()
            .filter_map(|name| {
                let at = self.resolve(&pointer::push(base, &name))?;
                Some((name, self.security_scheme(&at)?))
            })
            .collect()
    }

    fn security_scheme(&mut self, at: &str) -> Option<SecurityScheme> {
        if self.get(at)?.get("type").and_then(Value::as_str) == Some("oauth2") {
            let flows = self.get(at)?.get("flows").cloned().unwrap_or(Value::Null);
            return Some(SecurityScheme::OAuth2 { flows });
        }
        let typed: oas3::spec::SecurityScheme = self.typed(at, &[], "security scheme")?;
        self.scheme_from(at, typed)
    }

    fn scheme_from(
        &mut self,
        at: &str,
        typed: oas3::spec::SecurityScheme,
    ) -> Option<SecurityScheme> {
        use oas3::spec::SecurityScheme as S;
        Some(match typed {
            S::Http {
                scheme,
                bearer_format,
                ..
            } => match scheme.to_ascii_lowercase().as_str() {
                "basic" => SecurityScheme::HttpBasic,
                "bearer" => SecurityScheme::HttpBearer {
                    format: bearer_format,
                },
                _ => SecurityScheme::Http { scheme },
            },
            S::ApiKey { name, location, .. } => {
                let location = match location.as_str() {
                    "header" => ParameterLocation::Header,
                    "query" => ParameterLocation::Query,
                    "cookie" => ParameterLocation::Cookie,
                    other => {
                        let message = format!("invalid apiKey location '{other}'");
                        self.errors
                            .push(Diagnostic::error(pointer::push(at, "in"), message));
                        return None;
                    }
                };
                SecurityScheme::ApiKey { name, location }
            }
            S::OAuth2 { .. } => SecurityScheme::OAuth2 { flows: Value::Null },
            S::OpenIdConnect {
                open_id_connect_url,
                ..
            } => SecurityScheme::OpenIdConnect {
                url: open_id_connect_url,
            },
            S::MutualTls { .. } => SecurityScheme::MutualTls,
        })
    }

    fn operations(&mut self) -> Vec<Operation> {
        let mut operations = Vec::new();
        for path in self.keys("/paths") {
            let Some(item) = self.resolve(&pointer::push("/paths", &path)) else {
                continue;
            };
            let shared = self.parameter_list(&pointer::push(&item, "parameters"));
            for method in METHODS {
                let at = pointer::push(&item, method);
                if self.get(&at).is_some()
                    && let Some(op) = self.operation(&at, &path, method, &shared)
                {
                    operations.push(op);
                }
            }
        }
        operations
    }

    fn operation(
        &mut self,
        at: &str,
        path: &str,
        method: &str,
        shared: &[Parameter],
    ) -> Option<Operation> {
        let skip = [
            "parameters",
            "requestBody",
            "responses",
            "callbacks",
            "security",
            "servers",
            "externalDocs",
        ];
        let typed: oas3::spec::Operation = self.typed(at, &skip, "operation")?;
        let own = self.parameter_list(&pointer::push(at, "parameters"));
        let security_at = pointer::push(at, "security");
        let security = match self.get(&security_at) {
            Some(_) => Some(self.requirements(&security_at)?),
            None => None,
        };
        Some(Operation {
            pointer: at.to_owned(),
            operation_id: typed.operation_id,
            method: method.to_ascii_uppercase(),
            path: path.to_owned(),
            summary: typed.summary,
            description: typed.description,
            tags: typed.tags,
            parameters: merge(shared, own),
            request_body: self.request_body(&pointer::push(at, "requestBody")),
            responses: self.responses(&pointer::push(at, "responses")),
            security,
            deprecated: typed.deprecated.unwrap_or(false),
        })
    }

    fn parameter_list(&mut self, at: &str) -> Vec<Parameter> {
        (0..self.len(at))
            .filter_map(|i| self.parameter(&format!("{at}/{i}")))
            .collect()
    }

    fn parameter(&mut self, at: &str) -> Option<Parameter> {
        let target = self.resolve(at)?;
        let skip = ["schema", "content", "examples", "example"];
        let typed: oas3::spec::Parameter = self.typed(&target, &skip, "parameter")?;
        let content = self.content(&pointer::push(&target, "content"));
        let first = content.into_iter().next();
        let raw = self.get(&target)?;
        let schema = raw
            .get("schema")
            .cloned()
            .or_else(|| first.as_ref().and_then(|m| m.schema.clone()));
        let example = raw
            .get("example")
            .cloned()
            .or_else(|| self.first_example(&pointer::push(&target, "examples")))
            .or_else(|| first.and_then(|m| m.example));
        Some(Parameter {
            name: typed.name,
            location: location(&typed.location),
            required: typed.required.unwrap_or(false),
            schema,
            example,
            deprecated: typed.deprecated.unwrap_or(false),
        })
    }

    fn request_body(&mut self, at: &str) -> Option<RequestBody> {
        self.get(at)?;
        let target = self.resolve(at)?;
        let required = self
            .get(&target)
            .and_then(|b| b.get("required"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Some(RequestBody {
            required,
            content: self.content(&pointer::push(&target, "content")),
        })
    }

    fn responses(&mut self, at: &str) -> Vec<Response> {
        self.keys(at)
            .into_iter()
            .filter(|status| !status.starts_with("x-"))
            .filter_map(|status| {
                let target = self.resolve(&pointer::push(at, &status))?;
                let raw = self.get(&target)?;
                let description = raw
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let headers = self.keys(&pointer::push(&target, "headers"));
                Some(Response {
                    status,
                    description,
                    content: self.content(&pointer::push(&target, "content")),
                    headers,
                    links: self.links(&pointer::push(&target, "links")),
                })
            })
            .collect()
    }

    fn links(&mut self, at: &str) -> Vec<Link> {
        self.keys(at)
            .into_iter()
            .filter_map(|name| {
                let target = self.resolve(&pointer::push(at, &name))?;
                let raw = self.get(&target)?;
                let text = |key: &str| raw.get(key).and_then(Value::as_str).map(str::to_owned);
                let parameters = raw
                    .get("parameters")
                    .and_then(Value::as_object)
                    .map(|p| p.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                    .unwrap_or_default();
                Some(Link {
                    operation_id: text("operationId"),
                    operation_ref: text("operationRef"),
                    parameters,
                    name,
                })
            })
            .collect()
    }

    fn content(&mut self, at: &str) -> Vec<MediaType> {
        self.keys(at)
            .into_iter()
            .map(|media_type| {
                let media_at = pointer::push(at, &media_type);
                let raw = self.get(&media_at);
                let schema = raw.and_then(|m| m.get("schema")).cloned();
                let example = raw.and_then(|m| m.get("example")).cloned();
                MediaType {
                    media_type,
                    schema,
                    example,
                    examples: self.examples(&pointer::push(&media_at, "examples")),
                }
            })
            .collect()
    }

    fn examples(&mut self, at: &str) -> Vec<(String, Value)> {
        self.keys(at)
            .into_iter()
            .filter_map(|name| {
                let target = self.resolve(&pointer::push(at, &name))?;
                let value = self.get(&target)?.get("value")?.clone();
                Some((name, value))
            })
            .collect()
    }

    fn first_example(&mut self, at: &str) -> Option<Value> {
        self.examples(at).into_iter().next().map(|(_, v)| v)
    }
}

fn location(location: &oas3::spec::ParameterIn) -> ParameterLocation {
    use oas3::spec::ParameterIn as In;
    match location {
        In::Path => ParameterLocation::Path,
        In::Query => ParameterLocation::Query,
        In::Header => ParameterLocation::Header,
        In::Cookie => ParameterLocation::Cookie,
    }
}

/// Path-level parameters overridden by operation-level ones with the same
/// name and location.
fn merge(shared: &[Parameter], own: Vec<Parameter>) -> Vec<Parameter> {
    let mut merged: Vec<Parameter> = shared
        .iter()
        .filter(|s| {
            !own.iter()
                .any(|o| o.name == s.name && o.location == s.location)
        })
        .cloned()
        .collect();
    merged.extend(own);
    merged
}
