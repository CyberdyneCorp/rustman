//! Routes compiled from the document's paths into a segment trie (design
//! D1; spec: Route matching). Literal children are tried before parameter
//! children; every match is collected and the one with the most literal
//! segments wins, then the earlier path in the document. The servers' base
//! paths are stripped first.

use std::collections::BTreeMap;

use percent_encoding::percent_decode_str;
use rustman_explore::ApiModel;

/// One path of the document with its methods.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathRoute {
    /// The path template, e.g. `/users/{id}`.
    pub template: String,
    /// Upper-case methods with the index of their operation in
    /// [`ApiModel::operations`], in document order.
    pub methods: Vec<(String, usize)>,
    literals: usize,
}

impl PathRoute {
    /// The `Allow` header value: `GET, POST`.
    pub fn allow(&self) -> String {
        let methods: Vec<&str> = self.methods.iter().map(|(m, _)| m.as_str()).collect();
        methods.join(", ")
    }
}

/// What a request resolves to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Routed<'r> {
    /// The operation at this index, with the path parameters by name.
    Operation {
        route: &'r PathRoute,
        operation: usize,
        params: Vec<(String, String)>,
    },
    /// The path exists without this method.
    MethodNotAllowed(&'r PathRoute),
    NotFound,
}

/// A template segment that is not a plain literal: `{id}`, or a parameter
/// with literal text around it such as `{name}.json`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ParamSegment {
    prefix: String,
    name: String,
    suffix: String,
}

impl ParamSegment {
    /// The parameter value in `segment`, when it fits; never empty.
    fn capture<'s>(&self, segment: &'s str) -> Option<&'s str> {
        let value = segment
            .strip_prefix(self.prefix.as_str())?
            .strip_suffix(self.suffix.as_str())?;
        (!value.is_empty()).then_some(value)
    }
}

#[derive(Debug, Default)]
struct Node {
    literals: BTreeMap<String, Node>,
    params: Vec<(ParamSegment, Node)>,
    /// The index of the path ending here in [`Router::paths`].
    path: Option<usize>,
}

/// The compiled routes of one document.
#[derive(Debug, Default)]
pub struct Router {
    root: Node,
    paths: Vec<PathRoute>,
    /// Base paths of the servers, longest first, without a trailing `/`.
    bases: Vec<String>,
}

impl Router {
    pub fn new(model: &ApiModel) -> Self {
        let mut router = Self {
            bases: base_paths(model),
            ..Self::default()
        };
        for (index, operation) in model.operations.iter().enumerate() {
            let method = (operation.method.clone(), index);
            match router
                .paths
                .iter_mut()
                .find(|p| p.template == operation.path)
            {
                Some(path) => path.methods.push(method),
                None => router.insert(&operation.path, method),
            }
        }
        router
    }

    pub fn paths(&self) -> &[PathRoute] {
        &self.paths
    }

    fn insert(&mut self, template: &str, method: (String, usize)) {
        let mut node = &mut self.root;
        let mut literals = 0;
        for segment in segments(template) {
            node = match parse_segment(segment) {
                None => {
                    literals += 1;
                    node.literals.entry(segment.to_owned()).or_default()
                }
                Some(param) => child_for(node, param),
            };
        }
        node.path = Some(self.paths.len());
        self.paths.push(PathRoute {
            template: template.to_owned(),
            methods: vec![method],
            literals,
        });
    }

    /// Resolve `method` and the request `path` (without the query).
    pub fn route(&self, method: &str, path: &str) -> Routed<'_> {
        let path = self.strip_base(path);
        let decoded: Vec<String> = segments(path).map(decode).collect();
        let mut found = Vec::new();
        collect(&self.root, &decoded, &mut Vec::new(), &mut found);
        let best = found.into_iter().min_by_key(|(index, _)| {
            let literals = self.paths[*index].literals;
            (std::cmp::Reverse(literals), *index)
        });
        let Some((index, values)) = best else {
            return Routed::NotFound;
        };
        let route = &self.paths[index];
        match route.methods.iter().find(|(m, _)| m == method) {
            Some((_, operation)) => Routed::Operation {
                route,
                operation: *operation,
                params: param_names(&route.template).zip(values).collect(),
            },
            None => Routed::MethodNotAllowed(route),
        }
    }

    /// `path` without the longest server base path it starts with.
    pub fn strip_base<'p>(&self, path: &'p str) -> &'p str {
        for base in &self.bases {
            if path == base {
                return "/";
            }
            if let Some(rest) = path.strip_prefix(base.as_str())
                && rest.starts_with('/')
            {
                return rest;
            }
        }
        path
    }
}

/// The child of `node` for `param`, created when new.
fn child_for(node: &mut Node, param: ParamSegment) -> &mut Node {
    let at = match node.params.iter().position(|(p, _)| *p == param) {
        Some(at) => at,
        None => {
            node.params.push((param, Node::default()));
            node.params.len() - 1
        }
    };
    &mut node.params[at].1
}

/// Every path matching `rest`, with its parameter values in order.
fn collect(
    node: &Node,
    rest: &[String],
    values: &mut Vec<String>,
    found: &mut Vec<(usize, Vec<String>)>,
) {
    let Some((segment, tail)) = rest.split_first() else {
        if let Some(path) = node.path {
            found.push((path, values.clone()));
        }
        return;
    };
    if let Some(child) = node.literals.get(segment) {
        collect(child, tail, values, found);
    }
    for (param, child) in &node.params {
        if let Some(value) = param.capture(segment) {
            values.push(value.to_owned());
            collect(child, tail, values, found);
            values.pop();
        }
    }
}

