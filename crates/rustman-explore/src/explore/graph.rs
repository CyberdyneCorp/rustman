//! Dependency discovery (the `api-exploration` spec, "Dependency
//! discovery"): which operation produces the value of each path parameter.
//!
//! Producers come, in order of preference, from `[explore.producers]`
//! overrides, from OpenAPI `links`, and from a heuristic: an operation whose
//! success response has a field named `id` or `<resource>Id` produces the
//! path parameter of that name in the operations under its path.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::operation_key;
use crate::openapi::model::{ApiModel, Operation, ParameterLocation};
use crate::synth::{Synthesizer, is_json_media_type};

/// Where an edge comes from, strongest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EdgeSource {
    Override,
    Link,
    Heuristic,
}

impl EdgeSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Override => "override",
            Self::Link => "link",
            Self::Heuristic => "heuristic",
        }
    }
}

/// `producer` yields the value of `consumer`'s path parameter `parameter`
/// at `pointer` in its response body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub consumer: String,
    pub parameter: String,
    pub producer: String,
    /// A JSON pointer into the producer's response body, e.g. `/id`.
    pub pointer: String,
    pub source: EdgeSource,
}

/// A path parameter nothing produces.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unresolved {
    pub operation: String,
    pub parameter: String,
}

/// The resource graph of one document.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Graph {
    /// Per consumer and parameter, in order of preference.
    pub edges: Vec<Edge>,
    pub unresolved: Vec<Unresolved>,
    /// Overrides that name no operation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

impl Graph {
    /// Discover the graph of `model`, with `overrides` from
    /// `[explore.producers]` (parameter name to `operation[#pointer]`).
    pub fn build(model: &ApiModel, overrides: &BTreeMap<String, String>) -> Self {
        let finder = Finder {
            model,
            synth: Synthesizer::new(&model.components),
        };
        let mut graph = Self::default();
        let overrides = finder.overrides(overrides, &mut graph.warnings);
        for consumer in &model.operations {
            for param in consumer.parameters_in(ParameterLocation::Path) {
                let edges = finder.edges(consumer, &param.name, &overrides);
                if edges.is_empty() {
                    graph.unresolved.push(Unresolved {
                        operation: operation_key(consumer),
                        parameter: param.name.clone(),
                    });
                }
                graph.edges.extend(edges);
            }
        }
        graph
    }

    /// The producers of `parameter` for `consumer`, best first.
    pub fn producers<'a>(
        &'a self,
        consumer: &'a str,
        parameter: &'a str,
    ) -> impl Iterator<Item = &'a Edge> {
        self.edges
            .iter()
            .filter(move |e| e.consumer == consumer && e.parameter == parameter)
    }

    /// One line per edge and per unresolved parameter, as `explore plan
    /// --graph` prints it.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for e in &self.edges {
            let _ = writeln!(
                out,
                "{}: {} <- {} {} ({})",
                e.consumer,
                e.parameter,
                e.producer,
                e.pointer,
                e.source.as_str()
            );
        }
        for u in &self.unresolved {
            let _ = writeln!(out, "{}: {} <- no producer", u.operation, u.parameter);
        }
        for w in &self.warnings {
            let _ = writeln!(out, "warning: {w}");
        }
        out
    }
}

/// An override, resolved to its producing operation.
struct Override<'a> {
    producer: &'a Operation,
    pointer: Option<String>,
}

struct Finder<'a> {
    model: &'a ApiModel,
    synth: Synthesizer<'a>,
}

