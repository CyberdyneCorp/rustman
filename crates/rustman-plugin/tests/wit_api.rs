//! The plugin API freeze (plugins spec, "Plugin API versioning"; task
//! 1.2): within major 1 the `rustman:plugin` WIT package may only grow.
//!
//! The package parses (the same parser as `wasm-tools component wit`), and
//! its API surface, one line per type, function, world import, world
//! export set and function set of an exported interface, must contain
//! every line of the frozen surface
//! `crates/rustman-plugin-sdk/wit/frozen/1.0.0.txt`. New types, functions
//! of imported interfaces, interfaces, worlds and world imports are fine;
//! changing or removing anything frozen, changing what a world exports, or
//! adding a function to an exported interface (which every existing
//! plugin would then lack) fails this test.
//!
//! After an additive edit, refresh the frozen file with
//! `RUSTMAN_FREEZE_WIT=1 cargo test -p rustman-plugin --test wit_api`;
//! never remove lines from it within a major.

use std::collections::BTreeSet;
use std::path::PathBuf;

use wit_parser::{Handle, Resolve, Type, TypeDefKind, WorldItem};

fn frozen_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../rustman-plugin-sdk/wit/frozen/1.0.0.txt")
}

/// The API surface of a WIT package source.
fn surface(wit: &str) -> BTreeSet<String> {
    let mut resolve = Resolve::default();
    let package = resolve
        .push_source("plugin.wit", wit)
        .expect("the WIT parses");
    let mut lines = BTreeSet::new();
    for (_, iface) in &resolve.packages[package].interfaces {
        let iface = &resolve.interfaces[*iface];
        let name = iface.name.clone().unwrap_or_default();
        for (ty_name, id) in &iface.types {
            let def = describe_kind(&resolve, &resolve.types[*id].kind);
            lines.insert(format!("type {name}.{ty_name} = {def}"));
        }
        for (func_name, func) in &iface.functions {
            let params: Vec<String> = func
                .params
                .iter()
                .map(|p| format!("{}: {}", p.name, describe(&resolve, &p.ty)))
                .collect();
            let result = func
                .result
                .as_ref()
                .map(|r| format!(" -> {}", describe(&resolve, r)))
                .unwrap_or_default();
            lines.insert(format!(
                "func {name}.{func_name}({}){result}",
                params.join(", ")
            ));
        }
    }
    for (world_name, world) in &resolve.packages[package].worlds {
        let world = &resolve.worlds[*world];
        for key in world.imports.keys() {
            lines.insert(format!(
                "world {world_name} imports {}",
                unversioned(&resolve.name_world_key(key))
            ));
        }
        let exports: Vec<String> = world
            .exports
            .iter()
            .map(|(key, item)| match item {
                WorldItem::Interface { .. } => unversioned(&resolve.name_world_key(key)),
                other => format!("{other:?}"),
            })
            .collect();
        lines.insert(format!(
            "world {world_name} exports [{}]",
            exports.join(", ")
        ));
        for item in world.exports.values() {
            if let WorldItem::Interface { id, .. } = item {
                lines.insert(exported_functions(&resolve, *id));
            }
        }
    }
    lines
}

/// The function set a plugin must implement for an exported interface.
fn exported_functions(resolve: &Resolve, id: wit_parser::InterfaceId) -> String {
    let iface = &resolve.interfaces[id];
    let names: Vec<&str> = iface.functions.keys().map(String::as_str).collect();
    format!(
        "export {} functions [{}]",
        iface.name.clone().unwrap_or_default(),
        names.join(", ")
    )
}

/// `rustman:plugin/http@1.0.0` without its version, which minors change.
fn unversioned(name: &str) -> String {
    name.split('@').next().unwrap_or(name).to_owned()
}

fn describe(resolve: &Resolve, ty: &Type) -> String {
    match ty {
        Type::Id(id) => {
            let def = &resolve.types[*id];
            match &def.name {
                Some(name) => name.clone(),
                None => describe_kind(resolve, &def.kind),
            }
        }
        other => format!("{other:?}").to_lowercase(),
    }
}

