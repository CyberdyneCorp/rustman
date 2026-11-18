//! Path variables (Postman `/users/:id`, Bruno `params:path`) are a
//! namespace of their own: `:token` in a path and `{{token}}` elsewhere
//! are different variables. Rustman has one namespace, and a request
//! variable applies to the whole request (auth included), so a path
//! variable whose name is used anywhere else is given a fresh name
//! (`path_token`, `path_token_2`, ...) that no variable or reference of the
//! collection uses.

use std::collections::HashSet;

use crate::ir::{Folder, ImportedCollection, ImportedRequest, Item, VisitStrings};

/// Rename the path variables of `collection` that would take over another
/// variable's `{{name}}`.
pub fn separate(collection: &mut ImportedCollection) {
    let mut used = HashSet::new();
    names_in_folder(&mut collection.root, &mut used);
    for env in &collection.environments {
        used.extend(env.variables.iter().map(|v| v.name.clone()));
    }
    separate_in(&mut collection.root, &mut used);
}

/// Every variable name declared, and every name referenced, outside path
/// variables and their placeholders.
fn names_in_folder(folder: &mut Folder, used: &mut HashSet<String>) {
    used.extend(folder.variables.iter().map(|v| v.name.clone()));
    let mut refs = |text: &mut String| references(text, used);
    folder.auth.visit_strings(&mut refs);
    folder.headers.visit_strings(&mut refs);
    folder.variables.visit_strings(&mut refs);
    for item in &mut folder.items {
        match item {
            Item::Folder(child) => names_in_folder(child, used),
            Item::Request(request) => names_in_request(request, used),
        }
    }
}

fn names_in_request(request: &mut ImportedRequest, used: &mut HashSet<String>) {
    let path_names: HashSet<String> = path_names(request).collect();
    used.extend(
        request
            .variables
            .iter()
            .filter(|v| !v.path)
            .map(|v| v.name.clone()),
    );
    // The URL's placeholders of path variables are theirs; any other
    // reference in it, and every reference elsewhere, is another variable's.
    let mut in_url = HashSet::new();
    references(&request.url, &mut in_url);
    used.extend(in_url.into_iter().filter(|n| !path_names.contains(n)));
    let url = std::mem::take(&mut request.url);
    request.visit_strings(&mut |text| references(text, used));
    request.url = url;
}

fn path_names(request: &ImportedRequest) -> impl Iterator<Item = String> + '_ {
    request
        .variables
        .iter()
        .filter(|v| v.path)
        .map(|v| v.name.clone())
}

fn separate_in(folder: &mut Folder, used: &mut HashSet<String>) {
    for item in &mut folder.items {
        match item {
            Item::Folder(child) => separate_in(child, used),
            Item::Request(request) => separate_request(request, used),
        }
    }
}

fn separate_request(request: &mut ImportedRequest, used: &mut HashSet<String>) {
    for i in 0..request.variables.len() {
        let var = &request.variables[i];
        if !var.path || !used.contains(&var.name) {
            continue;
        }
        let old = var.name.clone();
        let fresh = (1..)
            .map(|n| match n {
                1 => format!("path_{old}"),
                n => format!("path_{old}_{n}"),
            })
            .find(|c| !used.contains(c) && !request.variables.iter().any(|v| &v.name == c))
            .expect("some name is free");
        request.url = request
            .url
            .replace(&format!("{{{{{old}}}}}"), &format!("{{{{{fresh}}}}}"));
        used.insert(fresh.clone());
        request.variables[i].name = fresh;
    }
}

/// Add the names `text` references as `{{name}}` to `out`.
fn references(text: &str, out: &mut HashSet<String>) {
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let inner = &rest[start + 2..];
        let Some(end) = inner.find("}}") else {
            return;
        };
        out.insert(inner[..end].trim().to_owned());
        rest = &inner[end + 2..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{ImportedAuth, Variable};

    fn path_var(name: &str, value: &str) -> Variable {
        Variable {
            name: name.into(),
            value: value.into(),
            secret: false,
            path: true,
        }
    }

    fn collection(request: ImportedRequest) -> ImportedCollection {
        ImportedCollection {
            root: Folder {
                variables: vec![Variable {
                    name: "token".into(),
                    value: "{{env_tok}}".into(),
                    ..Variable::default()
                }],
                items: vec![Item::Request(Box::new(request))],
                ..Folder::default()
            },
            ..ImportedCollection::default()
        }
    }

    #[test]
    fn a_path_variable_named_like_another_variable_is_renamed() {
        let mut c = collection(ImportedRequest {
            url: "https://api.test/{{token}}/{{id}}".into(),
            variables: vec![path_var("token", "TOK_PATH"), path_var("id", "7")],
            auth: Some(ImportedAuth::Bearer {
                token: "{{token}}".into(),
            }),
            ..ImportedRequest::default()
        });
        separate(&mut c);
        let r = c.root.requests_mut().remove(0);
        assert_eq!(r.url, "https://api.test/{{path_token}}/{{id}}");
        assert_eq!(r.variables[0].name, "path_token");
        assert_eq!(r.variables[1].name, "id");
        assert_eq!(
            r.auth,
            Some(ImportedAuth::Bearer {
                token: "{{token}}".into()
            })
        );
    }
}
