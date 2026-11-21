//! What a plugin call can reach once a grant allows it: the session's
//! workspace and secrets, and the services behind the clock, randomness,
//! environment and network imports. The test harness swaps the services
//! for fakes, so plugin tests are deterministic.

use std::path::PathBuf;
use std::sync::{Arc, PoisonError, RwLock};
use std::time::SystemTime;

use rustman_core::{Redactor, Secret};

use crate::bindings::host::rustman::plugin::http::{Request, Response};
use crate::bindings::host::rustman::plugin::log::Level;
use crate::bindings::host::rustman::plugin::types::HostError;

/// The sources behind the host imports. Grants are checked before any of
/// these is called.
pub trait HostServices: Send + Sync {
    fn now(&self) -> SystemTime;
    fn fill_random(&self, bytes: &mut [u8]);
    fn env(&self, name: &str) -> Option<String>;
    /// Send a request the plugin's `net:` grants allow.
    fn fetch(&self, plugin: &str, request: Request) -> Result<Response, HostError>;
}

/// The real clock, the OS random source and the process environment.
/// Without a network, every fetch fails.
#[derive(Default)]
pub struct SystemServices {
    pub network: Option<super::net::GovernedNetwork>,
}

impl HostServices for SystemServices {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }

    fn fill_random(&self, bytes: &mut [u8]) {
        rand::fill(bytes);
    }

    fn env(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }

    fn fetch(&self, plugin: &str, request: Request) -> Result<Response, HostError> {
        match &self.network {
            Some(network) => network.fetch(plugin, request),
            None => Err(HostError::Failed(
                "this session has no network for plugins".to_owned(),
            )),
        }
    }
}

/// The session a plugin serves.
pub struct HostContext {
    /// The workspace root; `fs-read` resolves paths against it.
    pub root: PathBuf,
    /// Every secret of the session. A plugin sees one in clear only with
    /// `secrets:read:<name>`; everything it logs is redacted of all of them.
    pub secrets: Vec<Secret<String>>,
    /// Secrets the session's sends resolved after loading (provider
    /// values, captured secrets): redacted like `secrets`, granted by the
    /// same name.
    learned: RwLock<Vec<Secret<String>>>,
    pub services: Arc<dyn HostServices>,
}

/// The most learned secrets a session keeps; the oldest go first. A load
/// test capturing a new token every iteration must not grow without bound.
const MAX_LEARNED: usize = 1024;

impl std::fmt::Debug for HostContext {
    /// The root and the secret names, never a value.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<&str> = self.secrets.iter().map(Secret::name).collect();
        f.debug_struct("HostContext")
            .field("root", &self.root)
            .field("secrets", &names)
            .finish_non_exhaustive()
    }
}

impl HostContext {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            secrets: Vec::new(),
            learned: RwLock::default(),
            services: Arc::new(SystemServices::default()),
        }
    }

    pub fn with_secrets(mut self, secrets: Vec<Secret<String>>) -> Self {
        self.secrets = secrets;
        self
    }

    pub fn with_services(mut self, services: Arc<dyn HostServices>) -> Self {
        self.services = services;
        self
    }

    /// Make `secrets` secrets of the session from now on (see
    /// [`rustman_core::plugins::Extensions::learn_secrets`]).
    pub fn learn(&self, secrets: &[Secret<String>]) {
        let mut learned = self.learned.write().unwrap_or_else(PoisonError::into_inner);
        for secret in secrets {
            let known =
                |s: &Secret<String>| s.name() == secret.name() && s.expose() == secret.expose();
            if self.secrets.iter().any(known) || learned.iter().any(known) {
                continue;
            }
            if learned.len() == MAX_LEARNED {
                learned.remove(0);
            }
            learned.push(secret.clone());
        }
    }

    /// A redactor of every secret of the session.
    pub fn redactor(&self) -> Redactor {
        self.input_redactor(|_| false)
    }

    /// A redactor of the secrets `granted` does not name: what values
    /// handed to a plugin are redacted with, so a plugin without
    /// `secrets:read:<name>` sees `«redacted:<name>»`.
    pub fn input_redactor(&self, granted: impl Fn(&str) -> bool) -> Redactor {
        let mut redactor = Redactor::new();
        let learned = self.learned.read().unwrap_or_else(PoisonError::into_inner);
        for secret in self.secrets.iter().chain(learned.iter()) {
            if !granted(secret.name()) {
                redactor.add(secret);
            }
        }
        redactor
    }

    pub fn secret(&self, name: &str) -> Option<&Secret<String>> {
        self.secrets.iter().find(|s| s.name() == name)
    }
}

/// One line a plugin logged, already redacted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginLog {
    pub level: LogLevel,
    pub message: String,
}

/// The level of a [`PluginLog`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

impl From<Level> for LogLevel {
    fn from(level: Level) -> Self {
        match level {
            Level::Debug => Self::Debug,
            Level::Info => Self::Info,
            Level::Warn => Self::Warn,
            Level::Error => Self::Error,
        }
    }
}
