//! `rustman snapshot list|prune`: the snapshot files of a workspace and
//! whether a request still uses them (snapshot-testing "Snapshot list
//! command", "Prune obsolete snapshots"). Nothing is sent.

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use rustman_core::snapshot::{SnapshotEntry, inventory, prune};
use rustman_core::{ConfigError, Workspace};

use super::run::scopes;
use super::{current_dir, print_warnings};
use crate::exit::{CliError, SUCCESS};

#[derive(Debug, Args)]
pub struct SnapshotArgs {
    #[command(subcommand)]
    command: SnapshotCommand,
    /// Workspace directory (default: search from the current directory upwards)
    #[arg(long, value_name = "DIR", global = true)]
    workspace: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum SnapshotCommand {
    /// List the snapshot files with their request, name and state (ok,
    /// orphaned or unreferenced name)
    List(ListArgs),
    /// Delete snapshot files that no request uses anymore (with --yes or
    /// after confirmation on a terminal)
    Prune(PruneArgs),
}

#[derive(Debug, Args)]
struct ListArgs {
    /// A folder or request file to limit the listing to (default: the
    /// whole workspace)
    #[arg(value_name = "PATH")]
    path: Option<PathBuf>,
    /// Print a JSON array instead of one line per snapshot
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct PruneArgs {
    /// A folder or request file to limit pruning to (default: the whole
    /// workspace)
    #[arg(value_name = "PATH")]
    path: Option<PathBuf>,
    /// Only list what would be deleted
    #[arg(long)]
    dry_run: bool,
    /// Delete without asking
    #[arg(long, short = 'y')]
    yes: bool,
}

pub fn run(args: &SnapshotArgs) -> Result<u8, CliError> {
    let ws = super::plugins::discover(&current_dir()?, args.workspace.as_deref())?;
    print_warnings(&ws.warnings);
    match &args.command {
        SnapshotCommand::List(list) => {
            let entries = entries(&ws, list.path.as_deref())?;
            print_list(&entries, list.json);
        }
        SnapshotCommand::Prune(args) => {
            let entries = entries(&ws, args.path.as_deref())?;
            prune_entries(&ws, &entries, args)?;
        }
    }
    Ok(SUCCESS)
}

/// The snapshot files under `path` (relative to the current directory).
fn entries(ws: &Workspace, path: Option<&Path>) -> Result<Vec<SnapshotEntry>, ConfigError> {
    let scope = match path {
        Some(path) => scopes(ws, &current_dir()?, &[path.to_path_buf()])?.remove(0),
        None => PathBuf::new(),
    };
    inventory(ws, &scope).map_err(|source| ConfigError::Io {
        path: ws.root.join(&scope),
        source,
    })
}

fn print_list(entries: &[SnapshotEntry], json: bool) {
    let mut out = io::stdout().lock();
    if json {
        let text = serde_json::to_string_pretty(entries).expect("entries serialize");
        let _ = writeln!(out, "{text}");
        return;
    }
    if entries.is_empty() {
        let _ = writeln!(out, "no snapshots");
    }
    for entry in entries {
        let _ = writeln!(out, "{}", list_line(entry));
    }
}

/// `<state>  <snapshot path>  <request> <name>`, states padded to one width.
fn list_line(entry: &SnapshotEntry) -> String {
    format!(
        "{:<17}  {}  {} {}",
        entry.state.as_str(),
        entry.path,
        entry.request,
        entry.name
    )
}

/// What `prune` does with the obsolete snapshots it found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Delete,
    /// List them and ask on the terminal.
    Ask,
    /// List them only.
    List,
}

/// `--dry-run` never deletes, `--yes` always does, and otherwise a terminal
/// asks while anything else only lists (design D7).
fn action(dry_run: bool, yes: bool, interactive: bool) -> Action {
    match (dry_run, yes, interactive) {
        (true, _, _) => Action::List,
        (false, true, _) => Action::Delete,
        (false, false, true) => Action::Ask,
        (false, false, false) => Action::List,
    }
}

fn prune_entries(
    ws: &Workspace,
    entries: &[SnapshotEntry],
    args: &PruneArgs,
) -> Result<(), CliError> {
    let obsolete: Vec<SnapshotEntry> = entries
        .iter()
        .filter(|e| e.state.is_obsolete())
        .cloned()
        .collect();
    if obsolete.is_empty() {
        println!("no obsolete snapshots");
        return Ok(());
    }
    let interactive = io::stdin().is_terminal();
    let delete = match action(args.dry_run, args.yes, interactive) {
        Action::Delete => true,
        Action::Ask => {
            print_candidates(&obsolete);
            confirm(&obsolete, &mut io::stdin().lock())
        }
        Action::List => {
            print_candidates(&obsolete);
            if !args.dry_run {
                eprintln!("pass --yes to delete them");
            }
            false
        }
    };
    if delete {
        let deleted = prune(&obsolete).map_err(|source| ConfigError::Io {
            path: ws.root.clone(),
            source,
        })?;
        for entry in &deleted {
            println!("deleted {}", candidate(entry));
        }
    }
    Ok(())
}

fn print_candidates(obsolete: &[SnapshotEntry]) {
    for entry in obsolete {
        println!("obsolete {}", candidate(entry));
    }
}

/// `<snapshot path> (<state>)`.
fn candidate(entry: &SnapshotEntry) -> String {
    format!("{} ({})", entry.path, entry.state.as_str())
}

/// Ask on stderr whether to delete `obsolete`; only `y` or `yes` agrees.
fn confirm(obsolete: &[SnapshotEntry], input: &mut dyn BufRead) -> bool {
    let noun = if obsolete.len() == 1 { "file" } else { "files" };
    eprint!("delete {} snapshot {noun}? [y/N] ", obsolete.len());
    let _ = io::stderr().flush();
    let mut answer = String::new();
    if input.read_line(&mut answer).is_err() {
        return false;
    }
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustman_core::snapshot::EntryState;

    #[test]
    fn prune_deletes_only_when_told_or_confirmed() {
        assert_eq!(action(true, true, true), Action::List);
        assert_eq!(action(true, false, false), Action::List);
        assert_eq!(action(false, true, false), Action::Delete);
        assert_eq!(action(false, false, true), Action::Ask);
        assert_eq!(action(false, false, false), Action::List);
    }

    fn entry(state: EntryState) -> SnapshotEntry {
        SnapshotEntry {
            path: "users/__snapshots__/old.default.snap.yaml".into(),
            request: "users/old.rman.yaml".into(),
            name: "default".into(),
            state,
            file: PathBuf::new(),
            content: None,
        }
    }

    #[test]
    fn only_yes_confirms() {
        let entries = [entry(EntryState::Orphaned)];
        for (answer, expected) in [
            ("y\n", true),
            ("YES\n", true),
            ("n\n", false),
            ("\n", false),
        ] {
            assert_eq!(
                confirm(&entries, &mut answer.as_bytes()),
                expected,
                "{answer:?}"
            );
        }
        assert!(!confirm(&entries, &mut "".as_bytes()));
    }

    #[test]
    fn lines_name_the_state_path_request_and_name() {
        assert_eq!(
            list_line(&entry(EntryState::UnreferencedName)),
            "unreferenced name  users/__snapshots__/old.default.snap.yaml  users/old.rman.yaml default"
        );
        assert_eq!(
            list_line(&entry(EntryState::Ok)),
            "ok                 users/__snapshots__/old.default.snap.yaml  users/old.rman.yaml default"
        );
        assert_eq!(
            candidate(&entry(EntryState::Orphaned)),
            "users/__snapshots__/old.default.snap.yaml (orphaned)"
        );
    }
}
