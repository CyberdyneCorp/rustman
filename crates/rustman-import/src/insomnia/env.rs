//! Insomnia environments: a base environment and its sub-environments,
//! flattened into one Rustman environment per sub-environment. Every value
//! an environment marked `isPrivate` defines is a secret, the way
//! `http-client.private.env.json` values are.

use serde_json::{Map, Value};

use crate::common::value_text;
use crate::ir::{ImportedEnvironment, Location, Variable};
use crate::postman::string;

/// The base environment and each sub-environment.
#[derive(Debug, Default)]
pub(super) struct EnvSet {
    base: Option<Env>,
    subs: Vec<Env>,
}

/// One Insomnia environment: its name, data and `isPrivate` flag.
#[derive(Debug)]
struct Env {
    name: String,
    data: Map<String, Value>,
    private: bool,
}

impl Env {
    fn of(v: &Value) -> Self {
        Self {
            name: string(&v["name"]).unwrap_or_else(|| "environment".to_owned()),
            data: v["data"].as_object().cloned().unwrap_or_default(),
            private: v["isPrivate"] == true,
        }
    }
}

/// v4: the environment whose parent is the workspace, and its children.
pub(super) fn from_v4(resources: &[&Value], workspace: &str) -> EnvSet {
    let envs: Vec<&&Value> = resources
        .iter()
        .filter(|r| r["_type"] == "environment")
        .collect();
    let Some(base) = envs.iter().find(|e| e["parentId"] == workspace) else {
        return EnvSet::default();
    };
    let base_id = string(&base["_id"]).unwrap_or_default();
    let mut subs: Vec<&&Value> = envs
        .iter()
        .copied()
        .filter(|e| e["parentId"] == base_id.as_str())
        .collect();
    subs.sort_by(|a, b| super::sort_key(a).total_cmp(&super::sort_key(b)));
    EnvSet {
        base: Some(Env::of(base)),
        subs: subs.into_iter().map(|e| Env::of(e)).collect(),
    }
}

/// v5: `environments: {name, data, subEnvironments: [...]}`.
pub(super) fn from_v5(envs: &Value) -> EnvSet {
    if !envs.is_object() {
        return EnvSet::default();
    }
    EnvSet {
        base: Some(Env::of(envs)),
        subs: envs["subEnvironments"]
            .as_array()
            .into_iter()
            .flatten()
            .map(Env::of)
            .collect(),
    }
}

impl EnvSet {
    /// One environment per sub-environment, holding the base values with
    /// the sub-environment's overrides; the base alone when it has none.
    pub(super) fn flatten(self, at: &Location) -> Vec<ImportedEnvironment> {
        let Some(base) = self.base else {
            return Vec::new();
        };
        if self.subs.is_empty() {
            if base.data.is_empty() {
                return Vec::new();
            }
            return vec![environment(&base.name, &base, None, at)];
        }
        self.subs
            .iter()
            .map(|sub| environment(&sub.name, &base, Some(sub), at))
            .collect()
    }
}

/// The base values with `sub`'s overrides; a value is secret when the
/// environment that set it is private.
fn environment(name: &str, base: &Env, sub: Option<&Env>, at: &Location) -> ImportedEnvironment {
    let mut merged: Map<String, Value> = Map::new();
    let mut private = std::collections::HashSet::new();
    for env in std::iter::once(base).chain(sub) {
        for (key, value) in &env.data {
            merged.insert(key.clone(), value.clone());
            if env.private {
                private.insert(key.clone());
            } else {
                private.remove(key);
            }
        }
    }
    let mut variables = Vec::new();
    for (key, value) in &merged {
        flatten_into(key, value, private.contains(key), &mut variables);
    }
    ImportedEnvironment {
        name: name.to_owned(),
        variables,
        location: Location::item(&at.file, name),
    }
}

/// `value` as one variable per leaf, named the way Insomnia templates
/// reference them (`_.auth.token`, `_.hosts[0]`), so each leaf name goes
/// through secret extraction on its own.
fn flatten_into(name: &str, value: &Value, secret: bool, out: &mut Vec<Variable>) {
    match value {
        Value::Object(map) if !map.is_empty() => {
            for (key, child) in map {
                flatten_into(&format!("{name}.{key}"), child, secret, out);
            }
        }
        Value::Array(items) if !items.is_empty() => {
            for (i, child) in items.iter().enumerate() {
                flatten_into(&format!("{name}[{i}]"), child, secret, out);
            }
        }
        leaf => out.push(Variable {
            name: name.to_owned(),
            value: value_text(leaf),
            secret,
            path: false,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sub_environments_override_the_base() {
        let envs = serde_json::json!({
            "name": "Base Environment",
            "data": {"host": "http://localhost", "port": 80},
            "subEnvironments": [{"name": "Prod", "data": {"host": "https://prod"}}]
        });
        let out = from_v5(&envs).flatten(&Location::default());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "Prod");
        let vars: Vec<(&str, &str)> = out[0]
            .variables
            .iter()
            .map(|v| (v.name.as_str(), v.value.as_str()))
            .collect();
        assert_eq!(vars, [("host", "https://prod"), ("port", "80")]);
    }

    #[test]
    fn a_base_without_subs_is_its_own_environment() {
        let envs = serde_json::json!({"name": "Base", "data": {"a": "1"}});
        let out = from_v5(&envs).flatten(&Location::default());
        assert_eq!(out[0].name, "Base");
        let empty = serde_json::json!({"name": "Base", "data": {}});
        assert!(from_v5(&empty).flatten(&Location::default()).is_empty());
    }

    #[test]
    fn nested_values_become_one_variable_per_leaf() {
        let envs = serde_json::json!({"name": "Base", "data": {
            "auth": {"token": "t1", "deep": {"password": "p1"}},
            "hosts": ["a", {"key": "k1"}],
            "empty": {}
        }});
        let out = from_v5(&envs).flatten(&Location::default());
        let vars: Vec<(&str, &str)> = out[0]
            .variables
            .iter()
            .map(|v| (v.name.as_str(), v.value.as_str()))
            .collect();
        assert_eq!(
            vars,
            [
                ("auth.token", "t1"),
                ("auth.deep.password", "p1"),
                ("hosts[0]", "a"),
                ("hosts[1].key", "k1"),
                ("empty", "{}"),
            ]
        );
    }

    #[test]
    fn private_environment_values_are_secret() {
        let envs = serde_json::json!({
            "name": "Base",
            "data": {"host": "h", "shared": "base"},
            "subEnvironments": [{
                "name": "Private", "isPrivate": true,
                "data": {"plain": "p", "auth": {"id": "i"}}
            }, {"name": "Public", "data": {"plain": "q"}}]
        });
        let out = from_v5(&envs).flatten(&Location::default());
        let secret = |i: usize| -> Vec<(&str, bool)> {
            out[i]
                .variables
                .iter()
                .map(|v| (v.name.as_str(), v.secret))
                .collect()
        };
        assert_eq!(
            secret(0),
            [
                ("host", false),
                ("shared", false),
                ("plain", true),
                ("auth.id", true)
            ]
        );
        assert!(secret(1).iter().all(|(_, s)| !s));
    }
}
