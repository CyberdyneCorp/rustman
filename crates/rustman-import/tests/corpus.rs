//! Script migration over a corpus of public Postman collections (task 6.6):
//! the conversion rate of each may not drop below its recorded baseline.

mod common;

use std::fs;

use common::{fixture, plan_fixture};

/// `name found mapped` lines; `#` starts a comment.
const BASELINE: &str = "postman/corpus/baseline.txt";

#[test]
fn conversion_rates_do_not_drop_below_the_baseline() {
    let mut current = String::new();
    let mut failures = Vec::new();
    let baseline = fixture(BASELINE);
    let entries: Vec<Vec<&str>> = baseline
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| l.split_whitespace().collect())
        .collect();
    for fields in &entries {
        let [name, found, mapped] = fields[..] else {
            panic!("bad baseline line: {fields:?}");
        };
        let m = plan_fixture(&format!("postman/corpus/{name}"))
            .report
            .migration;
        current.push_str(&format!("{name} {} {}\n", m.found, m.mapped));
        let (found, mapped): (usize, usize) = (found.parse().unwrap(), mapped.parse().unwrap());
        if m.found != found || m.mapped < mapped {
            failures.push(format!(
                "{name}: {}/{} statements converted; baseline {mapped}/{found}",
                m.mapped, m.found
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{failures:#?}\ncurrent counts (name found mapped):\n{current}"
    );
    let files = fs::read_dir(common::fixture_path("postman/corpus"))
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".postman_collection.json")
        })
        .count();
    assert_eq!(
        entries.len(),
        files,
        "every corpus collection has a baseline"
    );
    assert!(files >= 5);
}
