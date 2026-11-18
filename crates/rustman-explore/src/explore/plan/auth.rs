//! Auth enforcement probes: for each operation that requires credentials,
//! the request without credentials and with a well-formed invalid one.

use super::inputs::Inputs;
use super::single::probe;
use super::{AuthMode, Check, Family, Probe};
use crate::openapi::model::{Operation, ParameterLocation, SecurityScheme};

/// The invalid credential sent in place of a real one.
pub const INVALID_TOKEN: &str = "invalid-rustman-credential";

/// `Basic` credentials `invalid:invalid`.
const INVALID_BASIC: &str = "Basic aW52YWxpZDppbnZhbGlk";

const EXPECTED: &str = "401 or 403";

pub fn probes(inputs: &Inputs<'_>, ops: &[&Operation]) -> Vec<Probe> {
    let mut probes = Vec::new();
    for op in ops {
        if !requires_credentials(inputs, op) {
            continue;
        }
        let path = inputs.loose_path(op);
        let bare = inputs.request(
            "probe",
            op,
            &path,
            AuthMode::None,
            Some(Check::RequiresAuth),
        );
        let mut invalid = bare.clone();
        probes.push(probe(
            Family::Auth,
            op,
            "no credentials",
            EXPECTED,
            Ok(vec![bare]),
        ));
        if first_scheme(inputs, op).is_some_and(|scheme| add_invalid(&mut invalid, scheme)) {
            probes.push(probe(
                Family::Auth,
                op,
                "invalid credentials",
                EXPECTED,
                Ok(vec![invalid]),
            ));
        }
    }
    probes
}

/// Whether `op` requires credentials: it has security requirements and
/// none of them is empty (an empty one makes auth optional).
pub fn requires_credentials(inputs: &Inputs<'_>, op: &Operation) -> bool {
    let requirements = op.security.as_ref().unwrap_or(&inputs.model.security);
    !requirements.is_empty() && !requirements.iter().any(Vec::is_empty)
}

/// The first scheme `op` requires, when the document defines it.
fn first_scheme<'a>(inputs: &Inputs<'a>, op: &'a Operation) -> Option<&'a SecurityScheme> {
    let requirements = op.security.as_ref().unwrap_or(&inputs.model.security);
    let (name, _) = requirements.first()?.first()?;
    inputs
        .model
        .security_schemes
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, s)| s)
}

/// Add the invalid credential of `scheme`; `false` for schemes that have
/// no well-formed invalid credential (mutual TLS, other HTTP schemes).
fn add_invalid(step: &mut super::Step, scheme: &SecurityScheme) -> bool {
    let header = |step: &mut super::Step, name: &str, value: String| {
        step.headers.push((name.to_owned(), value));
    };
    match scheme {
        SecurityScheme::HttpBearer { .. }
        | SecurityScheme::OAuth2 { .. }
        | SecurityScheme::OpenIdConnect { .. } => {
            header(step, "Authorization", format!("Bearer {INVALID_TOKEN}"));
        }
        SecurityScheme::HttpBasic => header(step, "Authorization", INVALID_BASIC.to_owned()),
        SecurityScheme::ApiKey { name, location } => match location {
            ParameterLocation::Query => step.query.push((name.clone(), INVALID_TOKEN.to_owned())),
            ParameterLocation::Cookie => header(step, "Cookie", format!("{name}={INVALID_TOKEN}")),
            _ => header(step, name, INVALID_TOKEN.to_owned()),
        },
        SecurityScheme::Http { .. } | SecurityScheme::MutualTls => return false,
    }
    true
}
