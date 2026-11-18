//! Postman environment exports (task 2.4).

use serde_json::Value;

use super::{Context, is_environment, string};
use crate::error::ImportError;
use crate::ir::{Folder, ImportedCollection, ImportedEnvironment, NoteKind, Variable};

/// Parse the environment export `text`, read from `file`. The result has
/// no requests and one environment.
pub fn parse_environment(text: &str, file: &str) -> Result<ImportedCollection, ImportError> {
    let doc: Value =
        serde_json::from_str(text).map_err(|e| ImportError::invalid(file, e.to_string()))?;
    if !is_environment(&doc) {
        return Err(ImportError::invalid(
            file,
            "not a Postman environment export",
        ));
    }
    let mut ctx = Context {
        file: file.to_owned(),
        notes: Vec::new(),
    };
    let name = string(&doc["name"]).unwrap_or_else(|| "environment".to_owned());
    let at = ctx.location(&name);
    let mut variables = Vec::new();
    for value in doc["values"].as_array().into_iter().flatten() {
        let Some(key) = string(&value["key"]).filter(|k| !k.is_empty()) else {
            continue;
        };
        if value["enabled"] == false {
            ctx.note(
                NoteKind::Dropped,
                format!("disabled variable {key} dropped"),
                &at,
            );
            continue;
        }
        variables.push(Variable {
            name: key,
            value: string(&value["value"]).unwrap_or_default(),
            secret: value["type"] == "secret",
        });
    }
    Ok(ImportedCollection {
        name: name.clone(),
        root: Folder::default(),
        environments: vec![ImportedEnvironment {
            name,
            variables,
            location: at,
        }],
        notes: ctx.notes,
    })
}
