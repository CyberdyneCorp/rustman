//! `rustman secret get|test|list-providers` (secret-providers spec "Secret
//! CLI commands"): provider secrets fetched, checked or listed, and never
//! printed unless `--reveal` is given on a terminal.

use std::io::{self, IsTerminal};
use std::path::PathBuf;

use clap::Args;
use rustman_core::secrets::provider::{ProviderError, SecretProviders};
use rustman_core::vars::ResolveError;
use rustman_core::{ConfigError, Workspace};
use rustman_secrets::describe;

use crate::exit::{CliError, SUCCESS};

#[derive(Debug, Args)]
pub struct GetArgs {
    /// The reference: <provider>/<path>[#field]
    reference: String,
    /// Print the value itself (only when stdout is a terminal)
    #[arg(long)]
    reveal: bool,
    #[command(flatten)]
    workspace: WorkspaceArg,
}

#[derive(Debug, Args)]
pub struct TestArgs {
    /// The provider to check (default: every configured provider)
    provider: Option<String>,
    #[command(flatten)]
    workspace: WorkspaceArg,
}

#[derive(Debug, Args)]
pub struct ListProvidersArgs {
    /// List the provider types this build supports instead
    #[arg(long)]
    types: bool,
    #[command(flatten)]
    workspace: WorkspaceArg,
}

#[derive(Debug, Args)]
struct WorkspaceArg {
    /// Workspace directory (default: search from the current directory upwards)
    #[arg(long, value_name = "DIR")]
    workspace: Option<PathBuf>,
}

impl WorkspaceArg {
    /// The providers of the workspace and the user config.
    fn providers(&self) -> Result<SecretProviders, ConfigError> {
        let cwd = std::env::current_dir().map_err(|e| ConfigError::Io {
            path: PathBuf::from("."),
            source: e,
        })?;
        let ws = Workspace::discover(&cwd, self.workspace.as_deref())?;
        super::print_warnings(&ws.warnings);
        rustman_secrets::load(&ws.root, &ws.settings)
    }
}

/// `secret get`: the masked value, or with `--reveal` on a terminal the
/// value itself.
pub fn get(args: &GetArgs) -> Result<u8, CliError> {
    let reference = describe::reference(&args.reference).map_err(ConfigError::Usage)?;
    if args.reveal && !io::stdout().is_terminal() {
        return Err(ConfigError::Usage("--reveal requires an interactive terminal".into()).into());
    }
    let providers = args.workspace.providers()?;
    // Whatever the cause (a type compiled out included), a reference that
    // cannot be resolved is a request error, exit code 3, as in `send`.
    let fetched = super::exec::runtime()?.block_on(providers.fetch(&reference));
    let secret = fetched.map_err(ResolveError::from)?;
    if args.reveal {
        println!("{}", secret.expose());
    } else {
        println!("{}", describe::masked(secret.expose(), &reference.provider));
    }
    Ok(SUCCESS)
}

/// `secret test`: one line per provider; exit code 3 when a check failed,
/// whatever the cause (a configuration problem or an unknown provider
/// included): the spec's "exits 0 or 3".
pub fn test(args: &TestArgs) -> Result<u8, CliError> {
    let providers = args.workspace.providers()?;
    let names = match &args.provider {
        Some(name) if !providers.names().contains(name) => {
            let unknown = ProviderError::UnknownProvider {
                name: name.clone(),
                configured: providers.names(),
            };
            eprintln!("error: {unknown}");
            return Ok(crate::exit::REQUEST);
        }
        Some(name) => vec![name.clone()],
        None => providers.names(),
    };
    if names.is_empty() {
        eprintln!("no secret providers configured");
    }
    let runtime = super::exec::runtime()?;
    let mut code = SUCCESS;
    for name in names {
        match runtime.block_on(providers.check(&name)) {
            Ok(()) => println!("{name}: ok"),
            Err(e) => {
                eprintln!("error: {}", describe::failure(&name, &e));
                code = crate::exit::REQUEST;
            }
        }
    }
    Ok(code)
}

/// `secret list-providers`: name, type, auth, origin and status per
/// provider; `--types`: every type this build supports.
pub fn list_providers(args: &ListProvidersArgs) -> Result<u8, CliError> {
    if args.types {
        for kind in rustman_secrets::built_types() {
            println!("{kind}");
        }
        return Ok(SUCCESS);
    }
    let providers = args.workspace.providers()?;
    if providers.is_empty() {
        eprintln!("no secret providers configured");
        return Ok(SUCCESS);
    }
    let header = ["NAME", "TYPE", "AUTH", "ORIGIN", "STATUS"].map(String::from);
    let rows: Vec<[String; 5]> = std::iter::once(header)
        .chain(providers.infos().into_iter().map(describe::row))
        .collect();
    for line in columns(&rows) {
        println!("{line}");
    }
    Ok(SUCCESS)
}

/// `rows` as left-aligned columns two spaces apart.
fn columns(rows: &[[String; 5]]) -> Vec<String> {
    let mut widths = [0; 5];
    for row in rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    rows.iter()
        .map(|row| {
            let cells = row.iter().zip(widths).map(|(c, w)| format!("{c:<w$}"));
            cells.collect::<Vec<_>>().join("  ").trim_end().to_owned()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::columns;

    #[test]
    fn columns_are_aligned() {
        let rows = [
            ["NAME", "TYPE", "AUTH", "ORIGIN", "STATUS"].map(String::from),
            ["vault", "vault", "oidc", "workspace", "available"].map(String::from),
        ];
        assert_eq!(
            columns(&rows),
            [
                "NAME   TYPE   AUTH  ORIGIN     STATUS",
                "vault  vault  oidc  workspace  available"
            ]
        );
    }
}