impl<'a> Finder<'a> {
    fn overrides(
        &self,
        raw: &BTreeMap<String, String>,
        warnings: &mut Vec<String>,
    ) -> BTreeMap<String, Override<'a>> {
        let mut out = BTreeMap::new();
        for (param, value) in raw {
            let (reference, pointer) = match value.split_once('#') {
                Some((r, p)) => (r.trim(), Some(p.to_owned())),
                None => (value.trim(), None),
            };
            match find_operation(self.model, reference) {
                Some(producer) => {
                    out.insert(param.clone(), Override { producer, pointer });
                }
                None => warnings.push(format!(
                    "[explore.producers] {param}: no operation '{reference}'"
                )),
            }
        }
        out
    }

    fn edges(
        &self,
        consumer: &Operation,
        param: &str,
        overrides: &BTreeMap<String, Override<'_>>,
    ) -> Vec<Edge> {
        let key = operation_key(consumer);
        let edge = |producer: &Operation, pointer: String, source| Edge {
            consumer: key.clone(),
            parameter: param.to_owned(),
            producer: operation_key(producer),
            pointer,
            source,
        };
        let mut edges = Vec::new();
        if let Some(o) = overrides.get(param).filter(|o| o.producer != consumer) {
            let pointer = o.pointer.clone().or_else(|| self.field(o.producer, param));
            edges.push(edge(
                o.producer,
                pointer.unwrap_or_else(|| "/id".into()),
                EdgeSource::Override,
            ));
        }
        for (producer, pointer) in self.links_to(consumer, param) {
            edges.push(edge(producer, pointer, EdgeSource::Link));
        }
        for (producer, pointer) in self.heuristic(consumer, param) {
            edges.push(edge(producer, pointer, EdgeSource::Heuristic));
        }
        edges
    }

    /// Producers whose success responses link to `consumer` with a
    /// `$response.body#<pointer>` value for `param`.
    fn links_to(&self, consumer: &Operation, param: &str) -> Vec<(&'a Operation, String)> {
        let mut out = Vec::new();
        for producer in &self.model.operations {
            let links = success_responses(producer).flat_map(|r| &r.links);
            for link in links.filter(|l| targets(l, consumer)) {
                let value = link
                    .parameters
                    .iter()
                    .find(|(name, _)| name == param || name.strip_prefix("path.") == Some(param));
                let pointer = value
                    .and_then(|(_, v)| v.as_str())
                    .and_then(|v| v.strip_prefix("$response.body#"));
                if let Some(pointer) = pointer {
                    out.push((producer, pointer.to_owned()));
                }
            }
        }
        out
    }

    /// Operations under a prefix of `consumer`'s path whose response has a
    /// matching field: POST first, then the longest prefix.
    fn heuristic(&self, consumer: &Operation, param: &str) -> Vec<(&'a Operation, String)> {
        let mut found: Vec<(&Operation, String)> = self
            .model
            .operations
            .iter()
            .filter(|p| *p != consumer && is_prefix(&p.path, &consumer.path))
            .filter(|p| {
                p.parameters_in(ParameterLocation::Path)
                    .all(|q| q.name != param)
            })
            .filter_map(|p| self.field(p, param).map(|pointer| (p, pointer)))
            .collect();
        found.sort_by_key(|(p, _)| (p.method != "POST", usize::MAX - p.path.len()));
        found
    }

    /// The pointer of the field of `producer`'s success response that
    /// carries `param`: the field itself, or `id` when `param` is `id` or
    /// `<resource>Id` for the producer's resource. A list response yields
    /// its first item's field.
    fn field(&self, producer: &Operation, param: &str) -> Option<String> {
        let schema = success_schema(producer)?;
        let resource = resource_ids(&producer.path);
        let names = [param.to_owned(), "id".to_owned()];
        let accepts = |name: &str| {
            name == param
                || (name == "id" && (param == "id" || resource.iter().any(|r| r == param)))
        };
        let pick = |object: &Map<String, Value>| {
            let props = object.get("properties")?.as_object()?;
            names
                .iter()
                .find(|n| accepts(n) && props.contains_key(n.as_str()))
                .cloned()
        };
        let object = self.synth.object_view(schema)?;
        if let Some(name) = pick(&object) {
            return Some(format!("/{name}"));
        }
        let items = object.get("items")?;
        let item = self.synth.object_view(items)?;
        pick(&item).map(|name| format!("/0/{name}"))
    }
}

/// `operationId`, or `METHOD /path`.
pub fn find_operation<'a>(model: &'a ApiModel, reference: &str) -> Option<&'a Operation> {
    model
        .operations
        .iter()
        .find(|op| op.operation_id.as_deref() == Some(reference) || operation_key(op) == reference)
}

fn targets(link: &crate::openapi::model::Link, consumer: &Operation) -> bool {
    match (&link.operation_id, &link.operation_ref) {
        (Some(id), _) => consumer.operation_id.as_deref() == Some(id.as_str()),
        (None, Some(reference)) => reference.strip_prefix('#') == Some(consumer.pointer.as_str()),
        (None, None) => false,
    }
}

fn success_responses(op: &Operation) -> impl Iterator<Item = &crate::openapi::model::Response> {
    op.responses.iter().filter(|r| r.status.starts_with('2'))
}

/// The JSON schema of `op`'s first success response.
fn success_schema(op: &Operation) -> Option<&Value> {
    success_responses(op)
        .flat_map(|r| &r.content)
        .find(|m| is_json_media_type(&m.media_type))
        .and_then(|m| m.schema.as_ref())
}

/// Whether `prefix` is a leading run of whole segments of `path`, shorter
/// than it.
fn is_prefix(prefix: &str, path: &str) -> bool {
    let prefix = prefix.trim_end_matches('/');
    path.strip_prefix(prefix)
        .is_some_and(|rest| rest.starts_with('/') && rest.len() > 1)
}

/// `orderId` and `order_id` for a path ending in `/orders`.
fn resource_ids(path: &str) -> Vec<String> {
    let Some(last) = path
        .rsplit('/')
        .find(|s| !s.is_empty() && !s.starts_with('{'))
    else {
        return Vec::new();
    };
    let singular = if let Some(stem) = last.strip_suffix("ies") {
        format!("{stem}y")
    } else {
        last.strip_suffix('s').unwrap_or(last).to_owned()
    };
    vec![format!("{singular}Id"), format!("{singular}_id")]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_are_whole_segments() {
        assert!(is_prefix("/users", "/users/{id}"));
        assert!(!is_prefix("/user", "/users/{id}"));
        assert!(!is_prefix("/users", "/users"));
        assert!(is_prefix("/", "/users"));
    }

    #[test]
    fn resource_ids_singularize() {
        assert_eq!(resource_ids("/orders"), ["orderId", "order_id"]);
        assert_eq!(
            resource_ids("/v1/categories"),
            ["categoryId", "category_id"]
        );
        assert_eq!(resource_ids("/users/{id}/posts"), ["postId", "post_id"]);
        assert!(resource_ids("/").is_empty());
    }
}
