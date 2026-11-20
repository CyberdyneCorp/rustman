//! Placeholder mode (runner-parity design D10): resolve a request while
//! keeping its variables as `{{name}}` markers, for snippets and docs that
//! show how a request is built rather than one concrete instance of it.
//!
//! Secrets are never read: a secret variable (from the store or a provider)
//! becomes `«redacted:<name>»` and a `{{secret:...}}` reference stays as
//! written.
//! Dynamic values (`{{$uuid}}`, `{{$env NAME}}`) are still evaluated,
//! unless the resolver keeps them ([`Resolver::keep_dynamics`]).

use std::cell::RefCell;
use std::rc::Rc;

use super::request::{ResolvedRequest, resolve_request};
use super::resolver::{ResolveError, Resolver};
use super::source::{Layer, Lookup, VarSource};
use crate::secret::placeholder;
use crate::workspace::RequestRef;

/// A variable a placeholder-mode request refers to, with the value the
/// chain gives it now (`{{name}}` when it has none).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaceholderVar {
    pub name: String,
    pub value: String,
}

/// `request` with every non-secret variable left as `{{name}}`, and those
/// variables in order of first use.
pub fn resolve_placeholders(
    request: &RequestRef<'_>,
    resolver: &Resolver<'_>,
) -> Result<(ResolvedRequest, Vec<PlaceholderVar>), ResolveError> {
    let used = Rc::new(RefCell::new(Vec::new()));
    let keep = resolver
        .empty_like()
        .keeping_secret_refs()
        .with(KeepVariables {
            inner: resolver,
            used: Rc::clone(&used),
        });
    let resolved = resolve_request(request, &keep)?;
    let vars = used.take();
    Ok((resolved, vars))
}

/// Answers every lookup with the variable's own marker.
struct KeepVariables<'r, 'a> {
    inner: &'r Resolver<'a>,
    used: Rc<RefCell<Vec<PlaceholderVar>>>,
}

impl VarSource for KeepVariables<'_, '_> {
    fn layer(&self) -> Layer {
        Layer::Override
    }

    fn lookup(&self, name: &str) -> Option<Lookup> {
        let marker = format!("{{{{{name}}}}}");
        let value = match self.inner.lookup(name) {
            Some(Lookup::Secret(_) | Lookup::SecretNotSet { .. } | Lookup::Provider(_)) => {
                return Some(Lookup::Value(placeholder(name)));
            }
            Some(Lookup::Value(value)) => value,
            None => marker.clone(),
        };
        let mut used = self.used.borrow_mut();
        if !used.iter().any(|v| v.name == name) {
            used.push(PlaceholderVar {
                name: name.to_owned(),
                value,
            });
        }
        Some(Lookup::Value(marker))
    }
}
