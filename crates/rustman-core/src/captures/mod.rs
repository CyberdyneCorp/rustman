//! Captures: values extracted from a response for later requests
//! (assertions spec "Captures", design D9).
//!
//! Captures run after assertions. A captured value is available to every
//! later request of the same run through [`RunCaptures`] (the `Capture`
//! layer of the precedence chain). `persist: true` also writes it to
//! `.rustman/state/<env>.yaml` ([`PersistedState`]). A capture whose variable
//! is listed under the active environment's `secrets` is a secret: it is
//! stored in the [`SecretStore`] (never in a state file) and resolves as a
//! secret, so every sink redacts it. A non-secret value that contains a
//! known secret (a server echoing it back) is never written to a state file,
//! and [`RunCaptures::taint`] turns it into a secret for the rest of the run.

mod extract;
mod state;

use std::path::Path;

use serde::Serialize;

use crate::assertions::ResponseView;
use crate::environment::Environment;
use crate::model::{Capture, Checked};
use crate::redact::Redactor;
use crate::secret::Secret;
use crate::secrets::SecretStore;
use crate::vars::{Layer, Lookup, VarSource};

use extract::{Miss, extract};
pub use state::{NO_ENVIRONMENT, PersistedState, STATE_FILES_DIR};

/// Values captured so far in one run.
#[derive(Clone, Debug, Default)]
pub struct RunCaptures {
    values: Vec<(String, Lookup)>,
    /// Captures that became secrets because their value held one; the
    /// secret store does not have them, so a request using one cannot be
    /// replayed from history.
    derived: Vec<String>,
}

impl RunCaptures {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set `name`, replacing an earlier capture of the same name.
    pub fn set(&mut self, name: &str, value: Lookup) {
        self.values.retain(|(n, _)| n != name);
        self.derived.retain(|n| n != name);
        self.values.push((name.to_owned(), value));
    }

    /// Turn every non-secret value that holds, or is a piece of, a value
    /// `known` redacts (a server echoing a secret back) into a secret of its own name, so every
    /// later request using it redacts it too. Returns the new secrets.
    pub fn taint(&mut self, known: &Redactor) -> Vec<Secret<String>> {
        self.make_secret(|_, text| known.reveals_secret(text))
    }

    /// Turn every non-secret capture `environment` lists under `secrets`
    /// into a secret. [`apply`] does so only when it persists captures;
    /// a run that does not (a load test) calls this instead, so those
    /// values are redacted all the same. Returns the new secrets.
    pub fn declare_secrets(&mut self, environment: &Environment) -> Vec<Secret<String>> {
        self.make_secret(|name, _| environment.is_secret(name))
    }

    /// Turn the non-secret values `is_secret(name, value)` accepts into
    /// secrets the secret store does not have.
    fn make_secret(&mut self, is_secret: impl Fn(&str, &str) -> bool) -> Vec<Secret<String>> {
        let mut made = Vec::new();
        for (name, value) in &mut self.values {
            let Lookup::Value(text) = value else {
                continue;
            };
            if is_secret(name, text) {
                let secret = Secret::new(name.clone(), std::mem::take(text));
                made.push(secret.clone());
                *value = Lookup::Secret(secret);
                self.derived.push(name.clone());
            }
        }
        made
    }

    /// The names of the captures [`RunCaptures::taint`] or
    /// [`RunCaptures::declare_secrets`] made secret.
    pub fn derived(&self) -> &[String] {
        &self.derived
    }

    pub fn get(&self, name: &str) -> Option<&Lookup> {
        self.values.iter().find(|(n, _)| n == name).map(|(_, v)| v)
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Set every value of `other`, replacing captures of the same names.
    pub fn merge(&mut self, other: RunCaptures) {
        for (name, value) in other.values {
            self.set(&name, value);
        }
        self.derived.extend(other.derived);
    }
}

impl VarSource for &RunCaptures {
    fn layer(&self) -> Layer {
        Layer::Capture
    }

    fn lookup(&self, name: &str) -> Option<Lookup> {
        self.get(name).cloned()
    }
}

/// Where captures are persisted: the workspace and active environment.
pub struct Persistence<'a> {
    pub root: &'a Path,
    pub environment: Option<&'a Environment>,
    pub secrets: &'a SecretStore,
    /// The secrets of the request whose response is captured. A value
    /// containing one of them is not written to a state file.
    pub redactor: &'a Redactor,
}

impl Persistence<'_> {
    fn env_name(&self) -> Option<&str> {
        self.environment.map(|e| e.name.as_str())
    }

    fn is_secret(&self, var: &str) -> bool {
        self.environment.is_some_and(|e| e.is_secret(var))
    }
}

