//! Moving secret collection and folder variables with their scope.
//!
//! A variable declared on a folder applies to that folder's subtree and
//! shadows a variable of the same name further up. When a secret variable
//! is moved under another name (`password` → `password_2`, because the
//! collection's `password` holds another value), only the references that
//! resolve to it are rewritten: those inside the declaring folder, minus
//! subfolders and requests that declare the name again.

use std::collections::HashMap;

use super::{Extractor, has_literal, is_secret_variable};
use crate::ir::{Folder, ImportedRequest, Item, VisitStrings};
use crate::slug::var_name;

/// Variable name → the name its references are rewritten to.
pub(super) type Renames = HashMap<String, String>;

impl Extractor<'_> {
    /// Move the secret variables of `folder` and the folders below it,
    /// rewriting references within each declaring folder's scope.
    /// `inherited` holds the renames in force where `folder` sits.
    pub(super) fn scoped_variables(&mut self, folder: &mut Folder, inherited: &Renames) {
        let mut scope = inherited.clone();
        let mut kept = Vec::new();
        for mut var in std::mem::take(&mut folder.variables) {
            if is_secret_variable(&var) && has_literal(&var.value) {
                let name = self.move_value(&var.name, &var.value, &folder.location);
                scope.insert(var.name.clone(), name);
            } else {
                // A plain variable shadows any rename from further up.
                scope.remove(&var.name);
                self.url(
                    &mut var.value,
                    &var_name(&var.name, "url"),
                    &folder.location,
                );
                kept.push(var);
            }
        }
        folder.variables = kept;
        let renames: Renames = scope
            .iter()
            .filter(|(old, new)| old != new)
            .map(|(old, new)| (old.clone(), new.clone()))
            .collect();
        if !renames.is_empty() {
            folder
                .auth
                .visit_strings(&mut |text| rename_text(text, &renames));
            folder
                .headers
                .visit_strings(&mut |text| rename_text(text, &renames));
            folder
                .variables
                .visit_strings(&mut |text| rename_text(text, &renames));
        }
        for item in &mut folder.items {
            match item {
                Item::Folder(child) => self.scoped_variables(child, &scope),
                Item::Request(request) => rename_request(request, &renames),
            }
        }
    }
}

/// Apply `renames` to `request`, except the names it declares itself.
fn rename_request(request: &mut ImportedRequest, renames: &Renames) {
    if renames.is_empty() {
        return;
    }
    let own = |name: &String| request.variables.iter().any(|v| &v.name == name);
    let applicable: Renames = renames
        .iter()
        .filter(|(old, _)| !own(old))
        .map(|(old, new)| (old.clone(), new.clone()))
        .collect();
    rename(request, &applicable);
}

/// Rewrite `{{old}}` references to `{{new}}` everywhere in `node`, in one
/// pass per string (a rename never feeds another).
pub(super) fn rename(node: &mut dyn VisitStrings, renames: &Renames) {
    if renames.is_empty() {
        return;
    }
    node.visit_strings(&mut |text| rename_text(text, renames));
}

fn rename_text(text: &mut String, renames: &Renames) {
    if !text.contains("{{") {
        return;
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    let mut changed = false;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let inner = &rest[start + 2..];
        let Some(end) = inner.find("}}") else {
            out.push_str(&rest[start..]);
            rest = "";
            break;
        };
        match renames.get(inner[..end].trim()) {
            Some(new) => {
                out.push_str("{{");
                out.push_str(new);
                out.push_str("}}");
                changed = true;
            }
            None => out.push_str(&rest[start..start + end + 4]),
        }
        rest = &inner[end + 2..];
    }
    out.push_str(rest);
    if changed {
        *text = out;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renames_apply_once_and_only_to_whole_references() {
        let renames: Renames = [
            ("a".to_owned(), "b".to_owned()),
            ("b".to_owned(), "c".to_owned()),
        ]
        .into_iter()
        .collect();
        let mut text = "{{a}}/{{ b }}/{{ab}}/{{a".to_owned();
        rename_text(&mut text, &renames);
        assert_eq!(text, "{{b}}/{{c}}/{{ab}}/{{a");
    }
}
