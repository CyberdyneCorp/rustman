//! Variables: `{{name}}` templates, dynamic values and the precedence chain
//! (design D8, environments spec).

pub mod dynamic;
pub mod request;
pub mod resolver;
pub mod source;
pub mod template;

pub use dynamic::Dynamic;
pub use request::{
    ResolvedBody, ResolvedFile, ResolvedPair, ResolvedPart, ResolvedPartSource, ResolvedRequest,
    TlsFiles, resolve_request,
};
pub use resolver::{
    Interpolator, Missing, ResolveError, Resolved, Resolver, SecretSpan, UsedSecret,
};
pub use source::{
    EnvironmentSource, Layer, Lookup, NoSecrets, SecretLookup, VarSource, VariablesSource,
    parse_overrides,
};
pub use template::{Segment, Template, TemplateError};
