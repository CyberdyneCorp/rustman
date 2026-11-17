use assert_cmd::Command;
use predicates::prelude::*;

fn rustman() -> Command {
    Command::cargo_bin("rustman").expect("rustman binary is built")
}

#[test]
fn version_prints_package_version() {
    rustman()
        .arg("--version")
        .assert()
        .success()
        .stdout(format!("rustman {}\n", env!("CARGO_PKG_VERSION")));
}

#[test]
fn help_describes_the_tool() {
    rustman()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage: rustman"))
        .stdout(predicate::str::contains("--version"));
}

#[test]
fn no_arguments_prints_help() {
    rustman()
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage: rustman"));
}

#[test]
fn unknown_flag_is_a_usage_error() {
    rustman().arg("--nope").assert().code(2);
}
