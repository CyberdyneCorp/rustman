//! GraphQL schema and JSON-RPC method references (api-docs-generation
//! "GraphQL and JSON-RPC reference pages").
//!
//! A `graphql` request contributes the schema `rustman-graphql` would
//! validate it against (its `schema_file`, the `[[graphql.endpoints]]`
//! file, or the cached introspection result); a `jsonrpc` request the
//! OpenRPC document of `[jsonrpc.catalogs]` or the discovery cache. To find
//! them, the endpoint is resolved without the secret store and without the
//! process environment, with no environment and then with each one in
//! turn; the resolved URL is only a lookup key and never shown. Nothing is
//! generated when no schema or document is found.

use apollo_compiler::Node;
use apollo_compiler::ast::InputValueDefinition;
use apollo_compiler::collections::IndexMap;
use apollo_compiler::schema::{
    Component, EnumType, ExtendedType, FieldDefinition, InputObjectType, Name, UnionType,
};
use rustman_core::model::Request;
use rustman_core::vars::{NoSecrets, Resolver};
use rustman_core::{RequestRef, Workspace};
use rustman_graphql::{GraphqlSchema, SchemaCache, active_schema, deprecation_reason};
use rustman_jsonrpc::{Catalog, MethodSpec, catalog::catalog_for};

use crate::commonmark::to_html;
use crate::model::{ReferencePage, ReferenceSet, Schema, Table};
use crate::rendered::{DocsRedactor, Rendered};
use crate::slug::Slugs;

/// Where reference pages are built from.
pub struct ReferenceInput<'a, 'w> {
    pub workspace: &'w Workspace,
    /// The visible requests, in workspace order.
    pub requests: &'a [RequestRef<'w>],
    pub redactor: &'a DocsRedactor,
}

/// The GraphQL then JSON-RPC reference sets. `sets` hands out the index
/// anchors (shared with sections), `pages` the page file stems.
pub fn references(
    input: &ReferenceInput<'_, '_>,
    sets: &mut Slugs,
    pages: &mut Slugs,
) -> Vec<ReferenceSet> {
    let mut out = Vec::new();
    let mut builder = SetBuilder {
        sets,
        pages,
        r: input.redactor,
    };
    for (endpoint, schema) in graphql_schemas(input) {
        out.push(builder.graphql(&endpoint, &schema));
    }
    for (endpoint, catalog) in jsonrpc_catalogs(input) {
        out.push(builder.jsonrpc(&endpoint, &catalog));
    }
    out
}

/// The endpoint of `request` resolved with no environment, then with each
/// environment; values that need a secret or the process environment are
/// skipped. `written` itself when nothing resolves.
fn candidates(workspace: &Workspace, request: &RequestRef<'_>, written: &str) -> Vec<String> {
    let mut envs: Vec<_> = workspace.environments.iter().collect();
    envs.sort_by(|a, b| a.name.cmp(&b.name));
    let mut out: Vec<String> = Vec::new();
    for env in std::iter::once(None).chain(envs.into_iter().map(Some)) {
        let resolver = Resolver::for_request(&workspace.settings, request, env, &NoSecrets)
            .with_os_env(|_| None);
        if let Ok(resolved) = resolver.resolve(written)
            && !out.contains(&resolved.text)
        {
            out.push(resolved.text);
        }
    }
    if out.is_empty() {
        out.push(written.to_owned());
    }
    out
}

/// Each distinct schema of the visible `graphql` requests, with the
/// endpoint of the first request using it, as written.
fn graphql_schemas(input: &ReferenceInput<'_, '_>) -> Vec<(String, GraphqlSchema)> {
    let ws = input.workspace;
    let cache = SchemaCache::for_workspace(&ws.root);
    let mut out: Vec<(String, GraphqlSchema)> = Vec::new();
    for request in input.requests {
        let Request::Graphql(graphql) = request.request() else {
            continue;
        };
        let found = candidates(ws, request, &graphql.endpoint)
            .iter()
            .find_map(|c| {
                active_schema(request, &ws.settings.graphql, c, &cache)
                    .ok()
                    .flatten()
            });
        let Some(active) = found else {
            continue;
        };
        let hash = active.schema.sdl_sha256();
        if !out.iter().any(|(_, s)| s.sdl_sha256() == hash) {
            out.push((graphql.endpoint.clone(), active.schema));
        }
    }
    out
}

