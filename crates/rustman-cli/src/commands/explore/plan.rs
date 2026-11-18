//! `rustman explore plan`: discover the resource graph and plan probes
//! against a target without sending anything to it.

use clap::Args;
use rustman_core::safety::Limits;
use rustman_core::settings::Settings as WorkspaceSettings;
use rustman_explore::explore::graph::Graph;
use rustman_explore::explore::plan::{self, Plan};
use rustman_explore::explore::rules::Severity;
use rustman_explore::{ImportOptions, import};
use serde::Serialize;

use super::{ExploreFlags, identity_b, optional_workspace, plan_options, policy};
use crate::commands::{exec, print_warnings};
use crate::exit::CliError;

#[derive(Debug, Args)]
pub struct PlanArgs {
    #[command(flatten)]
    flags: ExploreFlags,
    /// Also print the resource graph (producers of each path parameter)
    #[arg(long)]
    graph: bool,
    /// Print the plan as JSON
    #[arg(long)]
    json: bool,
}

/// The JSON output of `explore plan`.
#[derive(Serialize)]
struct PlanOutput<'a> {
    #[serde(flatten)]
    plan: &'a Plan,
    #[serde(skip_serializing_if = "Option::is_none")]
    graph: Option<&'a Graph>,
}

pub fn plan_command(args: &PlanArgs) -> Result<(), CliError> {
    let flags = &args.flags;
    let workspace = optional_workspace(&flags.selection)?;
    let ws_settings = workspace
        .as_ref()
        .map_or_else(WorkspaceSettings::ad_hoc, |ws| ws.settings.clone());
    let environment = match &workspace {
        Some(ws) => flags.selection.environment(ws)?,
        None => None,
    };
    if let Some(name) = &flags.identity_b {
        identity_b(workspace.as_ref(), name)?;
    }
    let settings = flags.settings(Limits::from_settings(&ws_settings.safety), Severity::High);
    let policy = policy(&flags.target, &settings, &ws_settings, environment)?;
    let options = ImportOptions {
        allow_remote_refs: flags.allow_remote_refs,
        ..ImportOptions::default()
    };
    let imported = exec::runtime()?.block_on(import(&flags.spec, &options))?;
    let model = &imported.model;
    let graph = Graph::build(model, &ws_settings.explore.producers);
    print_warnings(&graph.warnings);
    let options = plan_options(
        &settings,
        &policy,
        &ws_settings,
        environment,
        flags.identity_b.is_some(),
    );
    let planned = plan::plan(model, &graph, &options);
    if args.json {
        let output = PlanOutput {
            plan: &planned,
            graph: args.graph.then_some(&graph),
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&output).unwrap_or_default()
        );
        return Ok(());
    }
    print!("{}", plan::render(&planned));
    if args.graph {
        print!("\ngraph:\n{}", graph.render());
    }
    Ok(())
}
