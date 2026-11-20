//! Environment values (api-docs-generation "Environment values are not
//! published by default"): variables appear by name only, unless listed in
//! `[docs].publish_variables`, and then only with `--env`.

use rustman_core::Workspace;
use rustman_core::environment;

use crate::error::DocsError;
use crate::model::Row;
use crate::rendered::DocsRedactor;

/// The published variables, in `publish_variables` order, with their value
/// in environment `env` (none without `--env`). A secret of any
/// environment cannot be published.
pub fn published_variables(
    workspace: &Workspace,
    env: Option<&str>,
    redactor: &DocsRedactor,
) -> Result<Vec<Row>, DocsError> {
    let names = &workspace.settings.docs.publish_variables;
    if let Some(secret) = names
        .iter()
        .find(|name| workspace.environments.iter().any(|e| e.is_secret(name)))
    {
        return Err(DocsError::SecretPublished(secret.clone()));
    }
    let Some(env) = env else {
        return Ok(Vec::new());
    };
    let env = environment::find(&workspace.environments, env)?;
    Ok(names
        .iter()
        .map(|name| {
            let value = env
                .variables
                .get(name)
                .or_else(|| workspace.settings.variables.get(name))
                .map(ToString::to_string)
                .unwrap_or_default();
            Row {
                name: redactor.render(name),
                value: redactor.render(&value),
                description: None,
            }
        })
        .collect())
}