/// Each distinct OpenRPC catalog of the visible `jsonrpc` requests, with
/// the endpoint of the first request using it, as written.
fn jsonrpc_catalogs(input: &ReferenceInput<'_, '_>) -> Vec<(String, Catalog)> {
    let ws = input.workspace;
    let mut out: Vec<(String, Catalog)> = Vec::new();
    for request in input.requests {
        let Request::Jsonrpc(jsonrpc) = request.request() else {
            continue;
        };
        let written = &jsonrpc.endpoint;
        let found = candidates(ws, request, written).iter().find_map(|c| {
            catalog_for(&ws.root, &ws.settings.jsonrpc, written, c)
                .ok()
                .flatten()
        });
        if let Some(catalog) = found
            && !out.iter().any(|(_, c)| *c == catalog)
        {
            out.push((written.clone(), catalog));
        }
    }
    out
}

struct SetBuilder<'s> {
    sets: &'s mut Slugs,
    pages: &'s mut Slugs,
    r: &'s DocsRedactor,
}

impl SetBuilder<'_> {
    fn set(&mut self, kind: &'static str, title: &str, endpoint: &str) -> ReferenceSet {
        ReferenceSet {
            slug: self.sets.next(title),
            kind,
            title: self.r.render(title),
            endpoint: self.r.render(endpoint),
            pages: Vec::new(),
        }
    }

    fn page(&mut self, set: &ReferenceSet, name: &str, label: &str) -> ReferencePage {
        ReferencePage {
            slug: self.pages.next(&format!("{}-{name}", set.slug)),
            set: set.slug.clone(),
            name: self.r.render(name),
            label: self.r.render(label),
            description: None,
            deprecated: None,
            tables: Vec::new(),
            schemas: Vec::new(),
        }
    }

    fn graphql(&mut self, endpoint: &str, schema: &GraphqlSchema) -> ReferenceSet {
        let mut set = self.set("graphql", "GraphQL schema", endpoint);
        let mut types: Vec<(&Name, &ExtendedType)> = schema
            .schema()
            .types
            .iter()
            .filter(|(name, ty)| !ty.is_built_in() && !name.starts_with("__"))
            .collect();
        types.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        for (name, ty) in types {
            let mut page = self.page(&set, name, type_label(ty));
            page.description = ty.description().map(|d| to_html(&self.r.render(d)));
            page.tables = type_tables(ty, self.r);
            set.pages.push(page);
        }
        set
    }

    fn jsonrpc(&mut self, endpoint: &str, catalog: &Catalog) -> ReferenceSet {
        let mut set = self.set("jsonrpc", "JSON-RPC methods", endpoint);
        let mut methods: Vec<&MethodSpec> = catalog.methods.iter().collect();
        methods.sort_by(|a, b| a.name.cmp(&b.name));
        for method in methods {
            let page = self.method_page(&set, method);
            set.pages.push(page);
        }
        set
    }

    fn method_page(&mut self, set: &ReferenceSet, method: &MethodSpec) -> ReferencePage {
        let r = self.r;
        let mut page = self.page(set, &method.name, "method");
        let text: Vec<&str> = [&method.summary, &method.description]
            .into_iter()
            .flatten()
            .map(String::as_str)
            .collect();
        page.description = (!text.is_empty()).then(|| to_html(&r.render(&text.join("\n\n"))));
        page.deprecated = method.deprecated.then(|| r.render("deprecated"));
        let params = method.params.iter().map(|p| {
            let schema = serde_json::to_string(&p.schema).unwrap_or_default();
            vec![
                r.render(&p.name),
                r.render(if p.required { "yes" } else { "no" }),
                r.render(&schema),
            ]
        });
        push_table(
            &mut page.tables,
            r,
            "Params",
            &["Name", "Required", "Schema"],
            params.collect(),
        );
        let errors = method
            .errors
            .iter()
            .map(|e| vec![r.render(&e.code.to_string()), r.render(&e.message)]);
        push_table(
            &mut page.tables,
            r,
            "Errors",
            &["Code", "Message"],
            errors.collect(),
        );
        if let Some(result) = &method.result {
            page.schemas.push(Schema {
                label: r.render("Result"),
                json: r.json(result),
            });
        }
        page
    }
}

