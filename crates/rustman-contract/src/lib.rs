//! Contract testing for Rustman (the `contract-testing` capability).
//!
//! [`diff`] compares two OpenAPI 3.0/3.1 documents, loaded with
//! [`Spec::load`] through `rustman-explore`'s importer, and classifies every
//! difference with a rule of the versioned [`catalog`]: `breaking`,
//! `non-breaking` or `unknown`. Schemas are compared with a direction
//! ([`rules::Direction`]): a stricter request or a response that loses
//! guarantees breaks clients.
//!
//! A [`Verdict`] applies the [`allowlist`] and the strictness flags to a
//! diff; [`report`] turns it into a `kind: contract` report. With `--base`,
//! the old document is read from a Git tree ([`git::GitTree`]).
//!
//! [`pact`] generates Pact files from a collection and verifies them against
//! a provider through the safety policy ([`sender::GovernedSenders`]);
//! [`conformance`] checks recorded exchanges against a document without
//! sending anything.
//!
//! This crate holds no UI code; the CLI is a thin adapter over it.

pub mod allowlist;
pub mod catalog;
mod compare;
pub mod conformance;
pub mod difference;
pub mod git;
pub mod input;
pub mod output;
pub mod pact;
pub mod report;
pub mod rules;
pub mod sender;
pub mod spec;
pub mod verdict;
pub mod walk;

pub use catalog::{CATALOG_VERSION, Classification, RuleId, RuleInfo};
pub use compare::diff;
pub use difference::{Diff, Difference};
pub use spec::Spec;
pub use verdict::{Strictness, Verdict};
