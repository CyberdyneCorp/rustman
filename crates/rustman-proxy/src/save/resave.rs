//! Saving again into a workspace that holds an earlier recording (task
//! 6.2: re-saving gives no diff).
//!
//! - [`name_secrets`]: secret names are chosen against the target
//!   environment with its declared secrets free for reuse, so a later
//!   session gives `api_key` the same name instead of `api_key_2`. Raw
//!   values may be stored nowhere, so they cannot be compared; the name
//!   follows the field, like `auth_token` does.
//! - [`keep_earlier_order`]: a `_collection.yaml` rewritten by a save keeps
//!   the entries of the earlier saves that still exist, before the new ones.

use std::path::Path;

use rustman_core::Environment;
use rustman_core::collection::CollectionMeta;
use rustman_core::format::from_yaml;
use rustman_core::format::write::collection_to_yaml;
use rustman_core::model::REQUEST_FILE_SUFFIX;
use rustman_import::Plan;
use rustman_import::ir::{ImportedCollection, Variable};
use rustman_import::report::{Action, FileKind};
use rustman_import::secrets::{Existing, extract};

/// Replace the secrets the importer would find in `collection` by
/// placeholders, and declare them in its `target` environment. Names the
/// workspace's `target` environment already declares as secrets are
/// reused; plain variables and names the collection uses stay taken.
pub fn name_secrets(collection: &mut ImportedCollection, target: &str, workspace: &[Environment]) {
    let mut environments = workspace.to_vec();
    for env in environments.iter_mut().filter(|e| e.name == target) {
        env.secrets.clear();
    }
    let existing = Existing {
        environments: &environments,
        lookup: &|_, _| None,
    };
    // The environment holds the credential headers already moved; it is
    // set aside so they are not moved twice. The notes come back when the
    // importer runs.
    let declared = std::mem::take(&mut collection.environments);
    let notes = collection.notes.len();
    let moved = extract(collection, target, &existing);
    collection.notes.truncate(notes);
    collection.environments = declared;
    let Some(env) = collection
        .environments
        .iter_mut()
        .find(|e| e.name == target)
    else {
        return;
    };
    env.variables.extend(moved.into_iter().map(|m| Variable {
        name: m.secret.name().to_owned(),
        value: m.secret.expose().clone(),
        secret: true,
        path: false,
    }));
}

/// Give every `_collection.yaml` of `plan` that replaces an existing one
/// the earlier `order:` entries it drops (when their file or folder is
/// still on disk), ahead of the entries it adds.
pub fn keep_earlier_order(plan: &mut Plan, root: &Path) {
    for file in plan.files.iter_mut() {
        if file.kind != FileKind::Collection || file.action != Action::Overwrite {
            continue;
        }
        let Some(text) = merged_order(root, &file.path, &file.text) else {
            continue;
        };
        let current = std::fs::read_to_string(root.join(&file.path)).unwrap_or_default();
        file.action = if current == text {
            Action::Unchanged
        } else {
            Action::Overwrite
        };
        file.text = text;
    }
}

/// The planned `text` of the collection file at `rel`, with the order
/// entries of the file on disk it lacks; `None` when it lacks none.
fn merged_order(root: &Path, rel: &str, text: &str) -> Option<String> {
    let path = Path::new(rel);
    let current = std::fs::read_to_string(root.join(rel)).ok()?;
    let earlier: CollectionMeta = from_yaml(path, &current).ok()?;
    let mut planned: CollectionMeta = from_yaml(path, text).ok()?;
    let dir = root.join(rel).parent()?.to_path_buf();
    let new = planned.order.take().unwrap_or_default();
    let mut order: Vec<String> = earlier
        .order
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| new.contains(entry) || still_there(&dir, entry))
        .collect();
    if new.iter().all(|entry| order.contains(entry)) && order.len() == new.len() {
        return None;
    }
    let added: Vec<String> = new.into_iter().filter(|e| !order.contains(e)).collect();
    order.extend(added);
    planned.order = Some(order);
    let comments: String = text
        .lines()
        .take_while(|l| l.starts_with('#'))
        .map(|l| format!("{l}\n"))
        .collect();
    Some(format!("{comments}{}", collection_to_yaml(&planned)))
}

fn still_there(dir: &Path, entry: &str) -> bool {
    dir.join(entry).is_dir() || dir.join(format!("{entry}{REQUEST_FILE_SUFFIX}")).is_file()
}
