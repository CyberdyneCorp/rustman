use clap::{CommandFactory, Parser};

#[derive(Debug, Parser)]
#[command(name = "rustman", version = rustman_core::VERSION, about)]
struct Cli {}

fn main() {
    let _cli = Cli::parse();
    // Subcommands and the TUI arrive in later changes; until then, show help.
    Cli::command()
        .print_help()
        .expect("failed to write help to stdout");
    println!();
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
