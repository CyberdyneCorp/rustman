//! `rustman grpc health <target> [--service <name>]`: call
//! `grpc.health.v1.Health/Check` and print the serving status. Exit codes:
//! 0 for `SERVING`, 1 for `NOT_SERVING`, `SERVICE_UNKNOWN` or `UNKNOWN`,
//! 3 when the call fails (`UNIMPLEMENTED` included).

use std::time::Duration;

use clap::Args;
use rustman_grpc::{HealthCheck, ServingStatus, check_health, effective_deadline, remote};
use serde_json::json;

use super::{Reached, TargetFlags};
use crate::commands::exec::{parse_duration, runtime};
use crate::exit::{ASSERTION, CliError, REQUEST, SUCCESS};

#[derive(Debug, Args)]
pub struct HealthArgs {
    /// A target (host:port, grpc://host:port, grpcs://host:port or a
    /// {{variable}}), or a grpc request file, its id or its name
    #[arg(value_name = "TARGET|REQUEST")]
    target: String,
    /// The service to check (default: the whole server)
    #[arg(long, value_name = "NAME", default_value = "")]
    service: String,
    /// How long the check may take: 500ms, 5s, or a number of
    /// milliseconds (default: settings timeout_ms)
    #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
    timeout: Option<Duration>,
    /// Print the outcome as one JSON object
    #[arg(long)]
    json: bool,
    #[command(flatten)]
    flags: TargetFlags,
}

pub fn run(args: &HealthArgs) -> Result<u8, CliError> {
    let reached = Reached::open(&args.flags, &args.target)?;
    let remote = remote(&reached.resolved, &reached.options).map_err(|e| reached.redacted(e))?;
    let timeout = Some(reached.session.settings.defaults.timeout_ms);
    let deadline = args
        .timeout
        .unwrap_or_else(|| effective_deadline(None, timeout));
    let checked = runtime()?.block_on(check_health(&remote, &args.service, deadline));
    let checked = checked.map_err(|e| reached.redacted(e))?;
    let redactor = reached.resolved.redactor();
    match checked {
        HealthCheck::Status(status) => {
            if args.json {
                println!("{}", json!({"status": status.name()}));
            } else {
                println!("{status}");
            }
            Ok(exit_code(status))
        }
        HealthCheck::Failed(status) => {
            let shown = redactor.text(&status.render()).into_owned();
            if args.json {
                println!("{}", json!({"error": shown}));
            }
            eprintln!("error: grpc: health check failed: {shown}");
            Ok(REQUEST)
        }
    }
}

fn exit_code(status: ServingStatus) -> u8 {
    match status {
        ServingStatus::Serving => SUCCESS,
        _ => ASSERTION,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_serving_exits_0_every_other_status_exits_1() {
        assert_eq!(exit_code(ServingStatus::Serving), SUCCESS);
        for status in [
            ServingStatus::NotServing,
            ServingStatus::ServiceUnknown,
            ServingStatus::Unknown,
        ] {
            assert_eq!(exit_code(status), ASSERTION, "{status}");
        }
        assert_eq!(ASSERTION, 1);
    }
}
