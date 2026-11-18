//! Prompts (spec "Prompts"): `explore-api` walks an agent through plan,
//! review, run and report; `triage-findings` through grouping an
//! exploration's findings by root cause with reproduction steps.

use rmcp::ErrorData;
use rmcp::model::{GetPromptResult, JsonObject, Prompt, PromptArgument, PromptMessage, Role};

use super::redact::OutputRedactor;

pub const EXPLORE_API: &str = "explore-api";
pub const TRIAGE_FINDINGS: &str = "triage-findings";

fn argument(name: &str, description: &str) -> PromptArgument {
    PromptArgument::new(name)
        .with_description(description)
        .with_required(true)
}

/// Every prompt.
pub fn list() -> Vec<Prompt> {
    vec![
        Prompt::new(
            EXPLORE_API,
            Some("Explore an API from its OpenAPI document, safely: plan, review, run, report"),
            Some(vec![
                argument(
                    "spec",
                    "The OpenAPI document: a workspace file or an allowed URL",
                ),
                argument("target", "The base URL to probe"),
            ]),
        ),
        Prompt::new(
            TRIAGE_FINDINGS,
            Some("Group an exploration's findings by root cause, with reproduction steps"),
            Some(vec![argument(
                "exploration_id",
                "The exploration to triage (exp-...)",
            )]),
        ),
    ]
}

fn explore_api(spec: &str, target: &str) -> String {
    format!(
        "Explore the API described by `{spec}` against `{target}` with Rustman's tools, safely.\n\
         \n\
         1. Call `explore_plan` with spec `{spec}` and target `{target}`. It sends nothing. \
         Review the plan: which operations are probed, which probes are blocked or skipped and why, \
         and the seed.\n\
         2. Summarize the plan for the human. If probes are blocked because the host or the method \
         is not allowed, say so; do not try to work around the safety policy.\n\
         3. Only then call `explore_run` with the same spec, target and seed. It returns a `job_id` \
         and an `exploration_id` at once and runs in the background.\n\
         4. Poll `explore_status` with the `job_id` until its state is `completed` (use \
         `explore_cancel` to stop it; a cancelled exploration resumes with `rustman explore resume`).\n\
         5. Call `get_report` with the `exploration_id` for the summary, then `list_findings` \
         (start with severity `high`) and report what you found, most severe first.\n\
         \n\
         Secrets appear as `«redacted:<name>»`: refer to them by name and never ask for their values."
    )
}

fn triage_findings(exploration_id: &str) -> String {
    format!(
        "Triage the findings of exploration `{exploration_id}`.\n\
         \n\
         1. Call `explore_status` with id `{exploration_id}`; if it is not `completed`, wait or \
         report that it is unfinished.\n\
         2. Call `list_findings` with exploration_id `{exploration_id}`, page through every \
         finding with `next_cursor`.\n\
         3. Group the findings by root cause (for example one missing authorization check behind \
         several operations, or one error handler leaking stack traces), not by rule.\n\
         4. For each group give: the severity (the highest of its findings), the operations \
         affected, the evidence, and reproduction steps from each finding's `reproduction`. \
         The whole report is the resource `rustman://reports/{exploration_id}`.\n\
         5. Propose a fix per root cause, and say which findings may be false positives and why.\n\
         \n\
         Secrets appear as `«redacted:<name>»`: refer to them by name and never ask for their values."
    )
}

fn required<'a>(arguments: Option<&'a JsonObject>, name: &str) -> Result<&'a str, ErrorData> {
    arguments
        .and_then(|a| a.get(name))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| ErrorData::invalid_params(format!("missing argument '{name}'"), None))
}

/// The prompt `name` with `arguments`, redacted with `redactor`.
pub fn get(
    name: &str,
    arguments: Option<&JsonObject>,
    redactor: &OutputRedactor,
) -> Result<GetPromptResult, ErrorData> {
    let (description, text) = match name {
        EXPLORE_API => (
            "Explore an API: plan, review, run, report",
            explore_api(required(arguments, "spec")?, required(arguments, "target")?),
        ),
        TRIAGE_FINDINGS => (
            "Triage an exploration's findings",
            triage_findings(required(arguments, "exploration_id")?),
        ),
        _ => {
            return Err(ErrorData::invalid_params(
                format!("no prompt '{name}'"),
                None,
            ));
        }
    };
    let message = PromptMessage::new_text(Role::User, redactor.text(&text));
    Ok(GetPromptResult::new(vec![message]).with_description(description))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn explore_api_plans_before_it_runs() {
        let args = json!({"spec": "api.yaml", "target": "http://127.0.0.1:8080"});
        let result = get(EXPLORE_API, args.as_object(), &OutputRedactor::new()).unwrap();
        let text = serde_json::to_value(&result.messages[0].content).unwrap()["text"]
            .as_str()
            .unwrap()
            .to_owned();
        let plan = text.find("explore_plan").unwrap();
        assert!(plan < text.find("explore_run").unwrap());
        assert!(get(EXPLORE_API, None, &OutputRedactor::new()).is_err());
        assert!(get("nope", None, &OutputRedactor::new()).is_err());
    }
}
