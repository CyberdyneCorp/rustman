//! `rustman secret set|rm|list`, and `get|test|list-providers` for
//! provider secrets (in `secret_provider`).
//!
//! Values are never printed (`get --reveal` on a terminal aside). `set`
//! prompts without echo when stdin is a terminal, and otherwise reads the
//! value from stdin (for scripts and CI).

use std::io::{self, IsTerminal, Read};
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use rustman_core::environment::ENV_VAR;
use rustman_core::secrets::{BACKEND_VAR, SECRETS_FILE, StoredIn};
use rustman_core::{
    ConfigError, Environment, Secret, SecretBackend, SecretError, SecretStore, Workspace,
};

use crate::exit::{CliError, SUCCESS};

#[derive(Debug, Args)]
pub struct SecretArgs {
    #[command(subcommand)]
    command: SecretCommand,
}

#[derive(Debug, Subcommand)]
enum SecretCommand {
    /// Store a secret's value (prompted without echo, or read from stdin)
    Set(NameArgs),
    /// Delete a secret's stored value
    Rm(NameArgs),
    /// List declared secrets and whether a value is stored (values are never shown)
    List(Common),
    /// Fetch a secret reference from its provider and show it masked
    Get(super::secret_provider::GetArgs),
    /// Check that secret providers can authenticate and be reached
    Test(super::secret_provider::TestArgs),
    /// List the configured secret providers (never values)
    ListProviders(super::secret_provider::ListProvidersArgs),
}

#[derive(Debug, Args)]
struct NameArgs {
    /// Secret name, as listed under 'secrets' in the environment file
    name: String,
    #[command(flatten)]
    common: Common,
}

#[derive(Debug, Args)]
struct Common {
    /// Environment (default: RUSTMAN_ENV, then default_environment)
    #[arg(long, value_name = "NAME")]
    env: Option<String>,
    /// Workspace directory (default: search from the current directory upwards)
    #[arg(long, value_name = "DIR")]
    workspace: Option<PathBuf>,
}

pub fn run(args: &SecretArgs) -> Result<u8, CliError> {
    let done = |()| SUCCESS;
    match &args.command {
        SecretCommand::Set(a) => Ok(run_set(a).map(done)?),
        SecretCommand::Rm(a) => Ok(run_rm(a).map(done)?),
        SecretCommand::List(a) => Ok(run_list(a).map(done)?),
        SecretCommand::Get(a) => super::secret_provider::get(a),
        SecretCommand::Test(a) => super::secret_provider::test(a),
        SecretCommand::ListProviders(a) => super::secret_provider::list_providers(a),
    }
}

/// The loaded workspace and its secret store.
struct Context {
    ws: Workspace,
    store: SecretStore,
}

impl Context {
    fn open(common: &Common) -> Result<Self, ConfigError> {
        let cwd = std::env::current_dir().map_err(|e| ConfigError::Io {
            path: PathBuf::from("."),
            source: e,
        })?;
        let ws = Workspace::discover(&cwd, common.workspace.as_deref())?;
        super::print_warnings(&ws.warnings);
        let backend = SecretBackend::from_env_value(std::env::var(BACKEND_VAR).ok().as_deref())?;
        let store = SecretStore::open(&ws.root, backend);
        Ok(Self { ws, store })
    }

    fn active(&self, common: &Common) -> Result<Option<&Environment>, ConfigError> {
        let env_var = std::env::var(ENV_VAR).ok();
        self.ws
            .active_environment(common.env.as_deref(), env_var.as_deref())
    }

    fn required_env(&self, common: &Common) -> Result<&Environment, ConfigError> {
        self.active(common)?.ok_or(ConfigError::NoActiveEnvironment)
    }

    fn flush_warnings(&self) {
        super::print_warnings(&self.store.take_warnings());
    }
}