/// How one capture ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureOutcome {
    Captured,
    /// Nothing matched; the variable stays unset.
    NoMatch,
    /// The capture could not be evaluated.
    Error,
}

/// The result of one capture. It never holds the captured value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CaptureResult {
    pub var: String,
    pub outcome: CaptureOutcome,
    /// Why nothing was captured, such as `capture 'id': no match for $.id`.
    pub message: Option<String>,
    pub required: bool,
    /// Written to the state file or the secret store.
    pub persisted: bool,
    pub secret: bool,
}

/// What applying a request's captures produced.
#[derive(Clone, Debug, Default)]
pub struct CaptureReport {
    pub results: Vec<CaptureResult>,
    /// Problems that do not fail the request (an optional capture that
    /// matched nothing, a value that could not be persisted).
    pub warnings: Vec<String>,
    /// Problems that fail the request (a required capture that matched nothing).
    pub failures: Vec<String>,
    /// Secret values captured; every sink showing this exchange must redact them.
    pub secrets: Vec<Secret<String>>,
}

impl CaptureReport {
    pub fn failed(&self) -> bool {
        !self.failures.is_empty()
    }
}

/// Apply `captures` to `response`, in order, adding values to `scope` and
/// persisting them where asked. Without `persistence` (no workspace),
/// `persist: true` only warns.
pub fn apply(
    captures: &[Checked<Capture>],
    response: &ResponseView<'_>,
    scope: &mut RunCaptures,
    persistence: Option<&Persistence<'_>>,
) -> CaptureReport {
    let mut report = CaptureReport::default();
    for capture in captures {
        let result = apply_one(capture, response, scope, persistence, &mut report);
        report.results.push(result);
    }
    report
}

fn apply_one(
    capture: &Capture,
    response: &ResponseView<'_>,
    scope: &mut RunCaptures,
    persistence: Option<&Persistence<'_>>,
    report: &mut CaptureReport,
) -> CaptureResult {
    let var = capture.var.as_str();
    let required = capture.required.unwrap_or(false);
    let secret = persistence.is_some_and(|p| p.is_secret(var));
    let mut result = CaptureResult {
        var: var.to_owned(),
        outcome: CaptureOutcome::Captured,
        message: None,
        required,
        persisted: false,
        secret,
    };
    let value = match extract(capture, response) {
        Ok(value) => value,
        Err(miss) => {
            let (outcome, why) = match miss {
                Miss::NoMatch(why) => (CaptureOutcome::NoMatch, why),
                Miss::Error(why) => (CaptureOutcome::Error, why),
            };
            let message = format!("capture '{var}': {why}");
            if required {
                report.failures.push(message.clone());
            } else {
                report.warnings.push(message.clone());
            }
            result.outcome = outcome;
            result.message = Some(message);
            return result;
        }
    };
    let lookup = if secret {
        let value = Secret::new(var, value);
        report.secrets.push(value.clone());
        Lookup::Secret(value)
    } else {
        Lookup::Value(value)
    };
    result.persisted = store(capture, &lookup, persistence, report);
    scope.set(var, lookup);
    result
}

/// Persist a captured value: a secret to the secret store (always), any
/// other value to the state file when `persist: true`. Returns whether it
/// was written.
fn store(
    capture: &Capture,
    value: &Lookup,
    persistence: Option<&Persistence<'_>>,
    report: &mut CaptureReport,
) -> bool {
    let var = capture.var.as_str();
    let persist = capture.persist.unwrap_or(false);
    let Some(p) = persistence else {
        if persist {
            let message = format!("capture '{var}': not persisted (no workspace)");
            report.warnings.push(message);
        }
        return false;
    };
    let written = match value {
        Lookup::Secret(secret) => {
            let env = p.env_name().unwrap_or(NO_ENVIRONMENT);
            p.secrets.set(env, secret).map(|_| ())
        }
        Lookup::Value(text) if persist && holds_secret(p.redactor, &report.secrets, text) => {
            let message = format!("capture '{var}': not persisted: the value contains a secret");
            report.warnings.push(message);
            return false;
        }
        Lookup::Value(text) if persist => PersistedState::persist(p.root, p.env_name(), var, text),
        _ => return false,
    };
    match written {
        Ok(()) => true,
        Err(e) => {
            report
                .warnings
                .push(format!("capture '{var}': not persisted: {e}"));
            false
        }
    }
}

/// Whether `text` contains (or is a piece of) a secret known to `redactor` or captured as
/// `captured` earlier in the same response.
fn holds_secret(redactor: &Redactor, captured: &[Secret<String>], text: &str) -> bool {
    let mut known = redactor.clone();
    for secret in captured {
        known.add(secret);
    }
    known.reveals_secret(text)
}

#[cfg(test)]
mod tests;
