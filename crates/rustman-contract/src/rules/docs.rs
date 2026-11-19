//! Documentation-only changes (`docs-changed`) and vendor extensions
//! (`vendor-extension-changed`), at any level: operations, parameters,
//! bodies, responses and schemas.

use std::collections::BTreeSet;

use super::{Pair, Rule};
use crate::catalog::RuleId;
use crate::difference::Sink;
use crate::spec::DOC_KEYS;

/// The keys of [`DOC_KEYS`] plus `extra`.
pub struct DocsRule {
    extra: &'static [&'static str],
}

impl DocsRule {
    /// Schemas, parameters, bodies and responses: `deprecated` is
    /// documentation there.
    pub const SCHEMA: DocsRule = DocsRule {
        extra: &["deprecated"],
    };
    /// Operations: `deprecated` has its own rule.
    pub const OPERATION: DocsRule = DocsRule { extra: &[] };
}

impl Rule for DocsRule {
    fn check(&self, pair: &Pair<'_, '_>, sink: &mut Sink<'_>) {
        let changed: Vec<&str> = DOC_KEYS
            .iter()
            .chain(self.extra)
            .copied()
            .filter(|key| pair.old.get(key) != pair.new.get(key))
            .collect();
        if !changed.is_empty() {
            let what = format!("{} changed", changed.join(", "));
            pair.report(sink, RuleId::DocsChanged, what);
        }
    }
}

pub struct VendorRule;

/// Extensions the bundler adds itself.
const INTERNAL: &str = "x-rustman-";

impl Rule for VendorRule {
    fn check(&self, pair: &Pair<'_, '_>, sink: &mut Sink<'_>) {
        let keys: BTreeSet<&str> = [pair.old.value, pair.new.value]
            .into_iter()
            .filter_map(|v| v.as_object())
            .flat_map(|map| map.keys().map(String::as_str))
            .filter(|key| key.starts_with("x-") && !key.starts_with(INTERNAL))
            .collect();
        let changed: Vec<&str> = keys
            .into_iter()
            .filter(|key| pair.old.get(key) != pair.new.get(key))
            .collect();
        if !changed.is_empty() {
            let what = format!("{} changed", changed.join(", "));
            pair.report(sink, RuleId::VendorExtensionChanged, what);
        }
    }
}
