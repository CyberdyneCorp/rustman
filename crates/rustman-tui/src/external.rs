//! Editing a body in an external editor (design D9, spec: External editor
//! for bodies): the text goes to a temp file named for its content type,
//! `$VISUAL`, else `$EDITOR`, else `vi` (`notepad` on Windows) runs on it,
//! and the file's contents come back. The temp file is always deleted.
//!
//! The variable's value is split POSIX-style on Unix (`code --wait`). On
//! Windows a value naming an existing file is the program as is, and
//! anything else is split with the Windows command-line rules, so
//! `C:\Windows\System32\notepad.exe` keeps its backslashes. When the
//! configured editor cannot be started, the fallback editor runs instead
//! and [`Edited::fallback`] says why.
//!
//! Suspending and restoring the terminal around the editor is the caller's
//! job (see `runtime`).

use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, ExitStatus};

/// The editor used when neither `VISUAL` nor `EDITOR` is set, or when the
/// configured one cannot be started.
pub const FALLBACK: &str = if cfg!(windows) { "notepad" } else { "vi" };

/// The text the editor saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edited {
    pub text: String,
    /// Set when the configured editor could not be started and
    /// [`FALLBACK`] was used: why, for a warning toast.
    pub fallback: Option<String>,
}

/// The editor command and its arguments, from `var` (`VISUAL`, then
/// `EDITOR`), split by [`split`] with this platform's rules.
pub fn command(var: impl Fn(&str) -> Option<String>) -> Vec<String> {
    let set = |name: &str| var(name).filter(|v| !v.trim().is_empty());
    let raw = set("VISUAL")
        .or_else(|| set("EDITOR"))
        .unwrap_or_else(|| FALLBACK.to_owned());
    split(&raw, cfg!(windows), |path| path.is_file())
}

/// Split an editor value into program and arguments. With `windows`, a
/// value that (trimmed, surrounding quotes removed) names a file per
/// `is_file` is the program with no arguments; otherwise the value is
/// split by [`split_windows`]. Without `windows` it is split like a POSIX
/// shell would.
pub fn split(raw: &str, windows: bool, is_file: impl Fn(&Path) -> bool) -> Vec<String> {
    let words = if windows {
        let whole = unquote(raw.trim());
        if is_file(Path::new(whole)) {
            return vec![whole.to_owned()];
        }
        split_windows(raw)
    } else {
        shell_words::split(raw).unwrap_or_else(|_| vec![raw.to_owned()])
    };
    if words.is_empty() {
        vec![raw.trim().to_owned()]
    } else {
        words
    }
}

/// `value` without one pair of surrounding double or single quotes.
fn unquote(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|v| v.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
}

/// Split a command line with the Windows (`CommandLineToArgvW`) rules:
/// whitespace separates words outside double quotes, double quotes group,
/// backslashes are literal unless they come before a double quote (then
/// each pair is one backslash and an odd one escapes the quote), and `""`
/// inside quotes is a literal quote.
pub fn split_windows(raw: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quoted = false;
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let mut count = 1;
                while chars.next_if_eq(&'\\').is_some() {
                    count += 1;
                }
                if chars.peek() == Some(&'"') {
                    word.extend(std::iter::repeat_n('\\', count / 2));
                    if count % 2 == 1 {
                        word.push('"');
                        chars.next();
                    }
                } else {
                    word.extend(std::iter::repeat_n('\\', count));
                }
                in_word = true;
            }
            '"' if quoted && chars.next_if_eq(&'"').is_some() => word.push('"'),
            '"' => {
                quoted = !quoted;
                in_word = true;
            }
            c if c.is_whitespace() && !quoted => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            c => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(word);
    }
    words
}

/// Edit `text` in the editor `var` names, with a temp file in the system
/// temp folder ending in `.extension`.
pub fn edit(
    text: &str,
    extension: &str,
    var: impl Fn(&str) -> Option<String>,
) -> io::Result<Edited> {
    edit_in(
        &std::env::temp_dir(),
        text,
        extension,
        &command(var),
        FALLBACK,
    )
}

/// Edit `text` with the editor `argv` (falling back to `fallback` when it
/// cannot be started), with the temp file in `dir`.
pub fn edit_in(
    dir: &Path,
    text: &str,
    extension: &str,
    argv: &[String],
    fallback: &str,
) -> io::Result<Edited> {
    let mut file = tempfile::Builder::new()
        .prefix("rustman-body-")
        .suffix(&format!(".{extension}"))
        .tempfile_in(dir)?;
    file.write_all(text.as_bytes())?;
    file.flush()?;
    // Closed before the editor runs: Windows editors cannot save a file
    // another process holds open. The path deletes the file when dropped.
    let path = file.into_temp_path();
    let (program, args) = argv.split_first().expect("command is never empty");
    let mut notice = None;
    let mut ran = program.as_str();
    let status = match run(program, args, &path) {
        Err(err) if program != fallback => {
            notice = Some(format!("cannot run {program} ({err}); opened {fallback}"));
            ran = fallback;
            run(fallback, &[], &path).map_err(|e| {
                io::Error::new(e.kind(), format!("cannot run {program} or {fallback}: {e}"))
            })?
        }
        other => {
            other.map_err(|e| io::Error::new(e.kind(), format!("cannot run {program}: {e}")))?
        }
    };
    if !status.success() {
        return Err(io::Error::other(format!("{ran} exited with {status}")));
    }
    let edited = std::fs::read_to_string(&path)?;
    path.close()?;
    Ok(Edited {
        text: edited,
        fallback: notice,
    })
}