fn describe_kind(resolve: &Resolve, kind: &TypeDefKind) -> String {
    let d = |ty: &Type| describe(resolve, ty);
    let opt = |ty: &Option<Type>| ty.as_ref().map_or("_".to_owned(), d);
    match kind {
        TypeDefKind::Record(r) => {
            let fields: Vec<String> = r
                .fields
                .iter()
                .map(|f| format!("{}: {}", f.name, d(&f.ty)))
                .collect();
            format!("record {{ {} }}", fields.join(", "))
        }
        TypeDefKind::Variant(v) => {
            let cases: Vec<String> = v
                .cases
                .iter()
                .map(|c| format!("{}({})", c.name, opt(&c.ty)))
                .collect();
            format!("variant {{ {} }}", cases.join(", "))
        }
        TypeDefKind::Enum(e) => {
            let cases: Vec<&str> = e.cases.iter().map(|c| c.name.as_str()).collect();
            format!("enum {{ {} }}", cases.join(", "))
        }
        TypeDefKind::Flags(f) => {
            let flags: Vec<&str> = f.flags.iter().map(|f| f.name.as_str()).collect();
            format!("flags {{ {} }}", flags.join(", "))
        }
        TypeDefKind::Tuple(t) => {
            let types: Vec<String> = t.types.iter().map(d).collect();
            format!("tuple<{}>", types.join(", "))
        }
        TypeDefKind::Option(t) => format!("option<{}>", d(t)),
        TypeDefKind::Result(r) => format!("result<{}, {}>", opt(&r.ok), opt(&r.err)),
        TypeDefKind::List(t) => format!("list<{}>", d(t)),
        TypeDefKind::Handle(Handle::Own(id)) => format!("own<{}>", d(&Type::Id(*id))),
        TypeDefKind::Handle(Handle::Borrow(id)) => format!("borrow<{}>", d(&Type::Id(*id))),
        TypeDefKind::Resource => "resource".to_owned(),
        TypeDefKind::Type(t) => d(t),
        other => format!("{other:?}"),
    }
}

/// The frozen lines `current` lacks.
fn breaking_changes(frozen: &str, current: &BTreeSet<String>) -> Vec<String> {
    frozen
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .filter(|line| !current.contains(*line))
        .map(str::to_owned)
        .collect()
}

#[test]
fn the_package_parses_with_every_world() {
    let current = surface(rustman_plugin_sdk::WIT);
    for (world, export) in rustman_plugin_sdk::WORLDS {
        let line = format!("world {world} exports [rustman:plugin/{export}]");
        assert!(current.contains(&line), "{line}");
    }
    assert!(current.contains("world host exports []"));
}

#[test]
fn edits_within_major_1_are_additive() {
    let current = surface(rustman_plugin_sdk::WIT);
    if std::env::var_os("RUSTMAN_FREEZE_WIT").is_some() {
        let mut text = String::from(
            "# The frozen API surface of rustman:plugin@1.0.0 (see tests/wit_api.rs).\n",
        );
        for line in &current {
            text.push_str(line);
            text.push('\n');
        }
        std::fs::write(frozen_path(), text).unwrap();
    }
    let frozen = std::fs::read_to_string(frozen_path()).expect("the frozen surface exists");
    let broken = breaking_changes(&frozen, &current);
    assert!(
        broken.is_empty(),
        "non-additive changes to the plugin API (major 1):\n{}",
        broken.join("\n")
    );
}

#[test]
fn the_check_catches_breaking_edits() {
    let frozen = std::fs::read_to_string(frozen_path()).unwrap();
    let wit = rustman_plugin_sdk::WIT;
    let edits = [
        // A removed function.
        wit.replace(
            "    list-dir: func(path: string) -> result<list<string>, host-error>;\n",
            "",
        ),
        // A changed record.
        wit.replace(
            "        status: u16,\n        headers: list<header>,",
            "        status: u32,\n        headers: list<header>,",
        ),
        // A new export on an existing world.
        wit.replace(
            "    export auth-scheme;\n",
            "    export auth-scheme;\n    export report-renderer;\n",
        ),
        // A changed parameter.
        wit.replace("bytes: func(len: u32)", "bytes: func(len: u64)"),
        // A new function plugins export: 1.0 plugins lack it.
        wit.replace(
            "    schemes: func() -> list<string>;\n",
            "    schemes: func() -> list<string>;\n    describe: func() -> string;\n",
        ),
    ];
    for (i, edited) in edits.iter().enumerate() {
        assert_ne!(edited, wit, "edit {i} applied");
        let broken = breaking_changes(&frozen, &surface(edited));
        assert!(!broken.is_empty(), "edit {i} went unnoticed");
    }
    // Additions pass.
    let added = wit.replace(
        "    get: func(name: string) -> result<option<string>, host-error>;\n",
        "    get: func(name: string) -> result<option<string>, host-error>;\n    names: func() -> list<string>;\n",
    );
    assert_ne!(added, wit);
    assert!(breaking_changes(&frozen, &surface(&added)).is_empty());
}
