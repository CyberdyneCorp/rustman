//! Static API documentation generated from a Rustman workspace
//! (api-docs-generation).
//!
//! The pipeline has two stages (design D1): [`site`] builds a [`DocSite`]
//! page model whose every text passed the redaction step ([`Rendered`]),
//! then renderers ([`html`]) turn only that model into files. A post-build
//! [`leak`] scan checks every file again before it is written. [`serve`]
//! is the live-reload preview.

pub mod build;
mod commonmark;
mod endpoint;
mod error;
pub mod examples;
pub mod html;
pub mod leak;
mod literals;
pub mod markdown;
pub mod model;
mod openapi;
pub mod output;
mod publish;
mod reference;
mod rendered;
mod samples;
pub mod search;
pub mod serve;
mod site;
mod slug;

pub use build::{BuildOptions, BuildReport, Format, build, load_workspace, workspace_secrets};
pub use commonmark::Html;
pub use error::DocsError;
pub use examples::{ExampleSource, MAX_EXAMPLE_BYTES};
pub use model::{
    DocSite, Endpoint, Example, ReferencePage, ReferenceSet, Row, Sample, Schema, Section, Table,
};
pub use rendered::{DOCS_SENSITIVE_HEADERS, DocsRedactor, Rendered, sensitive_header};
pub use samples::DEFAULT_SAMPLES;
