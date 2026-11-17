//! `rustman completions <shell>`: print a completion script on stdout.

use std::io::Write;

use clap::Args;
use clap_complete::Shell;

#[derive(Debug, Args)]
pub struct CompletionsArgs {
    /// The shell to complete for
    #[arg(value_name = "SHELL")]
    shell: Shell,
}

pub fn run(args: &CompletionsArgs, command: &mut clap::Command) {
    let mut script = Vec::new();
    clap_complete::generate(args.shell, command, "rustman", &mut script);
    let mut out = std::io::stdout().lock();
    // A closed stdout (`| head`) is not an error worth reporting.
    let _ = out.write_all(&script).and_then(|()| out.flush());
}
