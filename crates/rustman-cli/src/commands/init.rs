//! `rustman init [dir] [--bare]`.

use std::path::PathBuf;

use clap::Args;
use rustman_core::ConfigError;

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Directory to initialize (created if missing)
    #[arg(default_value = ".")]
    dir: PathBuf,
    /// Only create rustman.toml and the .gitignore entry, without sample files
    #[arg(long)]
    bare: bool,
}

pub fn run(args: &InitArgs) -> Result<(), ConfigError> {
    let display = args.dir.display().to_string();
    let report = rustman_core::init::init(&args.dir, &display, args.bare)?;
    for path in &report.created {
        println!("created {}", path.display());
    }
    if report.gitignore_appended {
        println!("added .rustman/ to .gitignore");
    }
    println!("initialized rustman workspace in {display}");
    Ok(())
}
