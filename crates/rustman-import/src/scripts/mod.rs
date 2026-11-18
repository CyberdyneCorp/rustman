//! Scripts (design D3, task 2.3, script migration): never executed. Their
//! source is kept as an `# imported-script` comment block; statements in
//! the mapping table become declarative assertions and captures, and every
//! statement is counted for the migration summary.

mod patterns;
mod split;

use rustman_core::Redactor;

pub use patterns::{Alias, Outcome, classify};
pub use split::{normalize, split};

use crate::ir::{
    CaptureSpec, Check, Folder, ImportedCollection, Item, Location, Note, NoteKind, Script,
    ScriptStatement,
};

/// Suggest checks and captures from every request's scripts, record their
/// statements, and note each script as preserved but not executed.
pub fn suggest(collection: &mut ImportedCollection) {
    let mut notes = Vec::new();
    suggest_folder(&mut collection.root, &mut notes);
    collection.notes.extend(notes);
}

fn suggest_folder(folder: &mut Folder, notes: &mut Vec<Note>) {
    note_scripts(&folder.scripts, notes);
    for item in &mut folder.items {
        match item {
            Item::Folder(child) => suggest_folder(child, notes),
            Item::Request(request) => {
                note_scripts(&request.scripts, notes);
                for script in &request.scripts {
                    let found = analyze(&script.source);
                    push_new(&mut request.checks, found.checks);
                    push_new(&mut request.captures, found.captures);
                    request.statements.extend(found.statements);
                }
            }
        }
    }
}

fn note_scripts(scripts: &[Script], notes: &mut Vec<Note>) {
    for script in scripts {
        let message = format!("script not executed: {}", script.event);
        notes.push(Note::new(NoteKind::Script, message, &script.location));
    }
}

fn push_new<T: PartialEq>(into: &mut Vec<T>, found: Vec<T>) {
    for item in found {
        if !into.contains(&item) {
            into.push(item);
        }
    }
}

/// What a script maps to: declarative entries and its statements.
#[derive(Debug, Default, PartialEq)]
pub struct Analysis {
    pub checks: Vec<Check>,
    pub captures: Vec<CaptureSpec>,
    pub statements: Vec<ScriptStatement>,
}

/// Map the statements of `source` through the mapping table.
pub fn analyze(source: &str) -> Analysis {
    let mut found = Analysis::default();
    let mut aliases = Vec::new();
    for statement in split(source) {
        let outcome = classify(&normalize(&statement), &aliases);
        let mapped = match outcome {
            Outcome::Wrapper => continue,
            Outcome::Unmapped => false,
            Outcome::Alias(alias) => {
                aliases.push(alias);
                true
            }
            Outcome::Check(check) => {
                push_new(&mut found.checks, vec![check]);
                true
            }
            Outcome::Capture(capture) => {
                push_new(&mut found.captures, vec![capture]);
                true
            }
        };
        found.statements.push(ScriptStatement {
            text: statement,
            mapped,
        });
    }
    found
}

/// The `# imported-script` comment blocks for `scripts`, with any extracted
/// secret value redacted.
pub fn comment_block(scripts: &[Script], redactor: &Redactor) -> String {
    let mut out = String::new();
    for script in scripts {
        out.push_str(&format!(
            "# imported-script: {} (not executed)\n",
            script.event
        ));
        let source = redactor.text(&script.source);
        for line in source.lines() {
            let line = line.trim_end_matches('\r');
            if line.is_empty() {
                out.push_str("#\n");
            } else {
                out.push_str(&format!("# {line}\n"));
            }
        }
    }
    out
}

