//! Run reports for Rustman (`rustman.report/v1`, `kind: run`) and their
//! writers: pretty, json, junit and tap. Reports are built from
//! [`RunRecord`]s, whose every text field is already redacted.
//!
//! [`RunRecord`]: rustman_core::run::RunRecord

mod json;
mod junit;
mod pretty;
mod reporter;
mod tap;

pub use json::{KIND_RUN, SCHEMA, json};
pub use junit::junit;
pub use pretty::{pretty, pretty_styled};
pub use reporter::{Format, Reporter, ReporterError};
pub use tap::tap;
