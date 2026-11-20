//! The command-line definition: its help output (snapshots) and its
//! coverage in `docs/cli.md`.

use clap::CommandFactory;

use super::Cli;

/// Every command, as its full name (`rustman env list`), with its help.
fn commands() -> Vec<(String, String)> {
    let mut root = Cli::command();
    root.build();
    let mut out = Vec::new();
    collect(&root, "rustman", &mut out);
    out
}

fn collect(command: &clap::Command, path: &str, out: &mut Vec<(String, String)>) {
    out.push((
        path.to_owned(),
        command.clone().render_long_help().to_string(),
    ));
    for sub in command.get_subcommands().filter(|s| s.get_name() != "help") {
        collect(sub, &format!("{path} {}", sub.get_name()), out);
    }
}

#[test]
fn cli_definition_is_valid() {
    Cli::command().debug_assert();
}

#[test]
fn help_output_snapshots() {
    for (name, help) in commands() {
        let snapshot = name.replace(' ', "_");
        insta::assert_snapshot!(snapshot, help);
    }
}

/// Whether `doc` names the command `name` itself, in a code span: followed
/// by the closing backtick or by a word that is not one of `subcommands`
/// (an argument or a flag). `rustman env list` does not name `rustman env`,
/// and `rustman history listing` would not name `rustman history list`.
fn documents(doc: &str, name: &str, subcommands: &[String]) -> bool {
    let start = format!("`{name}");
    doc.match_indices(&start).any(|(at, _)| {
        let rest = &doc[at + start.len()..];
        match rest.chars().next() {
            None | Some('`') => true,
            Some(' ') => {
                let word = rest[1..].split([' ', '`']).next().unwrap_or_default();
                !subcommands.iter().any(|s| s == word)
            }
            Some(_) => false,
        }
    })
}

/// The subcommand names of the command `name` (`rustman env` has `list`).
fn subcommands_of(name: &str, all: &[(String, String)]) -> Vec<String> {
    let prefix = format!("{name} ");
    all.iter()
        .filter_map(|(n, _)| n.strip_prefix(&prefix))
        .filter(|rest| !rest.contains(' '))
        .map(str::to_owned)
        .collect()
}

#[test]
fn every_subcommand_is_in_the_cli_reference() {
    let doc = include_str!("../../../docs/cli.md");
    let all = commands();
    let missing: Vec<&String> = all
        .iter()
        .map(|(name, _)| name)
        .filter(|name| !documents(doc, name, &subcommands_of(name, &all)))
        .collect();
    assert!(missing.is_empty(), "missing from docs/cli.md: {missing:?}");
}

#[test]
fn a_command_is_documented_only_by_its_own_name() {
    let subs = ["list".to_owned()];
    assert!(!documents("`rustman env list`", "rustman env", &subs));
    assert!(documents(
        "`rustman env` and `rustman env list`",
        "rustman env",
        &subs
    ));
    assert!(documents("`rustman env --help`", "rustman env", &subs));
    assert!(documents("`rustman env list`", "rustman env list", &[]));
    assert!(!documents("`rustman env listing`", "rustman env list", &[]));
    assert!(documents(
        "`rustman history show <id>`",
        "rustman history show",
        &[]
    ));
    assert!(!documents("`rustman sendx`", "rustman send", &[]));
}

/// The completion script offers `rpc` and its subcommands with their flags.
#[test]
fn completion_snapshot_covers_rpc() {
    let mut script = Vec::new();
    let shell = clap_complete::Shell::Zsh;
    clap_complete::generate(shell, &mut Cli::command(), "rustman", &mut script);
    let script = String::from_utf8(script).unwrap();
    let start = script
        .find("(rpc)")
        .expect("the zsh script completes `rustman rpc`");
    let end = script[start..]
        .find("\n(completions)")
        .map_or(script.len(), |n| start + n);
    insta::assert_snapshot!("completions_zsh_rpc", &script[start..end]);
}

/// The fenced code blocks of `doc`, with their language.
fn code_blocks(doc: &str) -> Vec<(String, String)> {
    let mut blocks = Vec::new();
    let mut open: Option<(String, String)> = None;
    for line in doc.lines() {
        match (&mut open, line.strip_prefix("```")) {
            (None, Some(lang)) => open = Some((lang.trim().to_owned(), String::new())),
            (Some(_), Some(_)) => blocks.extend(open.take()),
            (Some((_, text)), None) => {
                text.push_str(line);
                text.push('\n');
            }
            (None, None) => {}
        }
    }
    blocks
}

/// add-stress-testing task 6.7: every `rustman stress` command in
/// `docs/stress.md` parses, and every TOML example is a valid
/// `rustman.toml` with no unknown key.
#[test]
fn every_stress_doc_example_parses() {
    use clap::Parser;
    let doc = include_str!("../../../docs/stress.md");
    let mut commands = 0;
    let mut tables = 0;
    for (lang, text) in code_blocks(doc) {
        if lang == "toml" {
            let toml = format!("[workspace]\nname = \"t\"\nversion = 1\n{text}");
            let path = std::path::Path::new("rustman.toml");
            let (_, warnings) = rustman_core::settings::Settings::parse(path, &toml)
                .unwrap_or_else(|e| panic!("{e}\n{text}"));
            assert!(warnings.is_empty(), "{warnings:?}");
            tables += 1;
            continue;
        }
        for line in text.lines() {
            let Some(at) = line.find("rustman stress ") else {
                continue;
            };
            let words = shell_words::split(&line[at..]).unwrap();
            if let Err(e) = Cli::try_parse_from(&words) {
                panic!("{line}\n{e}");
            }
            commands += 1;
        }
    }
    assert!(commands >= 4, "{commands} commands");
    assert_eq!(tables, 1);
}
