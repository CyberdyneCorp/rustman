//! Installing the CA into a trust store, only on explicit commands (spec:
//! CA is never trusted automatically, CA rotation; design D3). Rustman
//! shells out to the platform tool, prints each command before running it,
//! and records which stores it touched in `<id>.installed`, so `uninstall`
//! and `rotate` undo exactly that. Firefox's NSS store is not managed.

use std::fmt;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use super::store::CaStore;
use super::{CaError, CaInfo, info_from_pem};

/// The warning `ca install` prints before asking for confirmation.
pub const INSTALL_WARNING: &str = "warning: trusting this CA allows decrypting HTTPS traffic \
     to the hosts you intercept on this machine; keep its key in the keychain and run \
     'rustman record ca uninstall' when you are done";

/// A program and its arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cmd {
    pub program: String,
    pub args: Vec<String>,
}

impl Cmd {
    fn new(program: &str, args: &[&str]) -> Self {
        Self {
            program: program.to_owned(),
            args: args.iter().map(|&a| a.to_owned()).collect(),
        }
    }
}

/// The command as a shell line, arguments with spaces quoted.
impl fmt::Display for Cmd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.program)?;
        for arg in &self.args {
            if arg.contains(' ') {
                write!(f, " \"{arg}\"")?;
            } else {
                write!(f, " {arg}")?;
            }
        }
        Ok(())
    }
}

/// Runs trust store commands; tests use a fake.
pub trait CommandRunner {
    /// Run `cmd` with the terminal attached; `Err` carries why it failed.
    fn run(&self, cmd: &Cmd) -> Result<(), String>;
    /// Whether `program` is on `PATH`.
    fn has_program(&self, program: &str) -> bool;
}

/// Runs commands for real.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemRunner;

impl CommandRunner for SystemRunner {
    fn run(&self, cmd: &Cmd) -> Result<(), String> {
        let status = Command::new(&cmd.program)
            .args(&cmd.args)
            .status()
            .map_err(|e| e.to_string())?;
        if status.success() {
            Ok(())
        } else {
            Err(status.to_string())
        }
    }

    fn has_program(&self, program: &str) -> bool {
        let Some(path) = std::env::var_os("PATH") else {
            return false;
        };
        let name = format!("{program}{}", std::env::consts::EXE_SUFFIX);
        std::env::split_paths(&path).any(|dir| dir.join(&name).is_file())
    }
}

/// A trust store Rustman knows how to change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrustStore {
    /// The macOS keychain search list, via `security`.
    Macos,
    /// The current user's Root store, via `certutil -user`.
    WindowsUser,
    /// `/usr/local/share/ca-certificates` and `update-ca-certificates`.
    DebianCaCertificates,
    /// p11-kit's `trust anchor`.
    P11Kit,
}

impl TrustStore {
    /// The store of operating system `os` (`std::env::consts::OS`).
    pub fn detect(os: &str, runner: &dyn CommandRunner) -> Option<Self> {
        match os {
            "macos" => Some(Self::Macos),
            "windows" => Some(Self::WindowsUser),
            "linux" if runner.has_program("update-ca-certificates") => {
                Some(Self::DebianCaCertificates)
            }
            "linux" if runner.has_program("trust") => Some(Self::P11Kit),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Macos => "the macOS keychain",
            Self::WindowsUser => "the Windows current-user Root store",
            Self::DebianCaCertificates => "the system CA certificates (update-ca-certificates)",
            Self::P11Kit => "the system trust anchors (p11-kit)",
        }
    }

