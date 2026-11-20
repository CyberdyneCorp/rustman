//! Reports for Rustman (`rustman.report/v1`): the canonical [`Report`] of a
//! run (`kind: run`), an exploration (`kind: exploration`), a contract
//! check (`kind: contract`) or a load test (`kind: bench`), its JSON
//! Schema, the renderers (JSON, JUnit XML, SARIF, Markdown, HTML), `report
//! diff`, and the `rustman run --reporter` writers (pretty, json, junit,
//! tap). Reports are built from records whose every text field is already
//! redacted, so no renderer can show a secret.

pub mod bench;
pub mod contract;
pub mod diff;
pub mod exploration;
mod json;
mod pretty;
pub mod render;
mod report;
mod reporter;
pub mod run;
mod schema;
mod tap;

pub use diff::{Diff, KindMismatch, diff};
pub use json::json;
pub use pretty::{pretty, pretty_styled};
pub use render::{RenderFormat, RenderOptions, ReportTarget, render};
pub use report::{ReadError, Report, SCHEMA};
pub use reporter::{Format, Reporter, ReporterError};
pub use run::{KIND_RUN, RunReport};
pub use schema::report_schema;
pub use tap::tap;

/// `run` as a JUnit XML document, ending in a newline.
pub fn junit(run: &rustman_core::run::RunRecord) -> String {
    render::junit::run_junit(&RunReport::new(run))
}
