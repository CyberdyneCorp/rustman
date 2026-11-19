//! Stdio command trust (mcp-client spec "Stdio command trust", design D6).
//!
//! Spawning a stdio server runs a command the workspace chose, so it needs
//! this machine's approval, recorded in the workspace [`TrustStore`] under
//! `mcp/<name>`. The fingerprint is the SHA-256 of the command, args, env
//! (sorted by key) and cwd, where a secret substituted into any of them
//! counts only by its name: rotating a secret never asks again, while
//! changing an argument or an env value (`PATH`, `NODE_OPTIONS`...) or
//! adding a variable does. The store seals each fingerprint with this
//! machine's key, so an approval that came with the workspace matches
//! nothing. HTTP servers run nothing locally and need no approval.

use ring::digest::{SHA256, digest};
use rustman_core::Redactor;
use rustman_core::data_rows::secret_by_name;
use rustman_core::secret::placeholder;
use rustman_core::trust::{TrustState, TrustStore};
use rustman_core::vars::Resolved;
use serde_json::json;

use super::error::ClientError;
use super::resolve::{ResolvedServer, ResolvedStdio, ResolvedTransport};

/// Proof that a server may be connected to: its stdio command is approved,
/// or it is an HTTP server. Only this module creates one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Approved {
    /// The fingerprint approved; `None` for HTTP servers.
    pub(crate) fingerprint: Option<String>,
}

/// A stdio command waiting for the user's approval: what to show them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingApproval {
    pub server: String,
    /// The full command line, secrets redacted.
    pub command_line: String,
    /// The variables the definition sets in the child's environment, as
    /// `(name, value)`: substituted secrets, and the whole value of a
    /// variable named like a secret (`TOKEN`, `PASSWORD`...), redacted.
    pub env: Vec<(String, String)>,
    /// The working directory.
    pub cwd: String,
    /// Approved before with another definition (rather than never).
    pub changed: bool,
    fingerprint: String,
}

/// The outcome of [`check_trust`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrustCheck {
    Approved(Approved),
    NeedsApproval(PendingApproval),
}

/// The trust subject of the server named `name`.
pub fn subject(name: &str) -> String {
    format!("mcp/{name}")
}

/// Whether `server` may be connected to without asking.
pub fn check_trust(store: &TrustStore, server: &ResolvedServer) -> TrustCheck {
    let ResolvedTransport::Stdio(stdio) = &server.transport else {
        return TrustCheck::Approved(Approved { fingerprint: None });
    };
    let fingerprint = fingerprint(stdio);
    let state = store.state(&subject(&server.name), &fingerprint);
    if state == TrustState::Trusted {
        return TrustCheck::Approved(Approved {
            fingerprint: Some(fingerprint),
        });
    }
    let redactor = server.redactor();
    TrustCheck::NeedsApproval(PendingApproval {
        server: server.name.clone(),
        command_line: command_line(stdio, &redactor),
        env: shown_env(stdio, &redactor),
        cwd: redactor.text(&stdio.cwd.display().to_string()).into_owned(),
        changed: state == TrustState::Changed,
        fingerprint,
    })
}

/// The definition's env as the approval dialog shows it.
fn shown_env(stdio: &ResolvedStdio, redactor: &Redactor) -> Vec<(String, String)> {
    stdio
        .env
        .iter()
        .map(|(key, value)| {
            let shown = if secret_by_name(key) {
                placeholder(key)
            } else {
                redactor.resolved(value)
            };
            (key.clone(), shown)
        })
        .collect()
}

impl PendingApproval {
    /// Record the user's approval in `store` (saved to disk).
    pub fn approve(self, store: &mut TrustStore) -> Result<Approved, ClientError> {
        store.approve(&subject(&self.server), &self.fingerprint)?;
        store.save()?;
        Ok(Approved {
            fingerprint: Some(self.fingerprint),
        })
    }
}

/// The headless rule: an untrusted server is refused unless `trust` (the
/// `--trust` flag) is set, which approves and records it.
pub fn require_trust(
    store: &mut TrustStore,
    server: &ResolvedServer,
    trust: bool,
) -> Result<Approved, ClientError> {
    match check_trust(store, server) {
        TrustCheck::Approved(approved) => Ok(approved),
        TrustCheck::NeedsApproval(pending) if trust => pending.approve(store),
        TrustCheck::NeedsApproval(_) => Err(ClientError::Untrusted(server.name.clone())),
    }
}

/// The approval of an ad-hoc server whose command line the user typed for
/// this one invocation (`rustman mcp-client tools stdio:<command>`): it is
/// their own command, so nothing is asked and nothing is recorded.
pub fn approve_typed(server: &ResolvedServer) -> Approved {
    let fingerprint = match &server.transport {
        ResolvedTransport::Stdio(stdio) => Some(fingerprint(stdio)),
        ResolvedTransport::Http(_) => None,
    };
    Approved { fingerprint }
}

