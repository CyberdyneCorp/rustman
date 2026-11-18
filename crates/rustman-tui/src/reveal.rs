//! Session-only secret reveal (spec: Secret masking and reveal).
//!
//! Secrets are masked (`••••••••`) everywhere by default and their values
//! are never read on the render loop. `Alt+R` turns on the `SECRETS
//! VISIBLE` badge and reads the active environment's secrets through the
//! engine's secret store, off the loop; the values live in [`Revealed`],
//! in memory only, until the user hides them, switches environment or quits.
//! Nothing here is written anywhere, so a restart starts masked again.
//!
//! Response data stays redacted while revealed: bodies, headers and
//! captured values are redacted by the engine before the viewer gets them.

use std::path::Path;

use rustman_core::{Secret, SecretBackend};

use crate::app::{Effect, State};
use crate::toast::Severity;
use crate::workbench;

/// Secret values read for the reveal toggle. `Debug` shows only the names.
#[derive(Debug, Default)]
pub struct Revealed {
    environment: Option<String>,
    values: Vec<Secret<String>>,
}

impl Revealed {
    /// The value of secret `name` of `environment`, when it was read.
    pub fn value(&self, environment: Option<&str>, name: &str) -> Option<&str> {
        if environment.is_none() || self.environment.as_deref() != environment {
            return None;
        }
        self.values
            .iter()
            .find(|s| s.name() == name)
            .map(|s| s.expose().as_str())
    }

    pub fn clear(&mut self) {
        self.environment = None;
        self.values.clear();
    }

    pub(crate) fn set(&mut self, environment: String, values: Vec<Secret<String>>) {
        self.environment = Some(environment);
        self.values = values;
    }
}

/// The secrets the user may see right now: the read values while the badge
/// is on, nothing otherwise.
pub fn visible(state: &State) -> Option<&Revealed> {
    state.status.secrets_visible.then_some(&state.revealed)
}

/// `secrets.reveal`: show or mask secrets.
pub fn toggle(state: &mut State) -> Vec<Effect> {
    state.status.secrets_visible = !state.status.secrets_visible;
    state.revealed.clear();
    if !state.status.secrets_visible {
        state.notify(Severity::Info, "Secrets are masked");
        return Vec::new();
    }
    state.notify(
        Severity::Warn,
        "Secrets are visible until you hide them (Alt+R) or quit",
    );
    read_effect(state).into_iter().collect()
}

/// The active environment changed: forget the values read for the old one
/// and read the new one's while secrets are visible.
pub fn environment_changed(state: &mut State) -> Vec<Effect> {
    state.revealed.clear();
    if !state.status.secrets_visible {
        return Vec::new();
    }
    read_effect(state).into_iter().collect()
}

/// Read the active environment's secrets, if it has any.
fn read_effect(state: &State) -> Option<Effect> {
    let ws = workbench::workspace(state)?;
    let name = state.status.environment.as_deref()?;
    let env = ws.environments.iter().find(|e| e.name == name)?;
    if env.secrets.is_empty() {
        return None;
    }
    Some(Effect::ReadSecrets {
        root: ws.root.clone(),
        environment: env.name.clone(),
        names: env.secrets.clone(),
        backend: state.secret_backend,
    })
}

/// The secrets were read: keep them if they are still wanted.
pub fn on_read(
    state: &mut State,
    environment: String,
    result: Result<Vec<Secret<String>>, String>,
) {
    let wanted = state.status.secrets_visible
        && state.status.environment.as_deref() == Some(environment.as_str());
    match result {
        Ok(values) if wanted => state.revealed.set(environment, values),
        Ok(_) => {}
        Err(e) => state.notify(Severity::Error, format!("Cannot read secrets: {e}")),
    }
}

/// Read `names` of `environment` from the workspace's secret store (the
/// effect side). Secrets that are not set are left out.
pub fn read(
    root: &Path,
    environment: &str,
    names: &[String],
    backend: Option<SecretBackend>,
) -> Result<Vec<Secret<String>>, String> {
    let store = crate::send::secret_store(root, backend)?;
    let mut values = Vec::new();
    for name in names {
        if let Some(secret) = store.get(environment, name).map_err(|e| e.to_string())? {
            values.push(secret);
        }
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_kept_per_environment_and_hidden_from_debug() {
        let mut revealed = Revealed::default();
        revealed.set(
            "dev".into(),
            vec![Secret::new("token", "s3cr3t".to_owned())],
        );
        assert_eq!(revealed.value(Some("dev"), "token"), Some("s3cr3t"));
        assert_eq!(revealed.value(Some("staging"), "token"), None);
        assert_eq!(revealed.value(None, "token"), None);
        assert_eq!(revealed.value(Some("dev"), "other"), None);
        assert!(!format!("{revealed:?}").contains("s3cr3t"));
        revealed.clear();
        assert_eq!(revealed.value(Some("dev"), "token"), None);
    }
}