    /// The commands that trust the certificate at `cert`.
    pub fn install_commands(self, cert: &Path, workspace_id: &str) -> Vec<Cmd> {
        let cert = cert.to_string_lossy();
        match self {
            Self::Macos => vec![Cmd::new(
                "security",
                &["add-trusted-cert", "-r", "trustRoot", "-p", "ssl", &cert],
            )],
            Self::WindowsUser => vec![Cmd::new("certutil", &["-user", "-addstore", "Root", &cert])],
            Self::DebianCaCertificates => vec![
                Cmd::new(
                    "sudo",
                    &["install", "-m", "644", &cert, &debian_path(workspace_id)],
                ),
                Cmd::new("sudo", &["update-ca-certificates"]),
            ],
            Self::P11Kit => vec![Cmd::new("sudo", &["trust", "anchor", "--store", &cert])],
        }
    }

    /// The commands that undo [`TrustStore::install_commands`] for the
    /// certificate at `cert` with SHA-1 fingerprint `sha1`.
    pub fn uninstall_commands(self, cert: &Path, sha1: &str, workspace_id: &str) -> Vec<Cmd> {
        let cert = cert.to_string_lossy();
        match self {
            Self::Macos => vec![
                Cmd::new("security", &["remove-trusted-cert", &cert]),
                Cmd::new("security", &["delete-certificate", "-Z", sha1]),
            ],
            Self::WindowsUser => vec![Cmd::new("certutil", &["-user", "-delstore", "Root", sha1])],
            Self::DebianCaCertificates => vec![
                Cmd::new("sudo", &["rm", "-f", &debian_path(workspace_id)]),
                Cmd::new("sudo", &["update-ca-certificates", "--fresh"]),
            ],
            Self::P11Kit => vec![Cmd::new("sudo", &["trust", "anchor", "--remove", &cert])],
        }
    }
}

fn debian_path(workspace_id: &str) -> String {
    format!("/usr/local/share/ca-certificates/rustman-record-{workspace_id}.crt")
}

/// One store the CA was installed in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Install {
    pub store: TrustStore,
    pub sha256: String,
    pub sha1: String,
}

/// The contents of `<id>.installed`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallRecord {
    pub installs: Vec<Install>,
}

impl InstallRecord {
    /// The record at `path`; none means empty.
    pub fn load(path: &Path) -> Result<Self, CaError> {
        match fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| CaError::Invalid(format!("{}: {e}", path.display()))),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(io_error(path, source)),
        }
    }

    /// Write the record, or delete it when empty.
    fn save(&self, path: &Path) -> Result<(), CaError> {
        if self.installs.is_empty() {
            return match fs::remove_file(path) {
                Err(e) if e.kind() != ErrorKind::NotFound => Err(io_error(path, e)),
                _ => Ok(()),
            };
        }
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| io_error(dir, e))?;
        }
        let text =
            serde_json::to_string_pretty(self).map_err(|e| CaError::Invalid(e.to_string()))?;
        fs::write(path, text).map_err(|e| io_error(path, e))
    }

    /// Whether Rustman installed the CA anywhere.
    pub fn is_installed(&self) -> bool {
        !self.installs.is_empty()
    }
}

fn io_error(path: &Path, source: std::io::Error) -> CaError {
    CaError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// What `ca install` will do, shown before confirmation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallPlan {
    pub info: CaInfo,
    pub store: TrustStore,
    pub cert: PathBuf,
    pub commands: Vec<Cmd>,
}

/// How an install ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallOutcome {
    Installed,
    /// The user did not confirm; no store was changed.
    Declined,
}

/// The install plan for the workspace's CA into `store`.
pub fn plan_install(ca: &CaStore, store: Option<TrustStore>) -> Result<InstallPlan, CaError> {
    let pem = ca.certificate()?.ok_or(CaError::NotFound)?;
    let cert = ca.cert_path();
    let store = store.ok_or_else(|| CaError::Unsupported(cert.clone()))?;
    Ok(InstallPlan {
        info: info_from_pem(&pem)?,
        commands: store.install_commands(&cert, ca.workspace_id()),
        store,
        cert,
    })
}

