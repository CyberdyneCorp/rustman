//! The page model (design D1): what renderers turn into files. Every text
//! in it is [`Rendered`] (redacted) or [`Html`] made from redacted text.

use serde::Serialize;

use crate::commonmark::Html;
use crate::rendered::Rendered;

/// A whole documentation site.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DocSite {
    pub title: Rendered,
    /// Sections in workspace order.
    pub sections: Vec<Section>,
    /// Variables listed in `[docs].publish_variables`, with the value of
    /// the environment chosen with `--env` (none by default).
    pub variables: Vec<Row>,
    /// Ids of the requests whose page shows a recorded example.
    pub published_examples: Vec<String>,
    /// GraphQL schema references and JSON-RPC method catalogs, after the
    /// sections; empty when the workspace has neither.
    pub references: Vec<ReferenceSet>,
}

impl DocSite {
    /// Every endpoint, depth first in workspace order.
    pub fn endpoints(&self) -> Vec<&Endpoint> {
        let mut out = Vec::new();
        for section in &self.sections {
            section.collect_endpoints(&mut out);
        }
        out
    }

    /// Every section, depth first in workspace order.
    pub fn all_sections(&self) -> Vec<&Section> {
        let mut out = Vec::new();
        for section in &self.sections {
            section.collect_sections(&mut out);
        }
        out
    }
}

/// A collection (or the workspace's own requests); nested folders are
/// subsections.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Section {
    pub slug: String,
    pub title: Rendered,
    pub description: Option<Html>,
    /// 1 for a top-level collection.
    pub depth: usize,
    pub endpoints: Vec<Endpoint>,
    pub sections: Vec<Section>,
}

impl Section {
    fn collect_sections<'a>(&'a self, out: &mut Vec<&'a Section>) {
        out.push(self);
        for section in &self.sections {
            section.collect_sections(out);
        }
    }

    fn collect_endpoints<'a>(&'a self, out: &mut Vec<&'a Endpoint>) {
        out.extend(&self.endpoints);
        for section in &self.sections {
            section.collect_endpoints(out);
        }
    }
}

/// One request's page.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Endpoint {
    /// The page file stem and anchor id.
    pub slug: String,
    /// The slug of the section that lists it.
    pub section: String,
    /// The request id: its path without `.rman.yaml`.
    pub id: Rendered,
    pub name: Rendered,
    pub kind: Rendered,
    pub method: Option<Rendered>,
    /// The URL template, variables kept as `{{name}}`.
    pub url: Option<Rendered>,
    pub summary: Option<Rendered>,
    pub description: Option<Html>,
    pub deprecated: bool,
    pub query: Vec<Row>,
    pub headers: Vec<Row>,
    pub body_type: Option<Rendered>,
    pub body_fields: Vec<Row>,
    /// A raw body that has no fields (text, JSON text).
    pub body_text: Option<Rendered>,
    /// The auth scheme name, never credentials.
    pub auth: Option<Rendered>,
    /// The assertions, as "Expected: ..." lines.
    pub expected: Vec<Rendered>,
    /// The captures, as "Produces: ..." lines.
    pub produces: Vec<Rendered>,
    /// The request-file fields of kinds without a dedicated page.
    pub fields: Vec<Row>,
    /// OpenAPI request and response schemas, as pretty JSON.
    pub schemas: Vec<Schema>,
    pub example: Option<Example>,
    /// Code samples, in `[docs].samples` order; empty for kinds the
    /// snippet engine cannot express.
    pub samples: Vec<Sample>,
}

/// One code sample of an endpoint.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Sample {
    /// The language id as `[docs].samples` writes it (`curl`, `fetch`).
    pub id: Rendered,
    /// The fenced-code language of Markdown output (`sh`, `js`).
    pub fence: &'static str,
    pub code: Rendered,
}

/// The reference pages of one GraphQL schema or OpenRPC document.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ReferenceSet {
    /// The index anchor.
    pub slug: String,
    /// `graphql` or `jsonrpc`.
    pub kind: &'static str,
    /// `GraphQL schema` or `JSON-RPC methods`.
    pub title: Rendered,
    /// The endpoint, as the request file writes it.
    pub endpoint: Rendered,
    pub pages: Vec<ReferencePage>,
}

/// One GraphQL type or JSON-RPC method.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ReferencePage {
    /// The page file stem.
    pub slug: String,
    /// The slug of its set.
    pub set: String,
    pub name: Rendered,
    /// `object`, `enum`, `method`...
    pub label: Rendered,
    pub description: Option<Html>,
    /// The deprecation reason, when deprecated.
    pub deprecated: Option<Rendered>,
    pub tables: Vec<Table>,
    pub schemas: Vec<Schema>,
}

/// A table with a header row; every row has one cell per column.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Table {
    pub title: Rendered,
    pub columns: Vec<Rendered>,
    pub rows: Vec<Vec<Rendered>>,
}

/// A table row: name, value and an optional description.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Row {
    pub name: Rendered,
    pub value: Rendered,
    pub description: Option<Rendered>,
}

/// A schema from the OpenAPI document.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Schema {
    /// `Request body` or `Response 200`.
    pub label: Rendered,
    pub json: Rendered,
}

/// A recorded response, published because the request opted in.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Example {
    /// `history` or `snapshot`.
    pub source: &'static str,
    pub status: Option<u16>,
    /// The recording date (`2026-11-17`), from the exchange.
    pub recorded: Option<Rendered>,
    pub headers: Vec<Row>,
    pub body: Option<Rendered>,
    /// The body is JSON (pretty-printed).
    pub body_json: bool,
    /// The body was cut at [`crate::examples::MAX_EXAMPLE_BYTES`].
    pub truncated: bool,
}
