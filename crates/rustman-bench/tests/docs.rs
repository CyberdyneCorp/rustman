//! `docs/bench.md` and `docs/bench-internals.md` (add-load-testing task
//! 3.5): linked from the docs index, every relative link resolves, and the
//! texts they quote are the ones the code produces.

use std::fs;
use std::path::{Path, PathBuf};

use rustman_bench::caps::check;
use rustman_bench::{ConstantVus, ErrorClass, ExecutorKind, Limit, Profile, ThinkTime};
use rustman_core::bench::BenchSettings;

fn docs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs")
}

fn read(name: &str) -> String {
    fs::read_to_string(docs().join(name)).unwrap()
}

/// The targets of the Markdown links of `text` that are not URLs or
/// anchors, without their fragment.
fn relative_links(text: &str) -> Vec<String> {
    text.match_indices("](")
        .filter_map(|(at, _)| {
            let rest = &text[at + 2..];
            let target = &rest[..rest.find(')')?];
            let path = target.split('#').next().unwrap_or_default();
            let external = target.starts_with("http") || target.starts_with('#');
            (!external && !path.is_empty()).then(|| path.to_owned())
        })
        .collect()
}

#[test]
fn the_pages_are_in_the_index_and_their_links_resolve() {
    let index = read("README.md");
    for page in ["bench.md", "bench-internals.md"] {
        assert!(
            index.contains(&format!("]({page})")),
            "{page} is not linked"
        );
        for link in relative_links(&read(page)) {
            assert!(docs().join(&link).exists(), "{page}: broken link {link}");
        }
    }
}

#[test]
fn the_cap_message_is_the_documented_one() {
    let profile = Profile::ConstantVus(ConstantVus {
        vus: 100,
        limit: Limit::Iterations(1),
        think: ThinkTime::None,
    });
    let message = check(&profile, &BenchSettings::default(), 1).unwrap_err();
    assert!(
        read("bench.md").contains(&format!("error: {message}")),
        "{message}"
    );
}

#[test]
fn every_executor_is_documented() {
    let doc = read("bench.md");
    for kind in ExecutorKind::ALL {
        assert!(doc.contains(&format!("### {kind}")), "{kind}");
    }
    for mode in [
        "`sequential`",
        "`per-vu`",
        "`random`",
        "`recycle`",
        "`stop`",
    ] {
        assert!(doc.contains(mode), "{mode}");
    }
}

/// The load-testing spec: the change's delta until it is archived, then
/// the living spec.
fn load_testing_spec() -> String {
    let openspec = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../openspec");
    let delta = openspec.join("changes/add-load-testing/specs/load-testing/spec.md");
    let living = openspec.join("specs/load-testing/spec.md");
    fs::read_to_string(delta)
        .or_else(|_| fs::read_to_string(living))
        .unwrap()
}

/// Every error class the code reports, `other` included, is one the spec's
/// Metrics requirement and `docs/bench.md` define.
#[test]
fn every_error_class_is_specified_and_documented() {
    let spec = load_testing_spec();
    let metrics = spec
        .lines()
        .find(|l| l.starts_with("- errors by class"))
        .expect("the Metrics requirement lists the error classes");
    let docs = read("bench.md");
    let documented = docs
        .lines()
        .find(|l| l.starts_with("Error classes:"))
        .expect("bench.md lists the error classes");
    for class in ErrorClass::ALL {
        let quoted = format!("`{}`", class.name());
        assert!(metrics.contains(&quoted), "spec misses {quoted}");
        assert!(documented.contains(&quoted), "bench.md misses {quoted}");
    }
}
