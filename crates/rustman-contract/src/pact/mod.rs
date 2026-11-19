//! Consumer-driven contracts with Pact (design D4, D5, D6): Pact files
//! generated from a collection ([`generate`]) and verified against a
//! provider ([`verify`]) with our own model ([`model`], [`read`]) and
//! matching rules ([`matching`]).

pub mod expect;
pub mod generate;
pub mod matching;
pub mod model;
pub mod path;
pub mod read;
pub mod verify;

pub use generate::{GenerateError, Generated, Generator};
pub use model::{Pact, PactVersion};
pub use read::{ReadPact, read};
pub use verify::{Outcome, Verification, Verifier};

/// Where a generated Pact file goes by default, relative to the workspace
/// root: `contracts/pacts/<consumer>-<provider>.json`.
pub fn default_file(consumer: &str, provider: &str) -> String {
    format!("contracts/pacts/{consumer}-{provider}.json")
}
