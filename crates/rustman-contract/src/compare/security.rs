//! Security requirements. An operation accepts any one of its alternatives
//! (each a set of schemes with scopes; an empty one is anonymous access).
//! An old alternative still works when some new alternative asks for no
//! more than it did. When none does, every client breaks
//! (`security-requirement-removed`); when only some do, clients of the
//! others might (`security-alternative-removed`). API keys that move
//! between query, header and cookie are `parameter-location-changed`.

use std::collections::{BTreeMap, BTreeSet};

use rustman_explore::openapi::model::{SecurityRequirement, SecurityScheme};
use rustman_explore::openapi::pointer;

use super::Sides;
use super::view::OpView;
use crate::catalog::RuleId;
use crate::difference::Sink;
use crate::spec::Spec;

/// Scheme names with their scopes.
type Alternative = BTreeMap<String, BTreeSet<String>>;

pub(super) fn compare(sides: Sides<'_>, old: &OpView<'_>, new: &OpView<'_>, sink: &mut Sink<'_>) {
    let old_alts = alternatives(sides.old, old);
    let new_alts = alternatives(sides.new, new);
    let pointers = (pointer(old), pointer(new));
    let lost: Vec<&Alternative> = old_alts
        .iter()
        .filter(|alt| !new_alts.iter().any(|n| asks_no_more(n, alt)))
        .collect();
    if lost.len() == old_alts.len() && !lost.is_empty() {
        let message = format!(
            "security: no longer accepts {}",
            shown(&old_alts.iter().collect::<Vec<_>>())
        );
        sink.push(
            RuleId::SecurityRequirementRemoved,
            message,
            &pointers.0,
            &pointers.1,
        );
    } else if !lost.is_empty() {
        let message = format!("security: no longer accepts {}", shown(&lost));
        sink.push(
            RuleId::SecurityAlternativeRemoved,
            message,
            &pointers.0,
            &pointers.1,
        );
    }
    let gained: Vec<&Alternative> = new_alts.iter().filter(|n| !old_alts.contains(n)).collect();
    if !gained.is_empty() {
        let message = format!("security: also accepts {}", shown(&gained));
        sink.push(
            RuleId::SecurityAlternativeAdded,
            message,
            &pointers.0,
            &pointers.1,
        );
    }
    api_keys(sides, &old_alts, sink);
}

/// The operation's alternatives: its own `security`, else the document's.
fn alternatives(spec: &Spec, view: &OpView<'_>) -> Vec<Alternative> {
    let requirements: &[SecurityRequirement] =
        view.op.security.as_deref().unwrap_or(&spec.model.security);
    if requirements.is_empty() {
        return vec![Alternative::new()];
    }
    requirements
        .iter()
        .map(|req| {
            req.iter()
                .map(|(scheme, scopes)| (scheme.clone(), scopes.iter().cloned().collect()))
                .collect()
        })
        .collect()
}

/// Whether a client satisfying `old` also satisfies `new`.
fn asks_no_more(new: &Alternative, old: &Alternative) -> bool {
    new.iter().all(|(scheme, scopes)| {
        old.get(scheme)
            .is_some_and(|granted| scopes.is_subset(granted))
    })
}

/// Where the operation's requirements are declared.
fn pointer(view: &OpView<'_>) -> String {
    match view.node.child("security") {
        Some(own) => own.pointer,
        None => "/security".to_owned(),
    }
}

/// `basicAuth or bearerAuth`, `anonymous access`, `oauth (read, write)`.
fn shown(alternatives: &[&Alternative]) -> String {
    alternatives
        .iter()
        .map(|alt| {
            if alt.is_empty() {
                return "anonymous access".to_owned();
            }
            alt.iter()
                .map(|(scheme, scopes)| match scopes.len() {
                    0 => scheme.clone(),
                    _ => format!(
                        "{scheme} ({})",
                        scopes.iter().cloned().collect::<Vec<_>>().join(", ")
                    ),
                })
                .collect::<Vec<_>>()
                .join(" + ")
        })
        .collect::<Vec<_>>()
        .join(" or ")
}

/// API key schemes the operation used whose name or location changed.
fn api_keys(sides: Sides<'_>, old_alts: &[Alternative], sink: &mut Sink<'_>) {
    let used: BTreeSet<&String> = old_alts.iter().flat_map(|alt| alt.keys()).collect();
    for scheme in used {
        let (Some(old), Some(new)) = (api_key(sides.old, scheme), api_key(sides.new, scheme))
        else {
            continue;
        };
        if old != new {
            let message = format!(
                "API key '{}' (security scheme '{scheme}'): moved from {} '{}' to {} '{}'",
                new.0, old.1, old.0, new.1, new.0
            );
            let at = pointer::push("/components/securitySchemes", scheme);
            sink.push(RuleId::ParameterLocationChanged, message, &at, &at);
        }
    }
}

/// The parameter name and location of the API key scheme `name`.
fn api_key(spec: &Spec, name: &str) -> Option<(String, String)> {
    spec.model
        .security_schemes
        .iter()
        .find(|(n, _)| n == name)
        .and_then(|(_, scheme)| match scheme {
            SecurityScheme::ApiKey { name, location } => {
                Some((name.clone(), format!("{location:?}").to_lowercase()))
            }
            _ => None,
        })
}
