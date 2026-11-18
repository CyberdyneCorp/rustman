//! Environments next to `.http` files: JetBrains `http-client.env.json` and
//! `http-client.private.env.json` (whose values are secrets), and VS Code
//! REST Client's `rest-client.environmentVariables` in
//! `.vscode/settings.json`. `$shared` values apply to every environment.

use std::path::Path;

use serde_json::Value;

use crate::common::value_text;
use crate::error::ImportError;
use crate::ir::{ImportedEnvironment, Location, Note, NoteKind, Variable};
use crate::source::Reader;

const SHARED: &str = "$shared";

/// The environments defined next to the `.http` files in `dir`.
pub(super) fn environments(
    dir: &Path,
    reader: &mut Reader,
    notes: &mut Vec<Note>,
) -> Result<Vec<ImportedEnvironment>, ImportError> {
    let mut envs = Envs::default();
    for (file, secret) in [
        ("http-client.env.json", false),
        ("http-client.private.env.json", true),
    ] {
        let path = dir.join(file);
        if path.is_file() {
            let doc: Value = serde_json::from_str(&reader.read(&path)?)
                .map_err(|e| ImportError::invalid(file, e.to_string()))?;
            envs.add(&doc, file, secret, notes);
        }
    }
    let settings = dir.join(".vscode").join("settings.json");
    if settings.is_file() {
        let shown = ".vscode/settings.json";
        match serde_json::from_str::<Value>(&strip_comments(&reader.read(&settings)?)) {
            Ok(doc) => envs.add(
                &doc["rest-client.environmentVariables"],
                shown,
                false,
                notes,
            ),
            Err(e) => {
                let message = format!("unreadable settings ({e}); environments skipped");
                notes.push(Note::new(
                    NoteKind::Unsupported,
                    message,
                    &Location::item(shown, ""),
                ));
            }
        }
    }
    Ok(envs.finish())
}

#[derive(Default)]
struct Envs {
    list: Vec<ImportedEnvironment>,
    shared: Vec<Variable>,
}

impl Envs {
    /// Add the environments of `doc` (`{env: {name: value}}`) from `file`.
    fn add(&mut self, doc: &Value, file: &str, secret: bool, notes: &mut Vec<Note>) {
        for (env, vars) in doc.as_object().into_iter().flatten() {
            let at = Location::item(file, env);
            let variables = variables(vars, secret, &at, notes);
            let target = if env == SHARED {
                &mut self.shared
            } else {
                let environment = self.environment(env, &at);
                if secret {
                    // Moved secrets are reported where their values were.
                    environment.location = at.clone();
                }
                &mut environment.variables
            };
            for var in variables {
                upsert(target, var);
            }
        }
    }

    fn environment(&mut self, name: &str, at: &Location) -> &mut ImportedEnvironment {
        let index = match self.list.iter().position(|e| e.name == name) {
            Some(index) => index,
            None => {
                self.list.push(ImportedEnvironment {
                    name: name.to_owned(),
                    variables: Vec::new(),
                    location: at.clone(),
                });
                self.list.len() - 1
            }
        };
        &mut self.list[index]
    }

    /// Every environment, with the shared values it does not override.
    fn finish(mut self) -> Vec<ImportedEnvironment> {
        for env in &mut self.list {
            let own = std::mem::take(&mut env.variables);
            let shared: Vec<Variable> = self
                .shared
                .iter()
                .filter(|s| !own.iter().any(|v| v.name == s.name))
                .cloned()
                .collect();
            env.variables = shared.into_iter().chain(own).collect();
        }
        self.list
    }
}

fn upsert(list: &mut Vec<Variable>, var: Variable) {
    match list.iter_mut().find(|v| v.name == var.name) {
        Some(existing) => *existing = var,
        None => list.push(var),
    }
}

/// The scalar values of `vars`; objects and arrays (such as JetBrains auth
/// settings) are reported as unsupported.
fn variables(vars: &Value, secret: bool, at: &Location, notes: &mut Vec<Note>) -> Vec<Variable> {
    let mut out = Vec::new();
    for (name, value) in vars.as_object().into_iter().flatten() {
        if value.is_object() || value.is_array() {
            let message = format!("environment value {name} is not a scalar; unsupported");
            notes.push(Note::new(NoteKind::Unsupported, message, at));
            continue;
        }
        out.push(Variable {
            name: name.clone(),
            value: value_text(value),
            secret,
            path: false,
        });
    }
    out
}

/// `text` without `//` and `/* */` comments outside strings (JSON with
/// comments, as VS Code writes its settings).
fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if c == '\\' {
                out.extend(chars.next());
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match (c, chars.peek()) {
            ('/', Some('/')) => {
                for next in chars.by_ref() {
                    if next == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut last = ' ';
                for next in chars.by_ref() {
                    if last == '*' && next == '/' {
                        break;
                    }
                    last = next;
                }
            }
            _ => {
                in_string = c == '"';
                out.push(c);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_are_stripped_outside_strings() {
        let text = "{\n  // a comment\n  \"a\": \"http://x\", /* b */ \"c\": \"\\\"//\"\n}";
        let doc: Value = serde_json::from_str(&strip_comments(text)).unwrap();
        assert_eq!(doc["a"], "http://x");
        assert_eq!(doc["c"], "\"//");
    }

    #[test]
    fn shared_values_fill_in_and_private_values_are_secret() {
        let mut envs = Envs::default();
        let mut notes = Vec::new();
        let public = serde_json::json!({"$shared": {"v": "1", "host": "h"}, "dev": {"host": "d"}});
        let private = serde_json::json!({"dev": {"token": "t", "auth": {"type": "x"}}});
        envs.add(&public, "pub", false, &mut notes);
        envs.add(&private, "priv", true, &mut notes);
        let list = envs.finish();
        let vars: Vec<(&str, &str, bool)> = list[0]
            .variables
            .iter()
            .map(|v| (v.name.as_str(), v.value.as_str(), v.secret))
            .collect();
        assert_eq!(
            vars,
            [
                ("v", "1", false),
                ("host", "d", false),
                ("token", "t", true)
            ]
        );
        assert_eq!(notes.len(), 1);
    }
}
