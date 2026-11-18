use assert_cmd::Command;
use predicates::prelude::*;

fn rustman() -> Command {
    Command::cargo_bin("rustman").expect("rustman binary is built")
}

#[test]
fn version_prints_package_version() {
    rustman()
        .arg("--version")
        .assert()
        .success()
        .stdout(format!("rustman {}\n", env!("CARGO_PKG_VERSION")));
}

#[test]
fn help_describes_the_tool() {
    rustman()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage: rustman"))
        .stdout(predicate::str::contains("--version"));
}

#[test]
fn no_arguments_prints_help() {
    rustman()
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage: rustman"));
}

#[test]
fn unknown_flag_is_a_usage_error() {
    rustman().arg("--nope").assert().code(2);
}

mod init {
    use super::rustman;
    use predicates::prelude::*;
    use std::fs;
    use std::path::Path;

    fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
        let mut files = Vec::new();
        collect(dir, dir, &mut files);
        files.sort();
        files
    }

    fn collect(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                collect(root, &path, out);
            } else {
                let rel = path.strip_prefix(root).unwrap().display().to_string();
                out.push((rel, fs::read(&path).unwrap()));
            }
        }
    }

    #[test]
    fn fresh_init_creates_the_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        rustman()
            .current_dir(tmp.path())
            .arg("init")
            .assert()
            .success()
            .stdout(predicate::str::contains(
                "initialized rustman workspace in .",
            ));
        for file in [
            "rustman.toml",
            "environments/local.yaml",
            "examples/ping.rman.yaml",
        ] {
            assert!(tmp.path().join(file).is_file(), "{file} missing");
        }
        let gitignore = fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
        assert!(gitignore.lines().any(|l| l == ".rustman/"));
        rustman_core::Workspace::load(tmp.path()).unwrap();
    }

    #[test]
    fn init_into_a_new_directory() {
        let tmp = tempfile::tempdir().unwrap();
        rustman()
            .current_dir(tmp.path())
            .args(["init", "shop"])
            .assert()
            .success();
        let settings = fs::read_to_string(tmp.path().join("shop/rustman.toml")).unwrap();
        assert!(settings.contains("name = \"shop\""), "{settings}");
    }

    #[test]
    fn existing_gitignore_is_appended() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join(".gitignore"), "target/\n").unwrap();
        rustman()
            .current_dir(tmp.path())
            .arg("init")
            .assert()
            .success();
        let gitignore = fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
        assert_eq!(gitignore, "target/\n.rustman/\n");
    }

    #[test]
    fn existing_gitignore_entry_is_not_duplicated() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join(".gitignore"), ".rustman/\n").unwrap();
        rustman()
            .current_dir(tmp.path())
            .arg("init")
            .assert()
            .success();
        assert_eq!(
            fs::read_to_string(tmp.path().join(".gitignore")).unwrap(),
            ".rustman/\n"
        );
    }

    #[test]
    fn bare_skips_sample_files() {
        let tmp = tempfile::tempdir().unwrap();
        rustman()
            .current_dir(tmp.path())
            .args(["init", "--bare"])
            .assert()
            .success();
        assert!(tmp.path().join("rustman.toml").is_file());
        assert!(tmp.path().join(".gitignore").is_file());
        assert!(!tmp.path().join("examples").exists());
        assert!(!tmp.path().join("environments").exists());
    }

    #[test]
    fn re_init_is_refused_without_touching_files() {
        let tmp = tempfile::tempdir().unwrap();
        rustman()
            .current_dir(tmp.path())
            .arg("init")
            .assert()
            .success();
        fs::write(tmp.path().join(".gitignore"), "target/\n").unwrap();
        let before = snapshot(tmp.path());
        rustman()
            .current_dir(tmp.path())
            .arg("init")
            .assert()
            .code(2)
            .stdout("")
            .stderr("error: . is already a rustman workspace\n");
        assert_eq!(snapshot(tmp.path()), before);
    }
}

mod env {
    use super::rustman;
    use std::fs;
    use std::path::Path;