/// Run `plan` once `confirm` agrees, announcing each command through
/// `announce` first, and record the store.
pub fn install(
    ca: &CaStore,
    plan: &InstallPlan,
    runner: &dyn CommandRunner,
    confirm: impl FnOnce(&InstallPlan) -> bool,
    announce: &mut dyn FnMut(&Cmd),
) -> Result<InstallOutcome, CaError> {
    if !confirm(plan) {
        return Ok(InstallOutcome::Declined);
    }
    run_all(&plan.commands, runner, announce)?;
    let path = ca.installed_path();
    let mut record = InstallRecord::load(&path)?;
    let install = Install {
        store: plan.store,
        sha256: plan.info.sha256.clone(),
        sha1: plan.info.sha1.clone(),
    };
    if !record.installs.contains(&install) {
        record.installs.push(install);
    }
    record.save(&path)?;
    Ok(InstallOutcome::Installed)
}

/// The commands that undo every recorded install.
pub fn plan_uninstall(ca: &CaStore) -> Result<Vec<Cmd>, CaError> {
    let record = InstallRecord::load(&ca.installed_path())?;
    let cert = ca.cert_path();
    Ok(record
        .installs
        .iter()
        .flat_map(|i| {
            i.store
                .uninstall_commands(&cert, &i.sha1, ca.workspace_id())
        })
        .collect())
}

/// Undo every recorded install, store by store; the record keeps the
/// stores whose commands failed.
pub fn uninstall(
    ca: &CaStore,
    runner: &dyn CommandRunner,
    announce: &mut dyn FnMut(&Cmd),
) -> Result<usize, CaError> {
    let path = ca.installed_path();
    let mut record = InstallRecord::load(&path)?;
    let cert = ca.cert_path();
    let mut removed = 0;
    while let Some(install) = record.installs.first().cloned() {
        let commands = install
            .store
            .uninstall_commands(&cert, &install.sha1, ca.workspace_id());
        run_all(&commands, runner, announce)?;
        record.installs.remove(0);
        record.save(&path)?;
        removed += 1;
    }
    Ok(removed)
}

/// What `ca remove` did.
#[derive(Debug)]
pub struct Removal {
    pub old: CaInfo,
    /// How many trust stores the CA was removed from first.
    pub uninstalled: usize,
}

/// Delete the CA: undo its recorded installs, then delete its key from
/// the keychain, its certificate and its (now empty) install record. A
/// failed uninstall keeps everything, so it can be retried.
pub fn remove(
    ca: &CaStore,
    runner: &dyn CommandRunner,
    announce: &mut dyn FnMut(&Cmd),
) -> Result<Removal, CaError> {
    let pem = ca.certificate()?.ok_or(CaError::NotFound)?;
    let old = info_from_pem(&pem)?;
    let uninstalled = uninstall(ca, runner, announce)?;
    ca.remove()?;
    let record = ca.installed_path();
    if let Err(source) = std::fs::remove_file(&record)
        && source.kind() != std::io::ErrorKind::NotFound
    {
        return Err(CaError::Io {
            path: record,
            source,
        });
    }
    Ok(Removal { old, uninstalled })
}

/// What `ca rotate` did.
#[derive(Debug)]
pub struct Rotation {
    pub old: Option<CaInfo>,
    pub new: CaInfo,
    /// How many trust stores the old CA was removed from.
    pub uninstalled: usize,
}

/// Replace the CA: undo its recorded installs, delete its key and
/// certificate, and generate a new one (which is not installed).
pub fn rotate(
    ca: &CaStore,
    runner: &dyn CommandRunner,
    announce: &mut dyn FnMut(&Cmd),
) -> Result<Rotation, CaError> {
    let old = ca
        .certificate()?
        .map(|pem| info_from_pem(&pem))
        .transpose()?;
    let uninstalled = uninstall(ca, runner, announce)?;
    ca.remove()?;
    let new = ca.init()?.info()?;
    Ok(Rotation {
        old,
        new,
        uninstalled,
    })
}