/// The scripts of the `# imported-script` comment blocks at the top of a
/// written file (the inverse of [`comment_block`]), located at `at`.
pub fn parse_comment_block(text: &str, at: &Location) -> Vec<Script> {
    let mut blocks: Vec<(String, Vec<&str>)> = Vec::new();
    for line in text.lines().map(|l| l.trim_end_matches('\r')) {
        let header = line
            .strip_prefix("# imported-script: ")
            .and_then(|rest| rest.strip_suffix(" (not executed)"));
        match (header, line.strip_prefix('#'), blocks.last_mut()) {
            (Some(event), _, _) => blocks.push((event.to_owned(), Vec::new())),
            (None, Some(code), Some((_, lines))) => {
                lines.push(code.strip_prefix(' ').unwrap_or(code));
            }
            _ => break,
        }
    }
    blocks
        .into_iter()
        .map(|(event, lines)| Script {
            event,
            source: lines.join("\n"),
            location: at.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{CaptureFrom, JsonOp, Location};
    use rustman_core::Secret;

    fn checks(source: &str) -> Vec<Check> {
        analyze(source).checks
    }

    fn json(path: &str, op: JsonOp, value: &str) -> Check {
        Check::Json {
            path: path.into(),
            op,
            value: value.into(),
        }
    }

    #[test]
    fn status_checks_are_mapped() {
        let s = checks(r#"pm.test("ok", () => pm.response.to.have.status(201));"#);
        assert_eq!(s, [Check::Status(201)]);
        let s = checks(
            "pm.expect(pm.response.code).to.eql(201);\npm.expect( pm.response.code ).to.equal(204)",
        );
        assert_eq!(s, [Check::Status(201), Check::Status(204)]);
        assert_eq!(
            checks("pm.test('ok', function () { pm.response.to.be.ok; });"),
            [Check::StatusClass(2)]
        );
    }

    #[test]
    fn legacy_tests_assignments_are_mapped() {
        let source = r#"
            tests["Status code is 200"] = responseCode.code === 200;
            tests['has it'] = responseBody.has("authenticated");
            var body = JSON.parse(responseBody);
            tests["pass"] = body["status"] == "pass"
            tests["count"] = body.args.n === 3;
            var args = JSON.parse(responseBody).args;
            tests["val"] = args.val === "alpha";
            tests["Success"] = _.parseInt(postman.getEnvironmentVariable("count")) === 4
        "#;
        let found = analyze(source);
        assert_eq!(
            found.checks,
            [
                Check::Status(200),
                Check::BodyContains("authenticated".into()),
                json("$[\"status\"]", JsonOp::Equals, "\"pass\""),
                json("$.args.n", JsonOp::Equals, "3"),
                json("$.args.val", JsonOp::Equals, "\"alpha\""),
            ]
        );
        assert_eq!(found.statements.iter().filter(|s| s.mapped).count(), 7);
    }

    #[test]
    fn header_checks_are_mapped() {
        let s = checks(r#"pm.response.to.have.header("Content-Type");"#);
        assert_eq!(s, [Check::HeaderExists("Content-Type".into())]);
        let s = checks(r#"pm.response.to.have.header("Content-Type", "application/json");"#);
        assert_eq!(
            s,
            [Check::HeaderEquals {
                name: "Content-Type".into(),
                value: "application/json".into()
            }]
        );
    }

    #[test]
    fn response_time_and_text_checks_are_mapped() {
        let s = checks(
            r#"pm.test("fast", () => pm.expect(pm.response.responseTime).to.be.below(300));"#,
        );
        assert_eq!(s, [Check::LatencyBelow(300)]);
        let s = checks(r#"pm.expect(pm.response.text()).to.include('it\'s "done"');"#);
        assert_eq!(s, [Check::BodyContains("it's \"done\"".into())]);
    }

    #[test]
    fn json_fields_of_tracked_aliases_are_mapped() {
        let source = r#"
            const body = pm.response.json();
            let d = pm.response.json()
            pm.expect(body.user.id).to.eql(42);
            pm.expect(d.items[0].tags).to.have.lengthOf(2);
            pm.expect(d.items[0]["sku code"]).to.equal('A-1');
            pm.expect(body.user).to.exist;
            pm.expect(body.user.name).to.be.a("string");
            pm.expect(body.total).to.be.above(10);
            pm.expect(body.total).to.be.below(99.5);
            pm.expect(pm.response.json().ok).to.eql(true);
            pm.expect(other.user.id).to.eql(42);
        "#;
        let found = analyze(source);
        assert_eq!(
            found.checks,
            [
                json("$.user.id", JsonOp::Equals, "42"),
                json("$.items[0].tags", JsonOp::Length, "2"),
                json("$.items[0][\"sku code\"]", JsonOp::Equals, "\"A-1\""),
                json("$.user", JsonOp::Exists, ""),
                json("$.user.name", JsonOp::Type, "\"string\""),
                json("$.total", JsonOp::Gt, "10"),
                json("$.total", JsonOp::Lt, "99.5"),
                json("$.ok", JsonOp::Equals, "true"),
            ]
        );
        let unmapped: Vec<&str> = found
            .statements
            .iter()
            .filter(|s| !s.mapped)
            .map(|s| s.text.as_str())
            .collect();
        assert_eq!(unmapped, ["pm.expect(other.user.id).to.eql(42)"]);
    }

    #[test]
    fn variable_setters_become_captures() {
        let source = r#"
            const b = pm.response.json();
            pm.environment.set("token", pm.response.json().data.token);
            pm.environment.set("etag", pm.response.headers.get("ETag"));
            pm.collectionVariables.set("orderId", b.order.id);
            pm.globals.set("session", pm.cookies.get("sid"));
            pm.variables.set("first", b.items[0].id);
        "#;
        let capture = |var: &str, from: CaptureFrom, persist: bool| CaptureSpec {
            var: var.into(),
            from,
            persist,
        };
        assert_eq!(
            analyze(source).captures,
            [
                capture("token", CaptureFrom::Json("$.data.token".into()), true),
                capture("etag", CaptureFrom::Header("ETag".into()), true),
                capture("orderId", CaptureFrom::Json("$.order.id".into()), false),
                capture("session", CaptureFrom::Cookie("sid".into()), true),
                capture("first", CaptureFrom::Json("$.items[0].id".into()), false),
            ]
        );
    }

    #[test]
    fn everything_else_stays_comment_only() {
        for source in [
            r#"pm.environment.set("ts", Date.now());"#,
            "// pm.response.to.have.status(200);",
            "if (x) { postman.setNextRequest('a'); }",
            "pm.expect(pm.response.json().items.every(i => i.price > 0)).to.be.true;",
            r#"pm.expect(body.name).to.be.a("Date");"#,
        ] {
            let found = analyze(source);
            assert!(found.checks.is_empty(), "{source}");
            assert!(found.captures.is_empty(), "{source}");
            assert!(found.statements.iter().all(|s| !s.mapped), "{source}");
        }
    }

    #[test]
    fn statements_are_counted_without_wrappers() {
        let found = analyze(
            "pm.test(\"ok\", function () {\n  pm.response.to.have.status(200);\n  postman.setNextRequest(null);\n});",
        );
        let texts: Vec<(&str, bool)> = found
            .statements
            .iter()
            .map(|s| (s.text.as_str(), s.mapped))
            .collect();
        assert_eq!(
            texts,
            [
                ("pm.response.to.have.status(200)", true),
                ("postman.setNextRequest(null)", false)
            ]
        );
    }

    #[test]
    fn comment_blocks_parse_back_into_scripts() {
        let scripts = vec![
            Script {
                event: "prerequest".into(),
                source: "let a = 1;\n\n  pm.x(a);".into(),
                location: Location::default(),
            },
            Script {
                event: "test".into(),
                source: "pm.response.to.be.ok;".into(),
                location: Location::default(),
            },
        ];
        let text = format!(
            "{}version: 1\n# not a script\n",
            comment_block(&scripts, &Redactor::new())
        );
        assert_eq!(parse_comment_block(&text, &Location::default()), scripts);
        assert!(parse_comment_block("# other\nversion: 1\n", &Location::default()).is_empty());
    }

    #[test]
    fn comment_blocks_keep_the_source_and_redact_secrets() {
        let script = Script {
            event: "test".into(),
            source: "var t = \"s3cret-value\";\r\n\r\npm.environment.set(\"token\", t);".into(),
            location: Location::default(),
        };
        let redactor = Redactor::new().with(&Secret::new("token", "s3cret-value".to_owned()));
        assert_eq!(
            comment_block(&[script], &redactor),
            "# imported-script: test (not executed)\n\
             # var t = \"«redacted:token»\";\n\
             #\n\
             # pm.environment.set(\"token\", t);\n"
        );
    }
}
