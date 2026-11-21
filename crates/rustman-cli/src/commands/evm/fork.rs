//! `rustman evm fork --chain <name|id|environment> [--block N] [--port P]`,
//! or `--env <environment>` / `--rpc <url>`: start `anvil --fork-url <rpc>`,
//! print the local URL (one JSON object with `--json`), and keep a
//! temporary `fork-<chain>` environment until Ctrl+C. The fork URL may hold
//! an API key: it is never printed.

use clap::Args;
use rustman_core::ConfigError;
use rustman_evm::EvmError;
use rustman_evm::chain::ChainRegistry;
use rustman_evm::fork::{
    Anvil, AnvilOptions, DEFAULT_PORT, TempEnvironment, find_anvil, fork_environment_name,
    fork_source, remove_stale_environments,
};
use rustman_evm::settings::EvmSettings;
use serde_json::json;

use super::{DONE, Endpoint, Node, with_node};
use crate::exit::CliError;

#[derive(Debug, Args)]
pub struct ForkArgs {
    /// The chain to fork: an environment name, a chain name or a chain id
    /// (else --env or --rpc names the endpoint)
    #[arg(long, value_name = "CHAIN", conflicts_with_all = ["env", "rpc"])]
    chain: Option<String>,
    /// Fork at this block number (default: the latest)
    #[arg(long, value_name = "N")]
    block: Option<u64>,
    /// The local port (default 8545)
    #[arg(long, value_name = "PORT", default_value_t = DEFAULT_PORT)]
    port: u16,
    #[command(flatten)]
    endpoint: Endpoint,
}

pub fn run(args: &ForkArgs) -> Result<u8, CliError> {
    let program = match find_anvil(std::env::var_os("PATH").as_deref()) {
        Ok(program) => program,
        Err(error) => {
            eprintln!("error: {error}");
            return Ok(error.exit_code());
        }
    };
    let (endpoint, label) = source(args)?;
    with_node(&endpoint, async |node| {
        let chain = &node.connected.identity.chain;
        let name = fork_environment_name(label.as_deref().unwrap_or(&chain.name), chain.id);
        let mut options = AnvilOptions::new(program.clone());
        options.port = args.port;
        options.fork_url = Some(node.rpc.request().url.text.clone());
        options.fork_block = args.block;
        for stale in remove_stale_environments(node.root) {
            node.note(&format!("removed '{stale}': its anvil no longer runs"));
        }
        let mut anvil = Anvil::start(&options).await?;
        let env = TempEnvironment::write(node.root, &name, &anvil.url(), chain.id, anvil.pid())?;
        announce(node, &anvil, env.name(), args.block);
        let exited = tokio::select! {
            () = node.cancel.cancelled() => None,
            status = anvil.wait_exit() => Some(status),
        };
        drop(env);
        anvil.stop().await;
        match exited {
            None => DONE,
            Some(status) => Err(EvmError::Rpc(format!(
                "anvil exited ({status}); the fork and environment '{name}' are gone"
            ))),
        }
    })
}

/// The endpoint to fork and the name its environment is made from: the
/// environment `--chain` names (or the one pinned to that chain), else
/// `--env`/`--rpc` as given.
fn source(args: &ForkArgs) -> Result<(Endpoint, Option<String>), CliError> {
    let flags = &args.endpoint;
    let Some(chain) = &args.chain else {
        if flags.selection.env.is_none() && flags.rpc.is_none() {
            let message = "give --chain, --env or --rpc";
            return Err(ConfigError::Usage(message.to_owned()).into());
        }
        return Ok((flags.clone(), flags.selection.env.clone()));
    };
    let ws = flags.selection.workspace()?;
    let settings = EvmSettings::from_table(&ws.settings.evm).map_err(config)?;
    let registry = ChainRegistry::with_overrides(&settings).map_err(config)?;
    let found = fork_source(&ws.environments, &registry, chain).map_err(config)?;
    let mut endpoint = flags.clone();
    endpoint.selection.env = Some(found.name.clone());
    endpoint.selection.workspace = Some(ws.root.clone());
    Ok((endpoint, Some(chain.clone())))
}

/// Print the fork's URL (or, with `--json`, its URLs, environment and
/// chain) and what it forks.
fn announce(node: &Node<'_>, anvil: &Anvil, environment: &str, block: Option<u64>) {
    let chain = &node.connected.identity.chain;
    if node.json {
        let value = json!({
            "url": anvil.url(),
            "ws_url": anvil.ws_url(),
            "environment": environment,
            "chain_id": chain.id,
            "fork_block": block,
        });
        println!("{value}");
    } else {
        println!("{}", anvil.url());
    }
    let at = block.map_or_else(|| "the latest block".to_owned(), |b| format!("block {b}"));
    node.note(&format!(
        "forking {} at {at}; environment '{environment}' is available until Ctrl+C",
        chain.label()
    ));
    node.note(ARGV_NOTE);
}

/// Anvil takes the fork URL only as an argument.
const ARGV_NOTE: &str = "the fork URL is passed to anvil as --fork-url: other local users can \
     read it (with any API key in it) in the process list while the fork runs";

fn config(error: EvmError) -> CliError {
    ConfigError::Usage(error.to_string()).into()
}