/// Run `program args path` and wait for it.
fn run(program: &str, args: &[String], path: &Path) -> io::Result<ExitStatus> {
    Command::new(program).args(args).arg(path).status()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_owned()).collect()
    }

    #[test]
    fn visual_then_editor_then_the_fallback() {
        assert_eq!(
            command(vars(&[("VISUAL", "nvim"), ("EDITOR", "nano")])),
            ["nvim"]
        );
        assert_eq!(
            command(vars(&[("VISUAL", " "), ("EDITOR", "code --wait")])),
            ["code", "--wait"]
        );
        assert_eq!(command(vars(&[])), [FALLBACK]);
    }

    #[test]
    fn windows_paths_keep_their_backslashes() {
        // Regression: POSIX splitting turned this into
        // `C:WindowsSystem32notepad.exe`.
        let none = |_: &Path| false;
        assert_eq!(
            split(r"C:\Windows\System32\notepad.exe", true, none),
            [r"C:\Windows\System32\notepad.exe"]
        );
        assert_eq!(
            split(r#""C:\Program Files\Editor\ed.exe" --wait -n"#, true, none),
            [r"C:\Program Files\Editor\ed.exe", "--wait", "-n"]
        );
        assert_eq!(split("code --wait", true, none), ["code", "--wait"]);
    }

    #[test]
    fn a_windows_value_naming_a_file_is_the_whole_program() {
        let path = r"C:\Program Files\My Editor\ed.exe";
        let exists = |p: &Path| p == Path::new(path);
        assert_eq!(split(path, true, exists), [path]);
        assert_eq!(split(&format!(" \"{path}\" "), true, exists), [path]);
    }

    #[test]
    fn windows_splitting_follows_the_command_line_rules() {
        assert_eq!(split_windows(r#"a\\\"b "c d"  e"#), [r#"a\"b"#, "c d", "e"]);
        assert_eq!(split_windows(r#"x\\"y z""#), [r"x\y z"]);
        assert_eq!(split_windows(r#""a""b""#), [r#"a"b"#]);
        assert_eq!(split_windows(r#""" next"#), ["", "next"]);
        assert!(split_windows("   ").is_empty());
    }

    #[test]
    fn unix_splitting_stays_posix() {
        let none = |_: &Path| false;
        assert_eq!(
            split("'my editor' --wait", false, none),
            ["my editor", "--wait"]
        );
        assert_eq!(split(r"a\ b", false, none), ["a b"]);
    }

    #[test]
    fn the_editor_rewrites_the_file_and_the_temp_file_is_deleted() {
        let tools = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let editor = crate::testing::fake_editor(tools.path());
        let edited = edit_in(scratch.path(), "{}", "json", &[editor], FALLBACK).unwrap();
        assert_eq!(edited.text.trim_end(), "{\"edited\":true}");
        assert_eq!(edited.fallback, None);
        let left: Vec<_> = std::fs::read_dir(scratch.path()).unwrap().collect();
        assert!(left.is_empty(), "temp file left behind: {left:?}");
    }

    #[test]
    fn an_editor_that_cannot_start_falls_back_with_a_notice() {
        let tools = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let fallback = crate::testing::fake_editor(tools.path());
        let missing = argv(&["rustman-no-such-editor", "--wait"]);
        let edited = edit_in(scratch.path(), "x", "txt", &missing, &fallback).unwrap();
        assert_eq!(edited.text.trim_end(), "{\"edited\":true}");
        let notice = edited.fallback.expect("a fallback notice");
        assert!(notice.contains("rustman-no-such-editor"), "{notice}");
        assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_failing_editor_and_fallback_is_an_error_and_cleans_up() {
        let scratch = tempfile::tempdir().unwrap();
        let missing = argv(&["rustman-no-such-editor"]);
        let err = edit_in(scratch.path(), "x", "txt", &missing, "rustman-no-fallback").unwrap_err();
        assert!(err.to_string().contains("rustman-no-such-editor"), "{err}");
        assert!(err.to_string().contains("rustman-no-fallback"), "{err}");
        assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
    }
}