fn type_label(ty: &ExtendedType) -> &'static str {
    match ty {
        ExtendedType::Scalar(_) => "scalar",
        ExtendedType::Object(_) => "object",
        ExtendedType::Interface(_) => "interface",
        ExtendedType::Union(_) => "union",
        ExtendedType::Enum(_) => "enum",
        ExtendedType::InputObject(_) => "input",
    }
}

fn type_tables(ty: &ExtendedType, r: &DocsRedactor) -> Vec<Table> {
    let mut tables = Vec::new();
    match ty {
        ExtendedType::Scalar(_) => {}
        ExtendedType::Object(object) => fields_table(&mut tables, &object.fields, r),
        ExtendedType::Interface(interface) => fields_table(&mut tables, &interface.fields, r),
        ExtendedType::Union(union) => members_table(&mut tables, union, r),
        ExtendedType::Enum(e) => values_table(&mut tables, e, r),
        ExtendedType::InputObject(input) => inputs_table(&mut tables, input, r),
    }
    tables
}

fn fields_table(
    tables: &mut Vec<Table>,
    fields: &IndexMap<Name, Component<FieldDefinition>>,
    r: &DocsRedactor,
) {
    let rows = fields
        .values()
        .map(|f| {
            let args: Vec<String> = f.arguments.iter().map(argument).collect();
            vec![
                r.render(&f.name),
                r.render(&f.ty.to_string()),
                r.render(&args.join(", ")),
                r.render(&deprecation_reason(&f.directives).unwrap_or_default()),
                r.render(f.description.as_deref().unwrap_or_default()),
            ]
        })
        .collect();
    let columns = ["Field", "Type", "Arguments", "Deprecated", "Description"];
    push_table(tables, r, "Fields", &columns, rows);
}

/// `first: Int = 10`.
fn argument(arg: &Node<InputValueDefinition>) -> String {
    match &arg.default_value {
        Some(default) => format!("{}: {} = {default}", arg.name, arg.ty),
        None => format!("{}: {}", arg.name, arg.ty),
    }
}

fn members_table(tables: &mut Vec<Table>, union: &UnionType, r: &DocsRedactor) {
    let rows = union.members.iter().map(|m| vec![r.render(m)]).collect();
    push_table(tables, r, "Possible types", &["Type"], rows);
}

fn values_table(tables: &mut Vec<Table>, e: &EnumType, r: &DocsRedactor) {
    let rows = e
        .values
        .values()
        .map(|v| {
            vec![
                r.render(&v.value),
                r.render(&deprecation_reason(&v.directives).unwrap_or_default()),
                r.render(v.description.as_deref().unwrap_or_default()),
            ]
        })
        .collect();
    push_table(
        tables,
        r,
        "Values",
        &["Value", "Deprecated", "Description"],
        rows,
    );
}

fn inputs_table(tables: &mut Vec<Table>, input: &InputObjectType, r: &DocsRedactor) {
    let rows = input
        .fields
        .values()
        .map(|f| {
            let default = f.default_value.as_ref().map(ToString::to_string);
            vec![
                r.render(&f.name),
                r.render(&f.ty.to_string()),
                r.render(&default.unwrap_or_default()),
                r.render(f.description.as_deref().unwrap_or_default()),
            ]
        })
        .collect();
    let columns = ["Field", "Type", "Default", "Description"];
    push_table(tables, r, "Input fields", &columns, rows);
}

/// Add a table unless it has no rows.
fn push_table(
    tables: &mut Vec<Table>,
    r: &DocsRedactor,
    title: &str,
    columns: &[&str],
    rows: Vec<Vec<Rendered>>,
) {
    if rows.is_empty() {
        return;
    }
    tables.push(Table {
        title: r.render(title),
        columns: columns.iter().map(|c| r.render(c)).collect(),
        rows,
    });
}
