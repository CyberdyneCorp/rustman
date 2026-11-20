//! The plugin author's commands: `rustman plugin new` scaffolds a project
//! from the template of an extension point, and `rustman plugin test` runs
//! its test cases with the host test harness.

use std::path::PathBuf;

use clap::Args;
use clap::builder::PossibleValuesParser;
use rustman_core::ConfigError;
use rustman_plugin::ExtensionPoint;
use rustman_plugin::harness::{self, CaseReport, Report};
use rustman_plugin::template::Sdk;

use super::{shown, usage};
use crate::exit::{ASSERTION, CliError, SUCCESS};

#[derive(Debug, Args)]
pub struct NewArgs {
    /// The plugin's name, kebab-case (my-auth)
    name: String,
    /// The extension point the project implements
    #[arg(long, value_name = "EXTENSION_POINT", value_parser = PossibleValuesParser::new(
        ExtensionPoint::ALL.map(ExtensionPoint::as_str)
    ))]
    kind: String,
    /// The project folder (default: ./<name>)
    #[arg(long, value_name = "DIR")]
    dir: Option<PathBuf>,
    /// Build against the rustman-plugin-sdk at this path (such as
    /// crates/rustman-plugin-sdk of a Rustman checkout) instead of the
    /// Rustman repository
    #[arg(long, value_name = "PATH")]
    sdk: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct TestArgs {
    /// A plugin folder (with rustman-plugin.toml) or a .wasm file
    #[arg(default_value = ".")]
    path: PathBuf,
    /// The test cases (default: rustman-plugin-tests.toml next to the
    /// manifest)
    #[arg(long, value_name = "FILE")]
    cases: Option<PathBuf>,
}

pub fn new(args: &NewArgs) -> Result<u8, CliError> {
    let point = ExtensionPoint::parse(&args.kind).expect("clap allows only extension points");
    let dir = args
        .dir
        .clone()
        .unwrap_or_else(|| PathBuf::from(&args.name));
    let sdk = match &args.sdk {
        Some(path) => Sdk::Path(std::path::absolute(path).map_err(|source| ConfigError::Io {
            path: path.clone(),
            source,
        })?),
        None => Sdk::Git,
    };
    let written =
        rustman_plugin::template::scaffold(&dir, &args.name, point, &sdk).map_err(usage)?;
    println!(
        "created the {} plugin {} in {}",
        point,
        args.name,
        shown(&dir)
    );
    for path in &written {
        println!("  {}", shown(path));
    }
    println!("build it with: cargo build --target wasm32-wasip2 --release");
    println!("then: rustman plugin test {}", shown(&dir));
    Ok(SUCCESS)
}

pub fn test(args: &TestArgs) -> Result<u8, CliError> {
    let report = harness::run(&args.path, args.cases.as_deref()).map_err(usage)?;
    print!("{}", render(&report));
    Ok(if report.passed() { SUCCESS } else { ASSERTION })
}

/// The report as `rustman plugin test` prints it: the same bytes for the
/// same plugin and cases on every run.
fn render(report: &Report) -> String {
    let mut out = format!("plugin {} {}\n", report.plugin, report.version);
    for case in &report.cases {
        render_case(&mut out, case);
    }
    let passed = report.cases.iter().filter(|c| c.passed()).count();
    let failed = report.cases.len() - passed;
    out.push_str(&format!("{passed} passed, {failed} failed\n"));
    out
}

fn render_case(out: &mut String, case: &CaseReport) {
    let status = if case.passed() { "ok  " } else { "FAIL" };
    out.push_str(&format!("{status} {}\n", case.name));
    for (name, value) in &case.output {
        out.push_str(&format!("       {name}: {}\n", printable(value)));
    }
    for line in &case.logs {
        out.push_str(&format!("       {}\n", printable(line)));
    }
    for problem in &case.problems {
        out.push_str(&format!("     ! {}\n", printable(problem)));
    }
}

/// `text` on one line: control characters escaped (`\n`, `\u{1b}`).
fn printable(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.extend(c.escape_unicode()),
            c => out.push(c),
        }
    }
    out
}
