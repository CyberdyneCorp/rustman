//! The page model built from a workspace (api-docs-generation "Structure
//! follows the workspace"): collections become sections and nested folders
//! subsections, in workspace order; requests become endpoint pages, except
//! the ones with `docs: {hidden: true}`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use rustman_core::collection::{Collection, Entry};
use rustman_core::{RequestRef, Workspace};

use crate::commonmark::to_html;
use crate::endpoint::endpoint;
use crate::examples::Examples;
use crate::model::{DocSite, Endpoint, Row, Section};
use crate::openapi::OpenApiIndex;
use crate::reference::{ReferenceInput, references};
use crate::rendered::DocsRedactor;
use crate::samples::Samples;
use crate::slug::Slugs;

/// What the model is built from.
pub struct SiteInput<'a> {
    pub workspace: &'a Workspace,
    pub redactor: &'a DocsRedactor,
    pub examples: &'a Examples,
    pub openapi: &'a OpenApiIndex,
    pub samples: &'a Samples,
    /// The published variables (`[docs].publish_variables` with `--env`).
    pub variables: Vec<Row>,
}

/// The page model of `input.workspace`.
pub fn build_site(input: SiteInput<'_>) -> DocSite {
    let workspace = input.workspace;
    let requests: BTreeMap<PathBuf, RequestRef<'_>> = workspace
        .requests()
        .into_iter()
        .map(|r| (r.entry.path.clone(), r))
        .collect();
    let mut builder = Builder {
        input: &input,
        requests: &requests,
        sections: Slugs::default(),
        endpoints: Slugs::default(),
    };
    let mut sections = Vec::new();
    let root = &workspace.tree;
    if let Some(own) = builder.root_section(root) {
        sections.push(own);
    }
    for entry in &root.entries {
        if let Entry::Collection(child) = entry
            && let Some(section) = builder.section(child, 1)
        {
            sections.push(section);
        }
    }
    let visible: Vec<RequestRef<'_>> = workspace
        .requests()
        .into_iter()
        .filter(|r| !r.request().docs().is_some_and(|d| d.is_hidden()))
        .collect();
    let reference_input = ReferenceInput {
        workspace,
        requests: &visible,
        redactor: input.redactor,
    };
    let references = references(
        &reference_input,
        &mut builder.sections,
        &mut Slugs::default(),
    );
    let title = workspace
        .settings
        .docs
        .title
        .as_deref()
        .unwrap_or(&workspace.settings.workspace.name);
    let mut site = DocSite {
        title: input.redactor.render(title),
        sections,
        variables: input.variables.clone(),
        published_examples: Vec::new(),
        references,
    };
    site.published_examples = site
        .endpoints()
        .iter()
        .filter(|e| e.example.is_some())
        .map(|e| e.id.as_str().to_owned())
        .collect();
    site
}

struct Builder<'a, 'w> {
    input: &'a SiteInput<'w>,
    requests: &'a BTreeMap<PathBuf, RequestRef<'w>>,
    sections: Slugs,
    endpoints: Slugs,
}

impl Builder<'_, '_> {
    /// The workspace's own requests (at its root), as a section named
    /// after the workspace.
    fn root_section(&mut self, root: &Collection) -> Option<Section> {
        let name = self.input.workspace.settings.workspace.name.clone();
        let slug = self.sections.next(&name);
        let endpoints = self.endpoints_of(root, &slug);
        if endpoints.is_empty() {
            return None;
        }
        Some(Section {
            slug,
            title: self.input.redactor.render(&name),
            description: None,
            depth: 1,
            endpoints,
            sections: Vec::new(),
        })
    }

    /// `collection` as a section; `None` when it documents nothing.
    fn section(&mut self, collection: &Collection, depth: usize) -> Option<Section> {
        let r = self.input.redactor;
        let slug = self.sections.next(&collection.path.to_string_lossy());
        let endpoints = self.endpoints_of(collection, &slug);
        let sections: Vec<Section> = collection
            .entries
            .iter()
            .filter_map(|e| match e {
                Entry::Collection(child) => self.section(child, depth + 1),
                Entry::Request(_) => None,
            })
            .collect();
        if endpoints.is_empty() && sections.is_empty() {
            return None;
        }
        let description = collection.meta.description.as_deref();
        Some(Section {
            slug,
            title: r.render(&collection.name()),
            description: description.map(|d| to_html(&r.render(d))),
            depth,
            endpoints,
            sections,
        })
    }

    /// The visible requests directly in `collection`, in run order.
    fn endpoints_of(&mut self, collection: &Collection, section: &str) -> Vec<Endpoint> {
        let mut out = Vec::new();
        for entry in &collection.entries {
            let Entry::Request(request) = entry else {
                continue;
            };
            if request.request.docs().is_some_and(|d| d.is_hidden()) {
                continue;
            }
            let Some(request) = self.requests.get(&request.path) else {
                continue;
            };
            let slug = self.endpoints.next(&request.entry.id());
            out.push(endpoint(slug, section, request, self.input));
        }
        out
    }
}
