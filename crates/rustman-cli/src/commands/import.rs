//! `rustman import curl`: write a request file from a curl command.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use rustman_core::ConfigError;
use rustman_core::format::write::request_to_yaml;
use rustman_core::model::Request;
use rustman_core::workspace::check_new_request_file;
use rustman_http::{CurlError, import_curl, import_curl_words};

use super::print_warnings;
use crate::exit::CliError;

#[derive(Debug, Args)]
pub struct ImportArgs {
    #[command(subcommand)]
    command: ImportCommand,
}

#[derive(Debug, Subcommand)]
enum ImportCommand {
    /// Convert a curl command (an argument, or stdin) into a request file
    Curl(CurlArgs),
}

#[derive(Debug, Args)]
struct CurlArgs {
    /// Write the request file here instead of printing it (never overwrites)
    #[arg(long, value_name = "FILE")]
    out: Option<PathBuf>,
    /// The curl command: one quoted argument, or its words after `--`
    #[arg(
        value_name = "COMMAND",
        trailing_var_arg = true,
        allow_hyphen_values = true
    )]
    command: Vec<String>,
}

pub fn run(args: &ImportArgs) -> Result<(), CliError> {
    match &args.command {
        ImportCommand::Curl(a) => curl(a),
    }
}

fn curl(args: &CurlArgs) -> Result<(), CliError> {
    let imported = match args.command.as_slice() {
        [] => import_curl(&read_stdin()?)?,
        [command] => import_curl(command)?,
        words => import_curl_words(words)?,
    };
    print_warnings(&imported.warnings);
    let request = Request::Http(Box::new(imported.request));
    let yaml = request_to_yaml(&request);
    match &args.out {
        Some(path) => {
            check_new_request_file(path, &request)?;
            write_new(path, &yaml)?;
            println!("wrote {}", path.display());
        }
        None => print!("{yaml}"),
    }
    Ok(())
}

fn read_stdin() -> Result<String, CurlError> {
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .map_err(|e| CurlError(format!("cannot read the command from stdin: {e}")))?;
    Ok(text)
}

/// Create `path` with `text`, refusing to replace an existing file.
fn write_new(path: &Path, text: &str) -> Result<(), ConfigError> {
    let io = |source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    };
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(io)?;
    file.write_all(text.as_bytes()).map_err(io)
}
