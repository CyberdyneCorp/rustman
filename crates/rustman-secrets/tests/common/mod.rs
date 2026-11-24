//! Helpers for the provider tests: a context with a fixed environment,
//! a registry around one provider, and resolution of one reference.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use rustman_core::secrets::provider::{
    DEFAULT_TTL, Origin, ProviderInfo, SecretProvider, SecretProviders, SecretRef, UnresolvedSecret,
};
use rustman_secrets::Context;

/// A context for `root` whose environment holds only `vars`.
pub fn context(root: &Path, vars: &[(&str, &str)]) -> Context {
    let vars: HashMap<String, String> = vars
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    Context::new(root).with_env(move |name| vars.get(name).cloned())
}

/// A registry with `provider` under `name`.
pub fn registry(
    name: &str,
    provider: Arc<dyn SecretProvider>,
    timeout: Duration,
) -> SecretProviders {
    let mut providers = SecretProviders::new();
    let info = ProviderInfo {
        name: name.to_owned(),
        kind: "test".to_owned(),
        origin: Origin::Workspace,
        auth: None,
        timeout,
        ttl: DEFAULT_TTL,
    };
    providers.insert(info, provider);
    providers
}

/// The value of `reference` (`<provider>/<path>[#field]`) from `providers`.
pub async fn fetch(
    providers: &SecretProviders,
    reference: &str,
) -> Result<String, UnresolvedSecret> {
    let reference = SecretRef::parse(reference).expect("a valid reference");
    providers
        .fetch(&reference)
        .await
        .map(|s| s.expose().clone())
}

/// `fetch` with a single provider and a generous timeout.
pub async fn fetch_one(
    name: &str,
    provider: Arc<dyn SecretProvider>,
    reference: &str,
) -> Result<String, String> {
    let providers = registry(name, provider, Duration::from_secs(20));
    fetch(&providers, reference)
        .await
        .map_err(|e| e.to_string())
}

/// The `[secret_providers]` tables of a TOML document.
pub fn tables(text: &str) -> std::collections::BTreeMap<String, toml::Table> {
    let table: toml::Table = toml::from_str(text).unwrap();
    table
        .get("secret_providers")
        .and_then(toml::Value::as_table)
        .map(|t| {
            t.iter()
                .map(|(k, v)| (k.clone(), v.as_table().unwrap().clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// The providers of the workspace and user config documents, in `ctx`.
pub fn configured(workspace: &str, user: &str, ctx: &Context) -> SecretProviders {
    rustman_secrets::build_from(&tables(workspace), &tables(user), ctx).unwrap()
}

/// A local port nothing listens on: connections are refused at once.
pub fn closed_port_url() -> String {
    rustman_test_support::ports::closed_url()
}
