//! The workspace of a protocol command (`rustman rpc`, `rustman grpc`):
//! found when there is one, optional for ad-hoc targets.

use std::path::{Path, PathBuf};

use rustman_core::collection::RequestEntry;
use rustman_core::model::Request;
use rustman_core::settings::Settings;
use rustman_core::workspace::RequestRef;
use rustman_core::{ConfigError, Workspace};

use crate::commands::{EnvSelection, current_dir};

/// A request kind a protocol command works with.
pub struct Kind {
    /// As written in request files and messages: `grpc`, `jsonrpc`.
    pub name: &'static str,
    pub matches: fn(&Request) -> bool,
}

/// The workspace (if any) and what resolving needs from it.
pub struct Session {
    pub ws: Option<Workspace>,
    pub cwd: PathBuf,
    pub settings: Settings,
}

impl Session {
    /// The workspace, if any; outside one, only `ad_hoc` targets work.
    pub fn open(selection: &EnvSelection, ad_hoc: bool) -> Result<Self, ConfigError> {
        let cwd = current_dir()?;
        let explicit = selection.workspace.is_some() || selection.env.is_some();
        let ws = match selection.workspace() {
            Ok(ws) => Some(ws),
            Err(ConfigError::NoWorkspace) if !explicit && ad_hoc => None,
            Err(e) => return Err(e),
        };
        let settings = ws
            .as_ref()
            .map_or_else(Settings::ad_hoc, |w| w.settings.clone());
        Ok(Self { ws, cwd, settings })
    }

    pub fn root(&self) -> &Path {
        self.ws.as_ref().map_or(self.cwd.as_path(), |w| &w.root)
    }

    /// The request `target` (a file, id or name) of the workspace, which
    /// must be of `kind`.
    pub fn find(&self, target: &str, kind: &Kind) -> Result<RequestRef<'_>, ConfigError> {
        let ws = self.ws.as_ref().ok_or(ConfigError::NoWorkspace)?;
        let found = ws.find_request(target, &self.cwd)?;
        if !(kind.matches)(found.request()) {
            let article = if kind.name.starts_with(['a', 'e', 'i', 'o', 'u']) {
                "an"
            } else {
                "a"
            };
            let message = format!(
                "request '{}' is not {article} {} request",
                found.entry.id(),
                kind.name
            );
            return Err(ConfigError::Usage(message));
        }
        Ok(found)
    }

    /// `entry` as a request of this session.
    pub fn ad_hoc<'s>(&'s self, entry: &'s RequestEntry) -> RequestRef<'s> {
        RequestRef {
            root: self.root(),
            http: &self.settings.http,
            entry,
            ancestors: Vec::new(),
        }
    }
}
