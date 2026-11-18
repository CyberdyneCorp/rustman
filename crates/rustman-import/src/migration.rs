//! The script migration report (`--migration-report`): one Markdown section
//! per request with unconverted statements, each with a suggested manual
//! equivalent or a plugin hint.

use std::sync::LazyLock;

use regex::Regex;

use crate::report::{Report, RequestMigration};
use crate::scripts::normalize;

/// The hint for a statement with no declarative equivalent.
pub const NO_EQUIVALENT: &str = "no declarative equivalent; consider a plugin";

/// `pm.expect(<base>.a.b.every(x => x.f > N)).to.be.true`.
static EVERY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^pm\.expect\((?:pm\.response\.json\(\)|[A-Za-z_$][\w$]*)((?:\.[A-Za-z_$][\w$]*)+)\.every\(\(?([A-Za-z_$][\w$]*)\)?=>([A-Za-z_$][\w$]*)((?:\.[A-Za-z_$][\w$]*)+)(>=|<=|===|==|>|<)(-?\d+(?:\.\d+)?)\)\)\.to\.be\.true;?$",
    )
    .expect("the every pattern is valid")
});

/// The Markdown report for `report`.
pub fn markdown(report: &Report) -> String {
    let m = &report.migration;
    let mut out = format!(
        "# Script migration report\n\nSource: `{}` ({}). Scripts were not executed; converted \
         statements became assertions and captures in the request files.\n\n{}",
        report.source.path,
        report.source.format,
        m.summary_line()
    );
    let pending: Vec<&RequestMigration> = m.requests.iter().filter(|r| r.unconverted > 0).collect();
    if pending.is_empty() {
        out.push_str("\nEvery script statement was converted.\n");
    }
    for request in pending {
        out.push_str(&section(request));
    }
    out
}

fn section(request: &RequestMigration) -> String {
    let mut out = format!(
        "\n## {}\n\n{} of {} statements unconverted.\n\n\
         | Statement | Request file | Suggestion |\n| --- | --- | --- |\n",
        request.request, request.unconverted, request.found
    );
    for statement in &request.statements {
        out.push_str(&format!(
            "| {} | {} | {} |\n",
            code(statement),
            code(&request.path),
            cell(&suggestion(statement))
        ));
    }
    out
}

/// A suggested manual equivalent for an unconverted statement.
pub fn suggestion(statement: &str) -> String {
    let code = normalize(statement);
    if let Some(schema) = every_schema(&code) {
        return schema;
    }
    if code.contains(".every(") {
        return "use a `schema` assertion to constrain every element of the array".to_owned();
    }
    if code.contains("setNextRequest(") {
        return format!(
            "{NO_EQUIVALENT}; for run order, list the requests under `order:` in `_collection.yaml`"
        );
    }
    NO_EQUIVALENT.to_owned()
}

/// A `schema` assertion equivalent to an `every(...)` comparison.
fn every_schema(code: &str) -> Option<String> {
    let m = EVERY.captures(code)?;
    if m[2] != m[3] {
        return None;
    }
    let keyword = match &m[5] {
        ">" => "exclusiveMinimum",
        ">=" => "minimum",
        "<" => "exclusiveMaximum",
        "<=" => "maximum",
        _ => "const",
    };
    let constraint = format!("{{{keyword}: {}}}", &m[6]);
    let field: Vec<&str> = m[4].trim_start_matches('.').split('.').collect();
    let array: Vec<&str> = m[1].trim_start_matches('.').split('.').collect();
    let element = nest(&field, constraint.clone());
    let schema = nest(&array, format!("{{type: array, items: {element}}}"));
    Some(format!(
        "use a `schema` assertion with `{}.{}` constrained by `{keyword}: {}`: \
         `schema: {{inline: {schema}}}`",
        array.join("."),
        field.join("."),
        &m[6]
    ))
}

/// `{type: object, properties: {a: {type: object, properties: {b: inner}}}}`.
fn nest(path: &[&str], inner: String) -> String {
    path.iter().rev().fold(inner, |acc, key| {
        format!("{{type: object, properties: {{{key}: {acc}}}}}")
    })
}

/// `text` as an inline code span inside a table cell.
fn code(text: &str) -> String {
    let flat = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('|', "\\|");
    if flat.contains('`') {
        format!("`` {flat} ``")
    } else {
        format!("`{flat}`")
    }
}

fn cell(text: &str) -> String {
    text.replace('|', "\\|")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_comparisons_suggest_a_schema() {
        assert_eq!(
            suggestion("pm.expect(pm.response.json().items.every(i => i.price > 0)).to.be.true;"),
            "use a `schema` assertion with `items.price` constrained by `exclusiveMinimum: 0`: \
             `schema: {inline: {type: object, properties: {items: {type: array, items: \
             {type: object, properties: {price: {exclusiveMinimum: 0}}}}}}}`"
        );
        assert!(
            suggestion("pm.expect(b.data.list.every((x) => x.meta.n <= 5)).to.be.true")
                .contains("`data.list.meta.n` constrained by `maximum: 5`")
        );
        assert!(
            suggestion("pm.expect(b.items.every(i => i.tags.length)).to.be.true")
                .starts_with("use a `schema` assertion")
        );
    }

    #[test]
    fn other_statements_get_the_plugin_hint() {
        assert_eq!(
            suggestion("postman.setNextRequest(\"checkout\");"),
            "no declarative equivalent; consider a plugin; for run order, list the requests \
             under `order:` in `_collection.yaml`"
        );
        assert_eq!(
            suggestion("pm.environment.set(\"ts\", Date.now())"),
            NO_EQUIVALENT
        );
    }

    #[test]
    fn cells_escape_pipes_and_backticks() {
        assert_eq!(code("a ||\n  b"), "`a \\|\\| b`");
        assert_eq!(code("x = `t`"), "`` x = `t` ``");
    }
}
