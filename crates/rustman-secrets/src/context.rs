//! What providers read from the process: the workspace root, environment
//! variables and the browser. Tests replace them.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

type EnvFn = dyn Fn(&str) -> Option<String> + Send + Sync;
type BrowserFn = dyn Fn(&str) -> io::Result<()> + Send + Sync;

/// The environment providers are built in.
#[derive(Clone)]
pub struct Context {
    root: PathBuf,
    env: Arc<EnvFn>,
    browser: Arc<BrowserFn>,
}

impl Context {
    /// The workspace at `root`, with this process's environment and browser.
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            env: Arc::new(|name| std::env::var(name).ok()),
            browser: Arc::new(rustman_core::browser::open_browser),
        }
    }

    /// Read environment variables through `env` instead.
    #[must_use]
    pub fn with_env(
        mut self,
        env: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        self.env = Arc::new(env);
        self
    }

    /// Open login pages with `browser` instead.
    #[must_use]
    pub fn with_browser(
        mut self,
        browser: impl Fn(&str) -> io::Result<()> + Send + Sync + 'static,
    ) -> Self {
        self.browser = Arc::new(browser);
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The environment variable `name`, unless unset or empty.
    pub fn var(&self, name: &str) -> Option<String> {
        (self.env)(name).filter(|v| !v.is_empty())
    }

    pub fn open_browser(&self, url: &str) -> io::Result<()> {
        (self.browser)(url)
    }

    /// The user's home directory.
    pub fn home(&self) -> Option<PathBuf> {
        let name = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        self.var(name).map(PathBuf::from)
    }
}
