//! The host imports. Each checks the plugin's grants when it is called;
//! a call without its grant returns `capability-denied` and has no side
//! effect.

use std::time::{Instant, UNIX_EPOCH};

use wasmtime::component::Resource;

use super::fs::{FsRefusal, resolve};
use super::secrets::SecretHandle;
use super::{HostState, LogLevel, PluginLog};
use crate::bindings::host::rustman::plugin::{
    clock, env, fs_read, http, log, random, secrets, types,
};
use crate::grant::Grant;
use types::HostError;

/// The most bytes one `random.bytes` call returns.
pub const MAX_RANDOM_BYTES: u32 = 64 * 1024;

/// The most log lines kept per instance; later lines are dropped.
pub const MAX_LOG_LINES: usize = 1000;

/// The longest log line kept, in bytes; longer lines are cut.
pub const MAX_LOG_LINE_BYTES: usize = 8 * 1024;

/// The most refused calls kept per instance; later ones are still
/// refused, but not recorded.
pub const MAX_DENIALS: usize = 100;

/// The longest guest-supplied detail (an env, secret or host name) kept
/// in a refusal, in bytes.
pub const MAX_DETAIL_BYTES: usize = 256;

/// The longest secret name a plugin can look up, in bytes.
pub const MAX_SECRET_NAME_BYTES: usize = 256;

/// The most resources (secret handles) one instance may hold at once.
pub const MAX_RESOURCES: usize = 1024;

/// Cut `text` to at most `max` bytes, on a character boundary.
pub(crate) fn truncate(text: &mut String, max: usize) {
    if text.len() > max {
        let cut = (0..=max)
            .rev()
            .find(|i| text.is_char_boundary(*i))
            .unwrap_or(0);
        text.truncate(cut);
    }
}

impl types::Host for HostState {}

impl clock::Host for HostState {
    fn now(&mut self) -> Result<clock::Datetime, HostError> {
        if !self.has(&Grant::Clock) {
            return Err(self.deny("clock".to_owned()));
        }
        let since = self
            .context
            .services
            .now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Ok(clock::Datetime {
            seconds: since.as_secs(),
            nanoseconds: since.subsec_nanos(),
        })
    }
}

impl random::Host for HostState {
    fn bytes(&mut self, len: u32) -> Result<Vec<u8>, HostError> {
        if !self.has(&Grant::Random) {
            return Err(self.deny("random".to_owned()));
        }
        if len > MAX_RANDOM_BYTES {
            let message = format!("at most {MAX_RANDOM_BYTES} random bytes per call");
            return Err(HostError::Failed(message));
        }
        let mut bytes = vec![0; len as usize];
        self.context.services.fill_random(&mut bytes);
        Ok(bytes)
    }
}

impl env::Host for HostState {
    fn get(&mut self, name: String) -> Result<Option<String>, HostError> {
        if !self.has(&Grant::Env(name.clone())) {
            return Err(self.deny(format!("env {name}")));
        }
        Ok(self.context.services.env(&name))
    }
}

impl HostState {
    /// The file or folder `path` names under an `fs:read` grant, timed as
    /// host time.
    fn fs_path(&mut self, path: &str) -> Result<std::path::PathBuf, HostError> {
        let started = Instant::now();
        let resolved = resolve(&self.context.root, &self.grants, path);
        self.meter.add_host_time(started.elapsed());
        match resolved {
            Ok(path) => Ok(path),
            Err(FsRefusal::Denied) => Err(self.deny("fs".to_owned())),
            Err(FsRefusal::Failed(message)) => Err(HostError::Failed(message)),
        }
    }
}

impl fs_read::Host for HostState {
    fn read(&mut self, path: String) -> Result<Vec<u8>, HostError> {
        let full = self.fs_path(&path)?;
        let started = Instant::now();
        let read = read_capped(&full, self.limiter.limit() as u64);
        self.meter.add_host_time(started.elapsed());
        read
    }