/// The non-empty segments of a path: `/a//b/` has `a` and `b`.
fn segments(path: &str) -> impl Iterator<Item = &str> {
    path.split('/').filter(|s| !s.is_empty())
}

fn decode(segment: &str) -> String {
    percent_decode_str(segment).decode_utf8_lossy().into_owned()
}

/// `None` for a literal segment.
fn parse_segment(segment: &str) -> Option<ParamSegment> {
    let open = segment.find('{')?;
    let close = open + segment[open..].find('}')?;
    Some(ParamSegment {
        prefix: segment[..open].to_owned(),
        name: segment[open + 1..close].to_owned(),
        suffix: segment[close + 1..].to_owned(),
    })
}

fn param_names(template: &str) -> impl Iterator<Item = String> {
    segments(template).filter_map(parse_segment).map(|p| p.name)
}

/// The path part of each server URL (variables at their defaults), longest
/// first; `/` and empty paths are left out.
fn base_paths(model: &ApiModel) -> Vec<String> {
    let mut bases: Vec<String> = model
        .servers
        .iter()
        .filter_map(|server| {
            let mut url = server.url.clone();
            for variable in &server.variables {
                url = url.replace(&format!("{{{}}}", variable.name), &variable.default);
            }
            let path = url_path(&url).trim_end_matches('/').to_owned();
            (!path.is_empty()).then_some(path)
        })
        .collect();
    bases.sort_by_key(|b| std::cmp::Reverse(b.len()));
    bases.dedup();
    bases
}

/// `/v1` for `https://api.example.com/v1`, `/v1` or `//host/v1`.
fn url_path(url: &str) -> &str {
    let after_scheme = match url.find("://") {
        Some(at) => &url[at + 3..],
        None => match url.strip_prefix("//") {
            Some(rest) => rest,
            None => return url,
        },
    };
    after_scheme.find('/').map_or("", |at| &after_scheme[at..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustman_explore::openapi::model::{Operation, Server, ServerVariable};

    fn operation(method: &str, path: &str) -> Operation {
        Operation {
            pointer: String::new(),
            operation_id: None,
            method: method.into(),
            path: path.into(),
            summary: None,
            description: None,
            tags: Vec::new(),
            parameters: Vec::new(),
            request_body: None,
            responses: Vec::new(),
            security: None,
            deprecated: false,
        }
    }

    fn model(operations: &[(&str, &str)], servers: &[&str]) -> ApiModel {
        ApiModel {
            title: "t".into(),
            version: "1".into(),
            description: None,
            servers: servers
                .iter()
                .map(|url| Server {
                    url: (*url).into(),
                    description: None,
                    variables: vec![ServerVariable {
                        name: "version".into(),
                        default: "v2".into(),
                        values: Vec::new(),
                    }],
                })
                .collect(),
            tags: Vec::new(),
            operations: operations.iter().map(|(m, p)| operation(m, p)).collect(),
            security: Vec::new(),
            security_schemes: Vec::new(),
            components: serde_json::json!({}),
        }
    }

    fn operation_of(router: &Router, method: &str, path: &str) -> Option<usize> {
        match router.route(method, path) {
            Routed::Operation { operation, .. } => Some(operation),
            _ => None,
        }
    }

    #[test]
    fn literal_segments_beat_parameters_whatever_the_order() {
        let router = Router::new(&model(&[("GET", "/users/{id}"), ("GET", "/users/me")], &[]));
        assert_eq!(operation_of(&router, "GET", "/users/me"), Some(1));
        let Routed::Operation { params, .. } = router.route("GET", "/users/42") else {
            panic!("no match");
        };
        assert_eq!(params, [("id".to_owned(), "42".to_owned())]);
    }

    #[test]
    fn more_literals_win_then_document_order() {
        let ops = [
            ("GET", "/a/{x}/c"),
            ("GET", "/{y}/b/c"),
            ("GET", "/a/{z}/{w}"),
        ];
        let router = Router::new(&model(&ops, &[]));
        assert_eq!(operation_of(&router, "GET", "/a/b/c"), Some(0));
        assert_eq!(operation_of(&router, "GET", "/q/b/c"), Some(1));
        assert_eq!(operation_of(&router, "GET", "/a/b/d"), Some(2));
    }

    #[test]
    fn unknown_paths_and_undeclared_methods() {
        let router = Router::new(&model(&[("GET", "/users"), ("POST", "/users")], &[]));
        assert_eq!(router.route("GET", "/nope"), Routed::NotFound);
        let Routed::MethodNotAllowed(route) = router.route("DELETE", "/users") else {
            panic!("expected 405");
        };
        assert_eq!(route.allow(), "GET, POST");
    }

    #[test]
    fn parameters_with_literal_text_and_encoded_values() {
        let router = Router::new(&model(&[("GET", "/files/{name}.json")], &[]));
        let Routed::Operation { params, .. } = router.route("GET", "/files/a%20b.json") else {
            panic!("no match");
        };
        assert_eq!(params[0].1, "a b");
        assert_eq!(router.route("GET", "/files/.json"), Routed::NotFound);
        assert_eq!(router.route("GET", "/files/a.xml"), Routed::NotFound);
    }

    #[test]
    fn server_base_paths_are_stripped() {
        let servers = ["https://api.example.com/{version}", "/api/"];
        let router = Router::new(&model(&[("GET", "/pets"), ("GET", "/")], &servers));
        assert_eq!(operation_of(&router, "GET", "/v2/pets"), Some(0));
        assert_eq!(operation_of(&router, "GET", "/api/pets"), Some(0));
        assert_eq!(operation_of(&router, "GET", "/pets"), Some(0));
        assert_eq!(operation_of(&router, "GET", "/api"), Some(1));
        assert_eq!(url_path("https://h"), "");
    }
}