fn run_all(
    commands: &[Cmd],
    runner: &dyn CommandRunner,
    announce: &mut dyn FnMut(&Cmd),
) -> Result<(), CaError> {
    for command in commands {
        announce(command);
        runner.run(command).map_err(|reason| CaError::Command {
            command: command.to_string(),
            reason,
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::ca::store::tests::{ID, MockKeychain, store};

    /// Records commands instead of running them.
    #[derive(Default)]
    struct FakeRunner {
        ran: RefCell<Vec<String>>,
        programs: Vec<&'static str>,
        fail: Option<&'static str>,
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, cmd: &Cmd) -> Result<(), String> {
            let line = cmd.to_string();
            self.ran.borrow_mut().push(line.clone());
            match self.fail {
                Some(word) if line.contains(word) => Err("exit status: 1".into()),
                _ => Ok(()),
            }
        }

        fn has_program(&self, program: &str) -> bool {
            self.programs.contains(&program)
        }
    }

    fn lines(cmds: &[Cmd]) -> Vec<String> {
        cmds.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn the_store_follows_the_os() {
        let none = FakeRunner::default();
        assert_eq!(TrustStore::detect("macos", &none), Some(TrustStore::Macos));
        assert_eq!(
            TrustStore::detect("windows", &none),
            Some(TrustStore::WindowsUser)
        );
        assert_eq!(TrustStore::detect("linux", &none), None);
        let debian = FakeRunner {
            programs: vec!["update-ca-certificates", "trust"],
            ..FakeRunner::default()
        };
        assert_eq!(
            TrustStore::detect("linux", &debian),
            Some(TrustStore::DebianCaCertificates)
        );
        let fedora = FakeRunner {
            programs: vec!["trust"],
            ..FakeRunner::default()
        };
        assert_eq!(
            TrustStore::detect("linux", &fedora),
            Some(TrustStore::P11Kit)
        );
        assert_eq!(TrustStore::detect("freebsd", &debian), None);
    }

    #[test]
    fn the_exact_commands_per_os() {
        let cert = Path::new("/c/ca.pem");
        let install = |s: TrustStore| lines(&s.install_commands(cert, "W"));
        let uninstall = |s: TrustStore| lines(&s.uninstall_commands(cert, "AB12", "W"));
        assert_eq!(
            install(TrustStore::Macos),
            ["security add-trusted-cert -r trustRoot -p ssl /c/ca.pem"]
        );
        assert_eq!(
            uninstall(TrustStore::Macos),
            [
                "security remove-trusted-cert /c/ca.pem",
                "security delete-certificate -Z AB12"
            ]
        );
        assert_eq!(
            install(TrustStore::WindowsUser),
            ["certutil -user -addstore Root /c/ca.pem"]
        );
        assert_eq!(
            uninstall(TrustStore::WindowsUser),
            ["certutil -user -delstore Root AB12"]
        );
        let debian = "/usr/local/share/ca-certificates/rustman-record-W.crt";
        assert_eq!(
            install(TrustStore::DebianCaCertificates),
            [
                format!("sudo install -m 644 /c/ca.pem {debian}"),
                "sudo update-ca-certificates".to_owned()
            ]
        );
        assert_eq!(
            uninstall(TrustStore::DebianCaCertificates),
            [
                format!("sudo rm -f {debian}"),
                "sudo update-ca-certificates --fresh".to_owned()
            ]
        );
        assert_eq!(
            install(TrustStore::P11Kit),
            ["sudo trust anchor --store /c/ca.pem"]
        );
        assert_eq!(
            uninstall(TrustStore::P11Kit),
            ["sudo trust anchor --remove /c/ca.pem"]
        );
    }

    #[test]
    fn paths_with_spaces_are_quoted() {
        let cmd = Cmd::new("certutil", &["-addstore", "C:\\My Files\\ca.pem"]);
        assert_eq!(
            cmd.to_string(),
            "certutil -addstore \"C:\\My Files\\ca.pem\""
        );
    }

    #[test]
    fn declining_leaves_the_stores_untouched() {
        let config = tempfile::tempdir().unwrap();
        let keychain = MockKeychain::default();
        let ca = store(&keychain, config.path());
        ca.init().unwrap();
        let plan = plan_install(&ca, Some(TrustStore::Macos)).unwrap();
        let runner = FakeRunner::default();
        let outcome = install(&ca, &plan, &runner, |_| false, &mut |_| {}).unwrap();
        assert_eq!(outcome, InstallOutcome::Declined);
        assert!(runner.ran.borrow().is_empty());
        assert!(!ca.installed_path().exists());
    }

    #[test]
    fn install_runs_the_plan_and_records_the_store() {
        let config = tempfile::tempdir().unwrap();
        let keychain = MockKeychain::default();
        let ca = store(&keychain, config.path());
        let info = ca.init().unwrap().info().unwrap();
        let plan = plan_install(&ca, Some(TrustStore::WindowsUser)).unwrap();
        assert_eq!(plan.info, info);
        let runner = FakeRunner::default();
        let mut announced = Vec::new();
        let outcome = install(&ca, &plan, &runner, |_| true, &mut |c| {
            announced.push(c.to_string());
        })
        .unwrap();
        assert_eq!(outcome, InstallOutcome::Installed);
        let cert = ca.cert_path().to_string_lossy().into_owned();
        let expected = [format!("certutil -user -addstore Root {cert}")];
        assert_eq!(*runner.ran.borrow(), expected);
        assert_eq!(announced, expected);
        let record = InstallRecord::load(&ca.installed_path()).unwrap();
        assert_eq!(
            record.installs,
            [Install {
                store: TrustStore::WindowsUser,
                sha256: info.sha256,
                sha1: info.sha1.clone(),
            }]
        );
        assert_eq!(
            lines(&plan_uninstall(&ca).unwrap()),
            [format!("certutil -user -delstore Root {}", info.sha1)]
        );
        uninstall(&ca, &runner, &mut |_| {}).unwrap();
        assert!(!ca.installed_path().exists());
    }

    #[test]
    fn a_failed_command_is_reported_and_not_recorded() {
        let config = tempfile::tempdir().unwrap();
        let keychain = MockKeychain::default();
        let ca = store(&keychain, config.path());
        ca.init().unwrap();
        let plan = plan_install(&ca, Some(TrustStore::DebianCaCertificates)).unwrap();
        let runner = FakeRunner {
            fail: Some("update-ca-certificates"),
            ..FakeRunner::default()
        };
        let err = install(&ca, &plan, &runner, |_| true, &mut |_| {}).unwrap_err();
        assert_eq!(
            err.to_string(),
            "record: `sudo update-ca-certificates` failed: exit status: 1"
        );
        assert!(
            !InstallRecord::load(&ca.installed_path())
                .unwrap()
                .is_installed()
        );
    }

    #[test]
    fn unsupported_systems_name_the_certificate() {
        let config = tempfile::tempdir().unwrap();
        let keychain = MockKeychain::default();
        let ca = store(&keychain, config.path());
        ca.init().unwrap();
        let err = plan_install(&ca, None).unwrap_err();
        assert!(matches!(err, CaError::Unsupported(_)), "{err}");
    }

    #[test]
    fn rotate_uninstalls_deletes_and_generates_a_new_ca() {
        let config = tempfile::tempdir().unwrap();
        let keychain = MockKeychain::default();
        let ca = store(&keychain, config.path());
        let old = ca.init().unwrap().info().unwrap();
        let plan = plan_install(&ca, Some(TrustStore::Macos)).unwrap();
        let runner = FakeRunner::default();
        install(&ca, &plan, &runner, |_| true, &mut |_| {}).unwrap();
        runner.ran.borrow_mut().clear();
        let old_key = keychain.entries.lock().unwrap()[&format!("record-ca/{ID}")].clone();

        let rotation = rotate(&ca, &runner, &mut |_| {}).unwrap();

        assert_eq!(rotation.old.as_ref(), Some(&old));
        assert_ne!(rotation.new.sha256, old.sha256);
        assert_eq!(rotation.uninstalled, 1);
        let cert = ca.cert_path().to_string_lossy().into_owned();
        assert_eq!(
            *runner.ran.borrow(),
            [
                format!("security remove-trusted-cert {cert}"),
                format!("security delete-certificate -Z {}", old.sha1)
            ]
        );
        let entries = keychain.entries.lock().unwrap();
        assert_eq!(entries.len(), 1);
        assert_ne!(entries[&format!("record-ca/{ID}")], old_key);
        assert!(
            !InstallRecord::load(&ca.installed_path())
                .unwrap()
                .is_installed()
        );
        let new_pem = ca.certificate().unwrap().unwrap();
        assert_eq!(info_from_pem(&new_pem).unwrap(), rotation.new);
    }

    #[test]
    fn remove_uninstalls_then_deletes_the_key_and_the_certificate() {
        let config = tempfile::tempdir().unwrap();
        let keychain = MockKeychain::default();
        let ca = store(&keychain, config.path());
        assert!(matches!(
            remove(&ca, &FakeRunner::default(), &mut |_| {}),
            Err(CaError::NotFound)
        ));
        let old = ca.init().unwrap().info().unwrap();
        let plan = plan_install(&ca, Some(TrustStore::WindowsUser)).unwrap();
        let runner = FakeRunner::default();
        install(&ca, &plan, &runner, |_| true, &mut |_| {}).unwrap();
        runner.ran.borrow_mut().clear();

        let removal = remove(&ca, &runner, &mut |_| {}).unwrap();

        assert_eq!(removal.old, old);
        assert_eq!(removal.uninstalled, 1);
        assert_eq!(
            *runner.ran.borrow(),
            [format!("certutil -user -delstore Root {}", old.sha1)]
        );
        assert!(keychain.entries.lock().unwrap().is_empty(), "key deleted");
        assert!(ca.certificate().unwrap().is_none());
        assert!(crate::ca::store::tests::files(config.path()).is_empty());
    }

    #[test]
    fn a_failed_uninstall_keeps_the_ca_to_remove() {
        let config = tempfile::tempdir().unwrap();
        let keychain = MockKeychain::default();
        let ca = store(&keychain, config.path());
        ca.init().unwrap();
        let plan = plan_install(&ca, Some(TrustStore::P11Kit)).unwrap();
        install(&ca, &plan, &FakeRunner::default(), |_| true, &mut |_| {}).unwrap();
        let failing = FakeRunner {
            fail: Some("--remove"),
            ..FakeRunner::default()
        };
        assert!(remove(&ca, &failing, &mut |_| {}).is_err());
        assert!(ca.load().unwrap().is_some(), "key and certificate kept");
    }

    #[test]
    fn a_failed_uninstall_keeps_the_old_ca() {
        let config = tempfile::tempdir().unwrap();
        let keychain = MockKeychain::default();
        let ca = store(&keychain, config.path());
        let old = ca.init().unwrap().info().unwrap();
        let plan = plan_install(&ca, Some(TrustStore::P11Kit)).unwrap();
        install(&ca, &plan, &FakeRunner::default(), |_| true, &mut |_| {}).unwrap();
        let failing = FakeRunner {
            fail: Some("--remove"),
            ..FakeRunner::default()
        };
        assert!(rotate(&ca, &failing, &mut |_| {}).is_err());
        let pem = ca.certificate().unwrap().unwrap();
        assert_eq!(info_from_pem(&pem).unwrap(), old);
        assert!(
            InstallRecord::load(&ca.installed_path())
                .unwrap()
                .is_installed()
        );
    }
}