    fn list_dir(&mut self, path: String) -> Result<Vec<String>, HostError> {
        let full = self.fs_path(&path)?;
        let entries = std::fs::read_dir(&full).map_err(|e| HostError::Failed(e.to_string()))?;
        let mut names: Vec<String> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| !name.eq_ignore_ascii_case(rustman_core::secrets::STATE_DIR))
            .collect();
        names.sort();
        Ok(names)
    }
}

/// The file at `path`, refused when larger than `limit` bytes (it could
/// not fit in the plugin's memory anyway). At most `limit + 1` bytes are
/// read, however the file grows meanwhile.
pub(crate) fn read_capped(path: &std::path::Path, limit: u64) -> Result<Vec<u8>, HostError> {
    use std::io::Read as _;
    let failed = |e: std::io::Error| HostError::Failed(e.to_string());
    let file = std::fs::File::open(path).map_err(failed)?;
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(failed)?;
    if bytes.len() as u64 > limit {
        return Err(HostError::Failed(format!("file larger than {limit} bytes")));
    }
    Ok(bytes)
}

impl secrets::HostSecretRef for HostState {
    fn name(&mut self, handle: Resource<SecretHandle>) -> String {
        self.table
            .get(&handle)
            .map(|h| h.name.clone())
            .unwrap_or_default()
    }

    fn reveal(&mut self, handle: Resource<SecretHandle>) -> Result<String, HostError> {
        let name = secrets::HostSecretRef::name(self, handle);
        if !self.has(&Grant::SecretsRead(name.clone())) {
            return Err(self.deny(format!("secrets {name}")));
        }
        self.context
            .secret(&name)
            .map(|secret| secret.expose().clone())
            .ok_or_else(|| HostError::Failed(format!("no secret '{name}'")))
    }

    fn drop(&mut self, handle: Resource<SecretHandle>) -> wasmtime::Result<()> {
        self.table.delete(handle)?;
        Ok(())
    }
}

impl secrets::Host for HostState {
    /// A handle for `name`. Without `secrets:read:<name>` every name gets
    /// one (and `reveal` refuses it), so an ungranted plugin cannot learn
    /// which secrets the session has. Names longer than
    /// [`MAX_SECRET_NAME_BYTES`] get none (no grant can name them), and
    /// the resource table holds at most [`MAX_RESOURCES`] handles.
    fn lookup(&mut self, name: String) -> Option<Resource<SecretHandle>> {
        if name.len() > MAX_SECRET_NAME_BYTES {
            return None;
        }
        if self.has(&Grant::SecretsRead(name.clone())) {
            self.context.secret(&name)?;
        }
        self.table.push(SecretHandle { name }).ok()
    }
}

impl log::Host for HostState {
    fn write(&mut self, level: log::Level, message: String) {
        if self.logs.len() >= MAX_LOG_LINES {
            return;
        }
        let mut message = self.redactor.text(&message).into_owned();
        truncate(&mut message, MAX_LOG_LINE_BYTES);
        let level = LogLevel::from(level);
        tracing::debug!(plugin = %self.plugin, ?level, "{message}");
        self.logs.push(PluginLog { level, message });
    }
}

impl http::Host for HostState {
    fn fetch(&mut self, request: http::Request) -> Result<http::Response, HostError> {
        let Some((host, port)) = host_and_port(&request.url) else {
            return Err(HostError::Failed(format!("invalid URL '{}'", request.url)));
        };
        let granted = self.grants.iter().any(|grant| match grant {
            Grant::Net(target) => target.allows(&host, port),
            _ => false,
        });
        if !granted {
            return Err(self.deny(format!("net {host}")));
        }
        let started = Instant::now();
        let response = self.context.services.fetch(&self.plugin, request);
        self.meter.add_host_time(started.elapsed());
        response
    }
}