    fn workspace(default_env: Option<&str>) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let mut toml = String::from("[workspace]\nname = \"t\"\nversion = 1\n");
        if let Some(name) = default_env {
            toml.push_str(&format!("[defaults]\ndefault_environment = \"{name}\"\n"));
        }
        fs::write(tmp.path().join("rustman.toml"), toml).unwrap();
        let envs = tmp.path().join("environments");
        fs::create_dir(&envs).unwrap();
        for name in ["local", "prod", "staging"] {
            fs::write(envs.join(format!("{name}.yaml")), "variables: {}\n").unwrap();
        }
        tmp
    }

    fn env_list(dir: &Path) -> assert_cmd::Command {
        let mut cmd = rustman();
        cmd.current_dir(dir)
            .env_remove("RUSTMAN_ENV")
            .args(["env", "list"]);
        cmd
    }

    #[test]
    fn lists_environments_marking_the_default() {
        let ws = workspace(Some("local"));
        env_list(ws.path())
            .assert()
            .success()
            .stdout("* local\n  prod\n  staging\n")
            .stderr("");
    }

    #[test]
    fn no_active_environment_marks_nothing() {
        let ws = workspace(None);
        env_list(ws.path())
            .assert()
            .success()
            .stdout("  local\n  prod\n  staging\n");
    }

    #[test]
    fn rustman_env_beats_the_default() {
        let ws = workspace(Some("local"));
        env_list(ws.path())
            .env("RUSTMAN_ENV", "staging")
            .assert()
            .success()
            .stdout("  local\n  prod\n* staging\n");
    }

    #[test]
    fn env_flag_beats_rustman_env_and_the_default() {
        let ws = workspace(Some("local"));
        env_list(ws.path())
            .env("RUSTMAN_ENV", "staging")
            .args(["--env", "prod"])
            .assert()
            .success()
            .stdout("  local\n* prod\n  staging\n");
    }

    #[test]
    fn missing_environment_exits_2_listing_the_available_ones() {
        let ws = workspace(Some("local"));
        env_list(ws.path())
            .args(["--env", "qa"])
            .assert()
            .code(2)
            .stdout("")
            .stderr("error: environment 'qa' not found (available: local, prod, staging)\n");
        env_list(ws.path())
            .env("RUSTMAN_ENV", "qa")
            .assert()
            .code(2)
            .stderr("error: environment 'qa' not found (available: local, prod, staging)\n");
    }

    #[test]
    fn invalid_name_is_warned_about_and_skipped() {
        let ws = workspace(None);
        fs::write(ws.path().join("environments/my env.yaml"), "").unwrap();
        env_list(ws.path())
            .assert()
            .success()
            .stdout("  local\n  prod\n  staging\n")
            .stderr("warning: ignoring environment 'my env': invalid name\n");
    }

    #[test]
    fn works_from_a_nested_folder_and_with_workspace() {
        let ws = workspace(Some("prod"));
        let nested = ws.path().join("api/users");
        fs::create_dir_all(&nested).unwrap();
        env_list(&nested)
            .assert()
            .success()
            .stdout("  local\n* prod\n  staging\n");
        let elsewhere = tempfile::tempdir().unwrap();
        env_list(elsewhere.path())
            .arg("--workspace")
            .arg(ws.path())
            .assert()
            .success()
            .stdout("  local\n* prod\n  staging\n");
    }

    #[test]
    fn relative_workspace_with_parent_dirs_loads_request_files() {
        let tmp = tempfile::tempdir().unwrap();
        rustman()
            .current_dir(tmp.path())
            .arg("init")
            .assert()
            .success();
        let users = tmp.path().join("users");
        fs::create_dir_all(&users).unwrap();
        fs::copy(
            tmp.path().join("examples/ping.rman.yaml"),
            users.join("up.rman.yaml"),
        )
        .unwrap();
        for dir in ["..", "../users/.."] {
            env_list(&users)
                .args(["--workspace", dir])
                .assert()
                .success()
                .stdout("* local\n");
        }
    }

    #[test]
    fn outside_a_workspace_exits_2() {
        let tmp = tempfile::tempdir().unwrap();
        env_list(tmp.path())
            .assert()
            .code(2)
            .stderr("error: no rustman workspace found (run 'rustman init')\n");
    }

    #[test]
    fn fresh_init_has_local_active() {
        let tmp = tempfile::tempdir().unwrap();
        rustman()
            .current_dir(tmp.path())
            .arg("init")
            .assert()
            .success();
        env_list(tmp.path()).assert().success().stdout("* local\n");
    }
}

mod tui {
    use super::*;

    #[test]
    fn tui_help_prints_usage() {
        rustman()
            .args(["tui", "--help"])
            .assert()
            .success()
            .stdout(predicate::str::contains(
                "Usage: rustman tui [OPTIONS] [PATH]",
            ))
            .stdout(predicate::str::contains("--theme <NAME>"))
            .stdout(predicate::str::contains("--ascii"))
            .stdout(predicate::str::contains("--vim"));
    }

    #[test]
    fn tui_rejects_an_unknown_theme() {
        rustman()
            .args(["tui", "--theme", "neon"])
            .assert()
            .code(2)
            .stderr(predicate::str::contains("unknown theme `neon`"));
    }

    #[test]
    fn tui_without_a_terminal_is_a_usage_error() {
        let tmp = tempfile::tempdir().unwrap();
        rustman()
            .arg("tui")
            .arg(tmp.path())
            .assert()
            .code(2)
            .stderr(predicate::str::contains(
                "error: the terminal UI needs an interactive terminal",
            ));
    }

    #[test]
    fn tui_on_a_missing_directory_exits_2() {
        let tmp = tempfile::tempdir().unwrap();
        rustman()
            .arg("tui")
            .arg(tmp.path().join("missing"))
            .assert()
            .code(2)
            .stderr(predicate::str::contains("is not a directory"));
    }
}
