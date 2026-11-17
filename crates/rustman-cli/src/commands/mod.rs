//! One module per subcommand.

pub mod env;
pub mod init;
pub mod secret;

/// Print non-fatal workspace problems on stderr.
pub fn print_warnings(warnings: &[String]) {
    for warning in warnings {
        eprintln!("warning: {warning}");
    }
}
