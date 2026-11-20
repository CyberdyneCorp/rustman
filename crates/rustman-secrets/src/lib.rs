//! Secret providers (secret-providers spec): the provider types behind
//! `{{secret:<provider>/<path>}}` references, built from the
//! `[secret_providers.*]` configuration of the workspace and the user.
//!
//! The reference syntax, the resolver, the cache and the configuration
//! rules live in `rustman_core::secrets::provider`; this crate only turns a
//! [`ProviderConfig`] into a [`SecretProvider`]. Values are never written
//! anywhere: they live in the core cache for the life of the process.

#[cfg(any(
    feature = "secrets-aws",
    feature = "secrets-gcp",
    feature = "secrets-azure"
))]
pub mod cloud;
pub mod context;
pub mod describe;
mod onepassword;
mod process;
mod rest;
mod simple;
pub mod sops;
pub mod vault;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use rustman_core::error::{ConfigError, LoadError};
use rustman_core::secrets::provider::config::{self, USER_CONFIG_FILE};
use rustman_core::secrets::provider::{
    Origin, ProviderConfig, ProviderError, ProviderInfo, SecretProvider, SecretProviders,
};
use rustman_core::settings::{SETTINGS_FILE, Settings};

pub use context::Context;
pub use onepassword::{CONNECT_HOST_VAR, CONNECT_TOKEN_VAR, OnePassword};
pub use simple::{CmdProvider, EnvProvider, KeychainProvider, NotBuilt};
pub use sops::Sops;
pub use vault::Vault;

/// The providers of the workspace at `root` with `settings`, overlaid by
/// the user config (`config.toml` in the user config directory).
pub fn load(root: &Path, settings: &Settings) -> Result<SecretProviders, ConfigError> {
    let user = config::load_user(config::user_config_dir().as_deref())?;
    build_from(&settings.secret_providers, &user, &Context::new(root))
}

/// The providers of the workspace and user tables, in `ctx`.
pub fn build_from(
    workspace: &BTreeMap<String, toml::Table>,
    user: &BTreeMap<String, toml::Table>,
    ctx: &Context,
) -> Result<SecretProviders, ConfigError> {
    let invalid = |file: &str, message: String| {
        ConfigError::from(LoadError::new(Path::new(file), None, message))
    };
    let configs = config::merge(workspace, user).map_err(|m| invalid(SETTINGS_FILE, m))?;
    let mut providers = SecretProviders::new();
    for config in &configs {
        let provider = provider(config, ctx).map_err(|e| {
            let file = match config.origin {
                Origin::Workspace => SETTINGS_FILE,
                Origin::User => USER_CONFIG_FILE,
            };
            invalid(file, e.to_string())
        })?;
        providers.insert(ProviderInfo::of(config), provider);
    }
    Ok(providers)
}

/// The provider `config` describes. A cloud type compiled out of this
/// build is still listed, and fails every fetch naming its feature.
pub fn provider(
    config: &ProviderConfig,
    ctx: &Context,
) -> Result<Arc<dyn SecretProvider>, ProviderError> {
    let ctx = ctx.clone();
    Ok(match config.kind.as_str() {
        "env" => Arc::new(EnvProvider { ctx }),
        "keychain" => Arc::new(KeychainProvider::new(&ctx)?),
        "sops" => Arc::new(Sops::new(ctx, config.string("gpg_path"))),
        "onepassword" => Arc::new(onepassword(config, ctx)),
        "vault" => Arc::new(vault(config, ctx)?),
        "cmd" => Arc::new(cmd(config, ctx)?),
        #[cfg(feature = "secrets-aws")]
        "aws-secrets-manager" => Arc::new(aws(config, ctx, cloud::aws::Service::SecretsManager)),
        #[cfg(feature = "secrets-aws")]
        "aws-ssm" => Arc::new(aws(config, ctx, cloud::aws::Service::Ssm)),
        #[cfg(feature = "secrets-gcp")]
        "gcp-secret-manager" => Arc::new(cloud::gcp::Gcp::new(
            config.string("project").map(str::to_owned),
            config.string("endpoint").map(str::to_owned),
            ctx,
            config.timeout,
        )),
        #[cfg(feature = "secrets-azure")]
        "azure-key-vault" => Arc::new(cloud::azure::Azure::new(
            config.string("vault"),
            config.string("endpoint"),
            ctx,
            config.timeout,
        )),
        kind => match config::feature_of(kind) {
            Some(feature) => Arc::new(NotBuilt {
                kind: kind.to_owned(),
                feature,
            }),
            None => {
                return Err(ProviderError::Config(format!(
                    "secret provider '{}': unknown type '{kind}'",
                    config.name
                )));
            }
        },
    })
}

