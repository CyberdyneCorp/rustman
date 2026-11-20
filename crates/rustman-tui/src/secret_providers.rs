//! The secret providers of the TUI's sends (add-secret-providers): built
//! once per workspace configuration and shared by every send, so their
//! in-memory cache lasts the session. "Refresh secrets" clears it.
//!
//! Building providers contacts nothing: opening the TUI and browsing
//! requests never fetches a secret; only a send does.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use rustman_core::secrets::provider::{SecretProviders, config};
use rustman_core::settings::Settings;

/// The providers built so far, with the workspace and configuration
/// (workspace and user tables) they were built for.
struct Built {
    root: PathBuf,
    tables: BTreeMap<String, toml::Table>,
    user: BTreeMap<String, toml::Table>,
    providers: Arc<SecretProviders>,
}

static BUILT: Mutex<Vec<Built>> = Mutex::new(Vec::new());

/// The providers of the workspace at `root` with `settings`, built on the
/// first send and again when `[secret_providers]` of the workspace or of
/// the user config changes.
pub fn for_workspace(root: &Path, settings: &Settings) -> Result<Arc<SecretProviders>, String> {
    for_workspace_with(root, settings, config::user_config_dir().as_deref())
}

fn for_workspace_with(
    root: &Path,
    settings: &Settings,
    user_dir: Option<&Path>,
) -> Result<Arc<SecretProviders>, String> {
    let user = config::load_user(user_dir).map_err(|e| e.to_string())?;
    let mut built = BUILT.lock().unwrap_or_else(PoisonError::into_inner);
    let tables = &settings.secret_providers;
    if let Some(found) = built
        .iter()
        .find(|b| b.root == root && &b.tables == tables && b.user == user)
    {
        return Ok(Arc::clone(&found.providers));
    }
    let ctx = rustman_secrets::Context::new(root);
    let providers =
        Arc::new(rustman_secrets::build_from(tables, &user, &ctx).map_err(|e| e.to_string())?);
    built.retain(|b| b.root != root);
    built.push(Built {
        root: root.to_path_buf(),
        tables: tables.clone(),
        user,
        providers: Arc::clone(&providers),
    });
    Ok(providers)
}

/// Forget every cached secret value ("Refresh secrets").
pub fn refresh_all() {
    let built = BUILT.lock().unwrap_or_else(PoisonError::into_inner);
    for b in built.iter() {
        b.providers.refresh();
    }
}

/// Whether providers were built for a workspace under `dir`.
#[cfg(test)]
fn built_under(dir: &Path) -> bool {
    let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let built = BUILT.lock().unwrap_or_else(PoisonError::into_inner);
    built.iter().any(|b| {
        let root = b.root.canonicalize().unwrap_or_else(|_| b.root.clone());
        root.starts_with(&dir)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env_tests::{load, workspace};
    use crate::workbench_tests::{open, press, text};

    /// "Opening the TUI doesn't fetch": loading the workspace, drawing it
    /// and opening a request that uses provider secrets build no provider
    /// (so none can fetch); the first send does.
    #[test]
    fn opening_and_browsing_build_no_provider_until_a_send() {
        let tmp = workspace(&[
            (
                "rustman.toml",
                "[workspace]\nname = \"w\"\nversion = 1\n\n[defaults]\ndefault_environment = \"dev\"\n\n[secret_providers.ci]\ntype = \"env\"\n",
            ),
            (
                "environments/dev.yaml",
                "variables:\n  token: {secret: \"ci/RUSTMAN_TUI_UNSET_SECRET\"}\n",
            ),
            (
                "secret.rman.yaml",
                "version: 1\nkind: http\nname: secret\nmethod: GET\nurl: 'http://127.0.0.1:9/x?k={{secret:ci/RUSTMAN_TUI_UNSET_SECRET}}&t={{token}}'\n",
            ),
        ]);
        let mut state = load(&tmp, 120, 40);
        open(&mut state, "secret");
        let screen = text(&state);
        assert!(screen.contains("{{secret:ci/RUSTMAN_TUI"), "{screen}");
        press(&mut state, "Alt+e");
        let _ = text(&state);
        press(&mut state, "Esc");
        assert!(
            !built_under(tmp.path()),
            "providers were built before a send"
        );

        press(&mut state, "Ctrl+r");
        assert!(built_under(tmp.path()), "the send built no provider");
    }

    #[test]
    fn providers_are_shared_until_their_configuration_changes() {
        let dir = tempfile::tempdir().unwrap();
        let user_dir = tempfile::tempdir().unwrap();
        let user = Some(user_dir.path());
        let mut settings = Settings::ad_hoc();
        let first = for_workspace_with(dir.path(), &settings, user).unwrap();
        let again = for_workspace_with(dir.path(), &settings, user).unwrap();
        assert!(Arc::ptr_eq(&first, &again));
        let table: toml::Table = toml::from_str("type = \"env\"").unwrap();
        settings.secret_providers.insert("ci".into(), table);
        let changed = for_workspace_with(dir.path(), &settings, user).unwrap();
        assert!(!Arc::ptr_eq(&first, &changed));
        assert!(changed.names().contains(&"ci".to_owned()));
        // A change to the user config is seen without a restart.
        std::fs::write(
            user_dir.path().join(config::USER_CONFIG_FILE),
            "[secret_providers.mine]\ntype = \"env\"\n",
        )
        .unwrap();
        let user_changed = for_workspace_with(dir.path(), &settings, user).unwrap();
        assert!(!Arc::ptr_eq(&changed, &user_changed));
        assert!(user_changed.names().contains(&"mine".to_owned()));
    }
}