fn run_set(args: &NameArgs) -> Result<(), ConfigError> {
    let ctx = Context::open(&args.common)?;
    let env = ctx.required_env(&args.common)?;
    if !env.is_secret(&args.name) {
        return Err(SecretError::NotDeclared {
            env: env.name.clone(),
            name: args.name.clone(),
        }
        .into());
    }
    let value = read_value(&args.name, &env.name)?;
    if let Some(warning) = short_secret_warning(&args.name, &value) {
        super::print_warnings(&[warning]);
    }
    let stored = ctx.store.set(&env.name, &Secret::new(&args.name, value));
    ctx.flush_warnings();
    let place = match stored? {
        StoredIn::Keychain => "the OS keychain".to_owned(),
        StoredIn::File => SECRETS_FILE.to_owned(),
    };
    println!(
        "stored secret '{}' for environment '{}' in {place}",
        args.name, env.name
    );
    Ok(())
}

/// Secret values shorter than this many characters are set with a warning:
/// every occurrence is redacted, so a short one (`1234`) also replaces
/// unrelated text, such as a number in a report (`"duration_ms":1234`).
const SHORT_SECRET: usize = 8;

/// The warning for a non-empty value shorter than [`SHORT_SECRET`].
fn short_secret_warning(name: &str, value: &str) -> Option<String> {
    let short = !value.is_empty() && value.chars().count() < SHORT_SECRET;
    short.then(|| {
        format!(
            "secret '{name}' is shorter than {SHORT_SECRET} characters: \
             short secrets may also redact unrelated text"
        )
    })
}

fn run_rm(args: &NameArgs) -> Result<(), ConfigError> {
    let ctx = Context::open(&args.common)?;
    let env = ctx.required_env(&args.common)?;
    let removed = ctx.store.remove(&env.name, &args.name);
    ctx.flush_warnings();
    if removed? {
        println!(
            "removed secret '{}' from environment '{}'",
            args.name, env.name
        );
    } else {
        println!(
            "secret '{}' was not set in environment '{}'",
            args.name, env.name
        );
    }
    Ok(())
}

/// One line per declared secret: `<env>/<name>  set|not set`. With `--env`
/// (or `RUSTMAN_ENV`), only that environment; otherwise every environment.
fn run_list(args: &Common) -> Result<(), ConfigError> {
    let ctx = Context::open(args)?;
    let selected: Vec<&Environment> = match (
        &args.env,
        std::env::var(ENV_VAR).ok().filter(|v| !v.is_empty()),
    ) {
        (None, None) => ctx.ws.environments.iter().collect(),
        _ => ctx.active(args)?.into_iter().collect(),
    };
    let mut lines = Vec::new();
    for env in selected {
        for name in &env.secrets {
            let state = match ctx.store.get(&env.name, name)? {
                Some(_) => "set",
                None => "not set",
            };
            lines.push(format!("{}/{name}  {state}", env.name));
        }
    }
    ctx.flush_warnings();
    if lines.is_empty() {
        eprintln!("no secrets declared");
    }
    for line in lines {
        println!("{line}");
    }
    Ok(())
}

/// The secret value: a no-echo prompt on a terminal, else all of stdin
/// without its final line break.
fn read_value(name: &str, env: &str) -> Result<String, ConfigError> {
    let stdin_io = |e| ConfigError::Io {
        path: Path::new("<stdin>").to_path_buf(),
        source: e,
    };
    if io::stdin().is_terminal() {
        return rpassword::prompt_password(format!("value for {name} ({env}): ")).map_err(stdin_io);
    }
    let mut value = String::new();
    io::stdin().read_to_string(&mut value).map_err(stdin_io)?;
    Ok(strip_line_break(value))
}

fn strip_line_break(mut value: String) -> String {
    if value.ends_with('\n') {
        value.pop();
        if value.ends_with('\r') {
            value.pop();
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::strip_line_break;

    #[test]
    fn only_the_final_line_break_is_removed() {
        assert_eq!(strip_line_break("abc\n".into()), "abc");
        assert_eq!(strip_line_break("abc\r\n".into()), "abc");
        assert_eq!(strip_line_break("abc".into()), "abc");
        assert_eq!(strip_line_break("a\nb\n\n".into()), "a\nb\n");
        assert_eq!(strip_line_break(" x \n".into()), " x ");
    }
}
