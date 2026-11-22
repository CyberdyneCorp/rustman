//! evm-signing "Agents can never sign" as a build property (add-evm-toolkit
//! design D2, task 5.6): nothing `rustman-mcp` links, not even through its
//! dev-dependencies, reaches the signing gate or a key-handling crate.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The signing gate and the keystore decryption only it uses.
const SIGNING: [&str; 2] = ["rustman-evm-signing", "eth-keystore"];

/// `package -> dependencies` from the workspace's `Cargo.lock`. The lock
/// lists every dependency of every feature and target, so the graph is a
/// superset of what any build links; an edge to any version counts.
fn lock_graph() -> BTreeMap<String, Vec<String>> {
    let lock = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock");
    let text = std::fs::read_to_string(lock).expect("Cargo.lock");
    let lock: toml::Value = toml::from_str(&text).expect("Cargo.lock is TOML");
    let mut graph: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for package in lock["package"].as_array().expect("packages") {
        let name = package["name"].as_str().expect("name").to_owned();
        let deps = package
            .get("dependencies")
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(toml::Value::as_str)
            .map(|d| d.split(' ').next().unwrap_or(d).to_owned());
        graph.entry(name).or_default().extend(deps);
    }
    graph
}

fn reachable(graph: &BTreeMap<String, Vec<String>>, from: &str) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut todo = vec![from.to_owned()];
    while let Some(name) = todo.pop() {
        if seen.insert(name.clone()) {
            todo.extend(graph.get(&name).into_iter().flatten().cloned());
        }
    }
    seen
}

#[test]
fn rustman_mcp_does_not_depend_on_the_signing_gate() {
    let graph = lock_graph();
    assert!(
        reachable(&graph, "rustman-cli").contains("rustman-evm-signing"),
        "the graph is read correctly: the CLI links the signing gate"
    );
    let linked = reachable(&graph, "rustman-mcp");
    assert!(
        linked.contains("rustman-evm"),
        "the read-only toolkit is linked"
    );
    for name in SIGNING {
        assert!(!linked.contains(name), "rustman-mcp reaches {name}");
    }
}

#[test]
fn the_read_only_toolkit_has_no_signing_module() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../rustman-evm/src");
    for entry in std::fs::read_dir(src).expect("rustman-evm/src") {
        let name = entry.unwrap().file_name();
        let name = name.to_string_lossy();
        assert!(!name.starts_with("signing"), "rustman-evm has {name}");
    }
}
