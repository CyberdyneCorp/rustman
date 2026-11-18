//! Environment files: source environments and the secrets moved out of
//! requests, merged into `environments/<name>.yaml` under the conflict policy.

use rustman_core::Environment;
use rustman_core::model::{VarValue, Variables};

use crate::ir::ImportedCollection;
use crate::plan::OnConflict;
use crate::report::Conflict;
use crate::secrets::{Existing, MovedSecret, environment_name};

/// What the import adds to one environment.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct EnvChange {
    pub name: String,
    pub variables: Vec<(String, String)>,
    pub secrets: Vec<String>,
}

/// The changes for every environment the import touches, in source order:
/// source environments first, then the target of the moved secrets.
pub(crate) fn changes(collection: &ImportedCollection, moved: &[MovedSecret]) -> Vec<EnvChange> {
    let mut out: Vec<EnvChange> = Vec::new();
    for env in &collection.environments {
        let change = entry(&mut out, &environment_name(&env.name));
        for var in &env.variables {
            if var.secret {
                change.secrets.push(var.name.clone());
            } else {
                change.variables.push((var.name.clone(), var.value.clone()));
            }
        }
    }
    for secret in moved.iter().filter(|m| !m.from_environment) {
        let change = entry(&mut out, &secret.environment);
        let name = secret.secret.name().to_owned();
        if !change.secrets.contains(&name) {
            change.secrets.push(name);
        }
    }
    out
}

fn entry<'a>(changes: &'a mut Vec<EnvChange>, name: &str) -> &'a mut EnvChange {
    let index = match changes.iter().position(|c| c.name == name) {
        Some(index) => index,
        None => {
            changes.push(EnvChange {
                name: name.to_owned(),
                ..EnvChange::default()
            });
            changes.len() - 1
        }
    };
    &mut changes[index]
}

/// The result of merging one change into an environment.
#[derive(Debug)]
pub(crate) struct Merged {
    pub env: Environment,
    pub changed: bool,
    pub conflicts: Vec<Conflict>,
    /// Secrets whose stored value is kept: the moved value is not stored.
    pub kept_secrets: Vec<String>,
}

/// Merge `change` into the environment as it exists (if it does).
pub(crate) fn merge(change: &EnvChange, existing: &Existing<'_>, policy: OnConflict) -> Merged {
    let current = existing.environments.iter().find(|e| e.name == change.name);
    let mut merger = Merger {
        path: format!("environments/{}.yaml", change.name),
        vars: current
            .map(|e| {
                e.variables
                    .iter()
                    .map(|(n, v)| (n.to_owned(), v.clone()))
                    .collect()
            })
            .unwrap_or_default(),
        env: current.cloned().unwrap_or_else(|| Environment {
            name: change.name.clone(),
            ..Environment::default()
        }),
        policy,
        merged: Merged {
            env: Environment::default(),
            changed: current.is_none(),
            conflicts: Vec::new(),
            kept_secrets: Vec::new(),
        },
    };
    for (name, value) in &change.variables {
        merger.variable(name, value);
    }
    for name in &change.secrets {
        let stored = (existing.lookup)(&change.name, name).is_some();
        merger.secret(name, stored);
    }
    merger.finish()
}

struct Merger {
    path: String,
    vars: Vec<(String, VarValue)>,
    env: Environment,
    policy: OnConflict,
    merged: Merged,
}

impl Merger {
    fn conflict(&mut self, resolution: String) {
        self.merged.conflicts.push(Conflict {
            path: self.path.clone(),
            resolution,
        });
    }

    fn variable(&mut self, name: &str, value: &str) {
        let new = VarValue::String(value.to_owned());
        if self.env.is_secret(name) {
            self.conflict(format!("variable {name} kept as a secret (exists)"));
            return;
        }
        match self.vars.iter_mut().find(|(n, _)| n == name) {
            None => {
                self.vars.push((name.to_owned(), new));
                self.merged.changed = true;
            }
            Some((_, old)) if *old == new => {}
            Some((_, old)) if self.policy == OnConflict::Overwrite => {
                *old = new;
                self.merged.changed = true;
                self.conflict(format!("variable {name} overwritten"));
            }
            Some(_) => self.conflict(format!("variable {name} skipped (exists)")),
        }
    }

    /// Declare `name` as a secret; `stored` when it already has a value.
    fn secret(&mut self, name: &str, stored: bool) {
        let is_var = self.vars.iter().any(|(n, _)| n == name);
        let overwrite = self.policy == OnConflict::Overwrite;
        if is_var && !overwrite {
            self.conflict(format!("variable {name} skipped (exists)"));
            self.merged.kept_secrets.push(name.to_owned());
            return;
        }
        if is_var {
            self.vars.retain(|(n, _)| n != name);
            self.conflict(format!("variable {name} overwritten as a secret"));
        }
        if !self.env.is_secret(name) {
            self.env.secrets.push(name.to_owned());
            self.merged.changed = true;
        } else if stored && !overwrite {
            self.conflict(format!("secret {name} skipped (exists)"));
            self.merged.kept_secrets.push(name.to_owned());
        }
    }

    fn finish(mut self) -> Merged {
        self.env.variables = Variables::new(self.vars);
        self.merged.env = self.env;
        self.merged
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustman_core::Secret;

    fn change() -> EnvChange {
        EnvChange {
            name: "staging".into(),
            variables: vec![("host".into(), "new".into()), ("port".into(), "1".into())],
            secrets: vec!["api_key".into()],
        }
    }

    fn existing_env() -> Vec<Environment> {
        vec![Environment {
            name: "staging".into(),
            variables: Variables::new(vec![("host".into(), VarValue::String("old".into()))]),
            secrets: vec!["api_key".into()],
            production: true,
        }]
    }

    #[test]
    fn a_new_environment_is_created() {
        let merged = merge(&change(), &Existing::none(), OnConflict::Skip);
        assert!(merged.changed);
        assert_eq!(merged.env.secrets, ["api_key"]);
        assert_eq!(
            merged.env.variables.get("port"),
            Some(&VarValue::String("1".into()))
        );
        assert!(merged.conflicts.is_empty());
    }

    #[test]
    fn skip_keeps_existing_values_and_stored_secrets() {
        let envs = existing_env();
        let lookup = |_: &str, n: &str| Some(Secret::new(n, "v".to_owned()));
        let existing = Existing {
            environments: &envs,
            lookup: &lookup,
        };
        let merged = merge(&change(), &existing, OnConflict::Skip);
        assert_eq!(
            merged.env.variables.get("host"),
            Some(&VarValue::String("old".into()))
        );
        assert!(merged.env.production);
        assert_eq!(merged.kept_secrets, ["api_key"]);
        let resolutions: Vec<&str> = merged
            .conflicts
            .iter()
            .map(|c| c.resolution.as_str())
            .collect();
        assert_eq!(
            resolutions,
            [
                "variable host skipped (exists)",
                "secret api_key skipped (exists)"
            ]
        );
    }

    #[test]
    fn overwrite_replaces_values() {
        let envs = existing_env();
        let existing = Existing {
            environments: &envs,
            lookup: &|_, _| None,
        };
        let merged = merge(&change(), &existing, OnConflict::Overwrite);
        assert_eq!(
            merged.env.variables.get("host"),
            Some(&VarValue::String("new".into()))
        );
        assert!(merged.kept_secrets.is_empty());
        assert_eq!(merged.conflicts[0].resolution, "variable host overwritten");
    }
}