/// The host and port of an `http` or `https` URL.
fn host_and_port(url: &str) -> Option<(String, u16)> {
    let parsed = url::Url::parse(url).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let host = match parsed.host()? {
        url::Host::Ipv6(ip) => ip.to_string(),
        other => other.to_string(),
    };
    Some((host, parsed.port_or_known_default()?))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::sandbox::HostContext;

    fn state(grants: &[&str]) -> HostState {
        let context = HostContext::new(".").with_secrets(vec![rustman_core::Secret::new(
            "set",
            "s3cr3t-value".to_owned(),
        )]);
        let grants: Vec<Grant> = grants.iter().map(|g| Grant::parse(g).unwrap()).collect();
        HostState::new(
            "p",
            grants.into(),
            crate::limits::Limits::default(),
            Arc::new(context),
        )
    }

    /// Without a grant, lookup answers the same for set and unset names.
    #[test]
    fn lookup_does_not_reveal_which_secrets_exist() {
        use secrets::Host as _;
        let mut ungranted = state(&[]);
        assert!(ungranted.lookup("set".into()).is_some());
        assert!(ungranted.lookup("unset".into()).is_some());
        let mut granted = state(&["secrets:read:set", "secrets:read:unset"]);
        assert!(granted.lookup("set".into()).is_some());
        assert!(granted.lookup("unset".into()).is_none());
    }

    /// A guest calling refused imports in a loop with huge names leaves
    /// bounded host memory: a capped count of short refusals.
    #[test]
    fn refusals_are_capped_in_count_and_length() {
        use env::Host as _;
        let mut state = state(&[]);
        let huge = "N".repeat(1 << 20);
        for _ in 0..(MAX_DENIALS * 5) {
            let error = state.get(huge.clone()).unwrap_err();
            let HostError::CapabilityDenied(detail) = error else {
                panic!("expected a refusal");
            };
            assert!(detail.len() <= MAX_DETAIL_BYTES);
        }
        assert_eq!(state.denials.len(), MAX_DENIALS);
        let longest = "capability_denied: ".len() + MAX_DETAIL_BYTES;
        assert!(state.denials.iter().all(|d| d.len() <= longest));
    }

    #[test]
    fn log_lines_are_capped_in_count_and_length() {
        use log::Host as _;
        let mut state = state(&[]);
        for _ in 0..(MAX_LOG_LINES + 10) {
            state.write(log::Level::Info, "é".repeat(MAX_LOG_LINE_BYTES));
        }
        assert_eq!(state.logs.len(), MAX_LOG_LINES);
        assert!(
            state
                .logs
                .iter()
                .all(|l| l.message.len() <= MAX_LOG_LINE_BYTES)
        );
    }

    /// Ungranted lookups cannot grow the host without bound: overlong
    /// names get no handle, and the handle table is capped.
    #[test]
    fn secret_lookups_are_bounded() {
        use secrets::Host as _;
        let mut ungranted = state(&[]);
        assert!(ungranted.lookup("n".repeat(1 << 20)).is_none());
        assert!(
            ungranted
                .lookup("n".repeat(MAX_SECRET_NAME_BYTES))
                .is_some()
        );
        let mut ungranted = state(&[]);
        let held = (0..MAX_RESOURCES * 2)
            .filter_map(|i| ungranted.lookup(format!("s{i}")))
            .count();
        assert_eq!(held, MAX_RESOURCES);
    }

    #[test]
    fn reads_are_capped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, "0123456789").unwrap();
        assert_eq!(read_capped(&path, 10).unwrap(), b"0123456789");
        assert_eq!(
            read_capped(&path, 9).unwrap_err(),
            HostError::Failed("file larger than 9 bytes".into())
        );
    }

    #[test]
    fn urls_give_their_host_and_port() {
        assert_eq!(
            host_and_port("https://Auth.Example.com/token"),
            Some(("auth.example.com".to_owned(), 443))
        );
        assert_eq!(
            host_and_port("http://localhost:8080/"),
            Some(("localhost".to_owned(), 8080))
        );
        assert_eq!(
            host_and_port("http://[::1]:9/"),
            Some(("::1".to_owned(), 9))
        );
        assert_eq!(host_and_port("file:///etc/passwd"), None);
        assert_eq!(host_and_port("not a url"), None);
    }
}