/// The provider types this build has, in the documented order.
pub fn built_types() -> Vec<&'static str> {
    config::TYPES
        .iter()
        .filter(|(_, _, feature)| feature.is_none_or(is_built))
        .map(|(kind, ..)| *kind)
        .collect()
}

/// The cloud features this build was compiled with.
const BUILT_FEATURES: &[&str] = &[
    #[cfg(feature = "secrets-aws")]
    "secrets-aws",
    #[cfg(feature = "secrets-gcp")]
    "secrets-gcp",
    #[cfg(feature = "secrets-azure")]
    "secrets-azure",
];

/// Whether cargo feature `feature` of this crate is enabled.
pub fn is_built(feature: &str) -> bool {
    BUILT_FEATURES.contains(&feature)
}

#[cfg(feature = "secrets-aws")]
fn aws(config: &ProviderConfig, ctx: Context, service: cloud::aws::Service) -> cloud::aws::Aws {
    let setting = |key: &str| config.string(key).map(str::to_owned);
    let settings = cloud::aws::Settings {
        region: setting("region"),
        profile: setting("profile"),
        endpoint: setting("endpoint"),
    };
    cloud::aws::Aws::new(service, settings, ctx, config.timeout)
}

/// Whether `key` (a host that receives the user's credentials) was chosen
/// by the user: set in the user config, or equal to environment variable
/// `var`. A cloned workspace must not pick where those credentials go.
fn user_chosen(config: &ProviderConfig, ctx: &Context, key: &str, var: &str) -> bool {
    let normalize = |url: &str| url.trim_end_matches('/').to_ascii_lowercase();
    match config.string(key) {
        None => true,
        Some(_) if config.set_by_user(key) => true,
        Some(value) => ctx
            .var(var)
            .is_some_and(|v| normalize(&v) == normalize(value)),
    }
}

fn onepassword(config: &ProviderConfig, ctx: Context) -> OnePassword {
    match config.string("connect_host") {
        Some(host) => {
            let trusted = user_chosen(config, &ctx, "connect_host", onepassword::CONNECT_HOST_VAR);
            let provider = OnePassword::connect(ctx, host, config.timeout);
            if trusted {
                provider
            } else {
                provider.untrusted_host()
            }
        }
        None => {
            let op = config.string("op_path").unwrap_or("op").to_owned();
            OnePassword::cli(ctx, op, config.string("account").map(str::to_owned))
        }
    }
}

fn vault(config: &ProviderConfig, ctx: Context) -> Result<Vault, ProviderError> {
    let port = config
        .options
        .get("oidc_port")
        .and_then(toml::Value::as_integer)
        .map(|p| {
            u16::try_from(p)
                .map_err(|_| ProviderError::Config(format!("vault: invalid oidc_port {p}")))
        })
        .transpose()?;
    let auth = vault::AuthMethod::parse(
        config.string("auth"),
        config.string("mount"),
        config.string("role"),
        port,
    )?;
    let trusted = user_chosen(config, &ctx, "address", vault::ADDR_VAR);
    let provider = Vault::new(
        &config.name,
        config.string("address"),
        config.string("namespace"),
        auth,
        ctx,
        config.timeout,
    );
    Ok(if trusted {
        provider
    } else {
        provider.untrusted_address()
    })
}

fn cmd(config: &ProviderConfig, ctx: Context) -> Result<CmdProvider, ProviderError> {
    let command = config
        .strings("command")
        .filter(|c| !c.is_empty())
        .ok_or_else(|| {
            ProviderError::Config(format!(
                "secret provider '{}': 'command' must be a non-empty list of strings",
                config.name
            ))
        })?;
    Ok(CmdProvider {
        name: config.name.clone(),
        command,
        ctx,
    })
}
