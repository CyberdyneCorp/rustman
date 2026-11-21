//! `rustman record --save`: when the session ends, the captures are saved
//! as request files (traffic-recorder spec: Save captures as request files,
//! Secrets become placeholders). Everything is checked before the proxy
//! starts, so a long recording is never lost to a typo.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::Args;
use rustman_core::secrets::BACKEND_VAR;
use rustman_core::{ConfigError, Secret, SecretBackend, SecretError, SecretStore};
use rustman_proxy::Capture;
use rustman_proxy::save::{DEFAULT_ENVIRONMENT, SaveOptions, Saved, save};

use crate::commands::import::destination;
use crate::exit::{CliError, SUCCESS};

/// Printed when a session recorded no request to save.
const NOTHING_SAVED: &str = "No requests recorded; nothing saved";

#[derive(Debug, Args)]
pub(super) struct SaveArgs {
    /// When recording stops (Ctrl+C), save the captures as request files
    #[arg(long)]
    save: bool,
    /// Folder for the saved request files (default: recorded/<host>/)
    #[arg(long, value_name = "DIR", requires = "save")]
    into: Option<PathBuf>,
    /// Environment that receives base_url, path defaults and secret names
    /// (default: recorded)
    #[arg(long, value_name = "NAME", requires = "save")]
    env: Option<String>,
    /// Store the raw secret values in the secret store (keychain); without
    /// it they are kept nowhere and must be set with 'rustman secret set'
    #[arg(long, requires = "save")]
    store_secrets: bool,
}

/// A save to run when recording stops.
pub(super) struct Saving {
    root: PathBuf,
    into: Option<String>,
    environment: String,
    store: Option<SecretStore>,
}

impl SaveArgs {
    /// The save to run at the end, checked now; `None` without `--save`.
    pub(super) fn prepare(&self, root: Option<&Path>) -> Result<Option<Saving>, CliError> {
        if !self.save {
            return Ok(None);
        }
        let root = root.ok_or_else(|| {
            ConfigError::Usage(
                "record: --save needs a workspace; run 'rustman init' or pass --workspace"
                    .to_owned(),
            )
        })?;
        let environment = self
            .env
            .clone()
            .unwrap_or_else(|| DEFAULT_ENVIRONMENT.to_owned());
        if rustman_import::secrets::environment_name(&environment) != environment {
            let message = format!(
                "record: invalid --env {environment:?} (use lowercase letters, digits and -)"
            );
            return Err(ConfigError::Usage(message).into());
        }
        let into = self
            .into
            .as_deref()
            .map(|into| destination(root, into))
            .transpose()?;
        let store = if self.store_secrets {
            let backend = SecretBackend::from_env_value(std::env::var(BACKEND_VAR).ok().as_deref())
                .map_err(ConfigError::from)?;
            Some(SecretStore::open(root, backend))
        } else {
            None
        };
        Ok(Some(Saving {
            root: root.to_path_buf(),
            into,
            environment,
            store,
        }))
    }
}

impl Saving {
    /// Save `captures` and print what was written: on stdout, or on stderr
    /// with `--json` so stdout holds only JSON lines.
    pub(super) fn save(self, captures: &[Arc<Capture>], json: bool) -> Result<u8, CliError> {
        let options = SaveOptions {
            root: &self.root,
            into: self.into.clone(),
            environment: self.environment.clone(),
        };
        let store_batch = |batch: &[(&str, &Secret<String>)]| -> Result<(), SecretError> {
            match &self.store {
                Some(store) => store.set_many(batch),
                None => Ok(()),
            }
        };
        let store = self.store.is_some().then_some(&store_batch as _);
        let saved = save(captures, &options, store).map_err(save_error);
        if let Some(store) = &self.store {
            super::super::print_warnings(&store.take_warnings());
        }
        let saved = saved?;
        if saved.is_nothing() {
            eprintln!("{NOTHING_SAVED}");
            return Ok(SUCCESS);
        }
        let summary = summary(&saved, &self.environment);
        for warning in &summary.warnings {
            eprintln!("{warning}");
        }
        for line in &summary.lines {
            if json {
                eprintln!("{line}");
            } else {
                println!("{line}");
            }
        }
        Ok(SUCCESS)
    }
}

fn save_error(err: rustman_proxy::save::SaveError) -> CliError {
    match err {
        rustman_proxy::save::SaveError::Import(e) => e.into(),
        other => ConfigError::Usage(other.to_string()).into(),
    }
}

/// What a save prints.
struct Summary {
    warnings: Vec<String>,
    lines: Vec<String>,
}

/// What was saved, the secrets parameterized (names only) and warnings.
fn summary(saved: &Saved, environment: &str) -> Summary {
    Summary {
        warnings: saved.warnings.clone(),
        lines: summary_lines(
            saved.written,
            saved.unchanged,
            &saved.secrets,
            saved.stored,
            environment,
        ),
    }
}

fn summary_lines(
    written: usize,
    unchanged: usize,
    secrets: &[String],
    stored: bool,
    environment: &str,
) -> Vec<String> {
    let files = if written == 1 { "file" } else { "files" };
    let mut lines = vec![format!(
        "Saved {written} request {files} ({unchanged} unchanged), environment '{environment}'"
    )];
    if secrets.is_empty() {
        return lines;
    }
    let names = secrets.join(", ");
    lines.push(if stored {
        format!("Secrets parameterized and stored in the secret store: {names}")
    } else {
        format!(
            "Secrets parameterized (values not stored; use --store-secrets or \
             'rustman secret set <name> --env {environment}'): {names}"
        )
    });
    lines
}

#[cfg(test)]
mod tests {
    use super::{Saving, summary_lines};

    #[test]
    fn a_session_without_requests_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("rustman.toml"),
            "[workspace]\nname = \"rec\"\nversion = 1\n",
        )
        .unwrap();
        let saving = Saving {
            root: dir.path().to_path_buf(),
            into: None,
            environment: "recorded".to_owned(),
            store: None,
        };
        assert_eq!(saving.save(&[], false).unwrap(), crate::exit::SUCCESS);
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(entries, ["rustman.toml"]);
    }

    #[test]
    fn the_summary_counts_files_in_singular_and_plural() {
        assert_eq!(
            summary_lines(1, 0, &[], false, "staging"),
            ["Saved 1 request file (0 unchanged), environment 'staging'"]
        );
        let secrets = ["auth_token".to_owned(), "api_key".to_owned()];
        assert_eq!(
            summary_lines(3, 2, &secrets, true, "recorded"),
            [
                "Saved 3 request files (2 unchanged), environment 'recorded'",
                "Secrets parameterized and stored in the secret store: auth_token, api_key",
            ]
        );
        assert_eq!(
            summary_lines(0, 1, &[], false, "recorded")[0],
            "Saved 0 request files (1 unchanged), environment 'recorded'"
        );
    }
}