impl Approved {
    /// Whether this approval covers `server` as it is now.
    pub(crate) fn covers(&self, server: &ResolvedServer) -> bool {
        match &server.transport {
            ResolvedTransport::Stdio(stdio) => {
                self.fingerprint.as_deref() == Some(fingerprint(stdio).as_str())
            }
            ResolvedTransport::Http(_) => true,
        }
    }
}

/// The SHA-256 (hex) of what an approval covers.
pub fn fingerprint(stdio: &ResolvedStdio) -> String {
    let shown = Redactor::new();
    let text = |r: &Resolved| shown.resolved(r);
    let mut env: Vec<(&str, String)> = stdio
        .env
        .iter()
        .map(|(k, v)| (k.as_str(), text(v)))
        .collect();
    env.sort_unstable();
    let canonical = json!({
        "command": text(&stdio.command),
        "args": stdio.args.iter().map(text).collect::<Vec<_>>(),
        "env": env,
        "cwd": stdio.cwd_written.as_ref().map(text),
    });
    hex(digest(&SHA256, canonical.to_string().as_bytes()).as_ref())
}

/// The command line as shown to the user: each word quoted when it has
/// spaces or quotes, secrets redacted.
pub fn command_line(stdio: &ResolvedStdio, redactor: &Redactor) -> String {
    std::iter::once(&stdio.command)
        .chain(&stdio.args)
        .map(|word| quote(&redactor.resolved(word)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote(word: &str) -> String {
    let plain = !word.is_empty()
        && !word
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '\\'));
    if plain {
        return word.to_owned();
    }
    let escaped = word.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use rustman_core::Secret;
    use rustman_core::vars::SecretSpan;

    use rustman_core::trust::TrustKey;

    use super::*;

    fn secret(name: &str, value: &str) -> Resolved {
        Resolved {
            text: value.to_owned(),
            secret_spans: vec![SecretSpan {
                name: name.to_owned(),
                range: 0..value.len(),
            }],
        }
    }

    fn server(args: &[&str], env: &[(&str, Resolved)]) -> ResolvedServer {
        ResolvedServer {
            name: "files".into(),
            transport: ResolvedTransport::Stdio(ResolvedStdio {
                command: Resolved::literal("npx"),
                args: args.iter().map(|a| Resolved::literal(*a)).collect(),
                env: env
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), v.clone()))
                    .collect(),
                cwd: PathBuf::from("/w"),
                cwd_written: None,
            }),
            roots: None,
            workspace: PathBuf::from("."),
            timeout: None,
            secrets: env
                .iter()
                .filter(|(_, v)| !v.secret_spans.is_empty())
                .map(|(k, v)| Secret::new((*k).to_owned(), v.text.clone()))
                .collect(),
        }
    }

    /// The workspace folder, its store, and the folder of the key that
    /// seals it (never the user's own).
    struct Dirs {
        dir: tempfile::TempDir,
        keys: tempfile::TempDir,
    }

    impl Dirs {
        fn path(&self) -> &std::path::Path {
            self.dir.path()
        }

        fn reload(&self) -> TrustStore {
            TrustStore::load_with_key(self.path(), TrustKey::in_dir(self.keys.path())).unwrap()
        }
    }

    fn new_store() -> (Dirs, TrustStore) {
        let dirs = Dirs {
            dir: tempfile::tempdir().unwrap(),
            keys: tempfile::tempdir().unwrap(),
        };
        let store = dirs.reload();
        (dirs, store)
    }

    #[test]
    fn first_spawn_needs_approval_showing_the_command_line() {
        let (_dir, store) = new_store();
        let files = server(
            &["-y", "@modelcontextprotocol/server-filesystem", "my dir"],
            &[],
        );
        let TrustCheck::NeedsApproval(pending) = check_trust(&store, &files) else {
            panic!("a new definition must not be trusted");
        };
        assert_eq!(
            pending.command_line,
            "npx -y @modelcontextprotocol/server-filesystem \"my dir\""
        );
        assert!(!pending.changed);
    }

    #[test]
    fn approval_is_recorded_and_reused() {
        let (dir, mut store) = new_store();
        let files = server(&["a"], &[]);
        let TrustCheck::NeedsApproval(pending) = check_trust(&store, &files) else {
            panic!("untrusted");
        };
        let approved = pending.approve(&mut store).unwrap();
        assert!(approved.covers(&files));
        let reloaded = dir.reload();
        assert!(matches!(
            check_trust(&reloaded, &files),
            TrustCheck::Approved(_)
        ));
    }

    #[test]
    fn changed_args_need_approval_again() {
        let (_dir, mut store) = new_store();
        require_trust(&mut store, &server(&["a"], &[]), true).unwrap();
        let changed = server(&["b"], &[]);
        let TrustCheck::NeedsApproval(pending) = check_trust(&store, &changed) else {
            panic!("changed args must not be trusted");
        };
        assert!(pending.changed);
        let err = require_trust(&mut store, &changed, false).unwrap_err();
        assert_eq!(
            err.to_string(),
            "mcp: server files is not trusted; review it or pass --trust"
        );
    }

    #[test]
    fn rotating_a_secret_env_value_keeps_the_approval() {
        let (_dir, mut store) = new_store();
        let before = server(
            &["a"],
            &[
                ("TOKEN", secret("token", "old")),
                ("MODE", Resolved::literal("x")),
            ],
        );
        require_trust(&mut store, &before, true).unwrap();
        let after = server(
            &["a"],
            &[
                ("MODE", Resolved::literal("x")),
                ("TOKEN", secret("token", "new")),
            ],
        );
        assert!(matches!(
            check_trust(&store, &after),
            TrustCheck::Approved(_)
        ));
        let added = server(
            &["a"],
            &[
                ("TOKEN", secret("token", "new")),
                ("MODE", Resolved::literal("x")),
                ("EXTRA", Resolved::literal("1")),
            ],
        );
        assert!(matches!(
            check_trust(&store, &added),
            TrustCheck::NeedsApproval(_)
        ));
    }

    #[test]
    fn changing_a_literal_env_value_needs_approval_again() {
        let (_dir, mut store) = new_store();
        let with = |x: &str| server(&["-c", "eval \"$X\""], &[("X", Resolved::literal(x))]);
        require_trust(&mut store, &with("exec fixture"), true).unwrap();
        let changed = with("touch PWNED; exec fixture");
        let TrustCheck::NeedsApproval(pending) = check_trust(&store, &changed) else {
            panic!("a changed env value must not be trusted");
        };
        assert!(pending.changed);
        assert!(require_trust(&mut store, &changed, false).is_err());
    }

    #[test]
    fn rotating_a_secret_in_an_argument_keeps_the_approval() {
        let (_dir, mut store) = new_store();
        let with = |value: &str| {
            let mut s = server(&[], &[]);
            if let ResolvedTransport::Stdio(stdio) = &mut s.transport {
                stdio.args.push(secret("token", value));
            }
            s
        };
        require_trust(&mut store, &with("old"), true).unwrap();
        let rotated = with("new");
        let TrustCheck::Approved(_) = check_trust(&store, &rotated) else {
            panic!("a rotated secret must stay trusted");
        };
        let (_other, fresh) = new_store();
        let TrustCheck::NeedsApproval(pending) = check_trust(&fresh, &rotated) else {
            panic!("untrusted");
        };
        assert!(
            !pending.command_line.contains("new"),
            "{}",
            pending.command_line
        );
    }

    #[test]
    fn trust_flag_approves_headless() {
        let (dir, mut store) = new_store();
        let files = server(&["a"], &[]);
        assert!(require_trust(&mut store, &files, false).is_err());
        require_trust(&mut store, &files, true).unwrap();
        let reloaded = dir.reload();
        assert!(require_trust(&mut reloaded.clone(), &files, false).is_ok());
    }

    #[test]
    fn the_dialog_shows_env_values_with_secrets_redacted() {
        let (_dir, store) = new_store();
        let files = server(
            &["a"],
            &[
                ("MODE", Resolved::literal("fast")),
                ("API_TOKEN", Resolved::literal("literal-token-value")),
                ("AUTH", secret("auth", "s3cr3t-value")),
            ],
        );
        let TrustCheck::NeedsApproval(pending) = check_trust(&store, &files) else {
            panic!("untrusted");
        };
        assert_eq!(
            pending.env,
            vec![
                ("MODE".to_owned(), "fast".to_owned()),
                ("API_TOKEN".to_owned(), "«redacted:API_TOKEN»".to_owned()),
                ("AUTH".to_owned(), "«redacted:auth»".to_owned()),
            ]
        );
    }

    /// An approval recorded under another machine's key (a trust file that
    /// came with the workspace) does not let the server spawn.
    #[test]
    fn an_approval_sealed_elsewhere_is_not_trusted() {
        let (elsewhere, mut theirs) = new_store();
        let files = server(&["a"], &[]);
        require_trust(&mut theirs, &files, true).unwrap();
        let (here, mut ours) = new_store();
        // This machine has approved something already: it has a key.
        require_trust(&mut ours, &server(&["other"], &[]), true).unwrap();
        std::fs::copy(
            elsewhere.path().join(rustman_core::trust::TRUST_FILE),
            here.path().join(rustman_core::trust::TRUST_FILE),
        )
        .unwrap();
        let mut copied = here.reload();
        assert!(matches!(
            check_trust(&copied, &files),
            TrustCheck::NeedsApproval(_)
        ));
        assert!(require_trust(&mut copied, &files, false).is_err());
        assert!(matches!(
            check_trust(&elsewhere.reload(), &files),
            TrustCheck::Approved(_)
        ));
    }
}
