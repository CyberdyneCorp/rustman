//! Editing the `[plugins.<name>]` tables of `rustman.toml` as text, so
//! `plugin add` and `plugin remove` keep the rest of the file (comments,
//! order, line endings) as the user wrote it. Every edit is checked by
//! parsing the result: an edit that would not declare exactly what was
//! asked is refused and the file is left alone.

use crate::declare::{Declaration, parse_declarations};
use crate::error::LoadError;

/// `text` with the table of `name` added at the end. Refused when the
/// workspace already declares `name`.
pub fn add_table(text: &str, declaration: &Declaration) -> Result<String, LoadError> {
    let name = &declaration.name;
    if parse_declarations(text)?.iter().any(|d| &d.name == name) {
        let message =
            format!("already declared in rustman.toml (run `rustman plugin remove {name}` first)");
        return Err(LoadError::invalid(name, message));
    }
    let newline = newline_of(text);
    let mut out = text.to_owned();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push_str(newline);
    }
    if !out.is_empty() {
        out.push_str(newline);
    }
    out.push_str(&table(declaration).replace('\n', newline));
    let declared = parse_declarations(&out)?;
    match declared.iter().find(|d| &d.name == name) {
        Some(found) if found == declaration => Ok(out),
        _ => Err(not_editable(name)),
    }
}

/// `text` without the table of `name` (and its sub-tables such as
/// `[plugins.<name>.limits]`). Refused when `name` is not declared, or is
/// declared in a form other than its own table.
pub fn remove_table(text: &str, name: &str) -> Result<String, LoadError> {
    if !parse_declarations(text)?.iter().any(|d| d.name == name) {
        return Err(LoadError::invalid(name, "not declared in rustman.toml"));
    }
    let mut out = String::with_capacity(text.len());
    let mut inside = false;
    for line in text.split_inclusive('\n') {
        if let Some(key) = header_key(line) {
            inside = belongs_to(&key, name);
        }
        if !inside {
            out.push_str(line);
        }
    }
    let out = trim_blank_tail(out, newline_of(text));
    let still = parse_declarations(&out)?.iter().any(|d| d.name == name);
    if still {
        return Err(not_editable(name));
    }
    Ok(out)
}

/// The TOML table `plugin add` writes: no grants.
pub fn table(declaration: &Declaration) -> String {
    let quoted = |text: &str| toml::Value::String(text.to_owned()).to_string();
    let grants: Vec<String> = declaration.grants.iter().map(|g| quoted(g)).collect();
    format!(
        "[plugins.{}]\nsource = {}\nsha256 = {}\ngrants = [{}]\n",
        declaration.name,
        quoted(declaration.source.as_str()),
        quoted(&declaration.sha256),
        grants.join(", ")
    )
}

fn not_editable(name: &str) -> LoadError {
    let message = "is not declared as its own [plugins.".to_owned()
        + name
        + "] table; edit rustman.toml by hand";
    LoadError::invalid(name, message)
}

/// The dotted key of a `[table]` header line, without spaces or quotes;
/// `None` for any other line (including `[[array]]` headers).
fn header_key(line: &str) -> Option<String> {
    let line = line.trim();
    let inner = line.strip_prefix('[')?;
    if inner.starts_with('[') {
        return None;
    }
    let end = inner.find(']')?;
    let key: String = inner[..end]
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '"' && *c != '\'')
        .collect();
    Some(key)
}

fn belongs_to(key: &str, name: &str) -> bool {
    let own = format!("plugins.{name}");
    key == own || key.starts_with(&format!("{own}."))
}

fn newline_of(text: &str) -> &'static str {
    if text.contains("\r\n") { "\r\n" } else { "\n" }
}

/// `text` with at most one line ending at its end.
fn trim_blank_tail(text: String, newline: &str) -> String {
    let trimmed = text.trim_end_matches(['\n', '\r']);
    if trimmed.is_empty() {
        return String::new();
    }
    format!("{trimmed}{newline}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::declare::Source;
    use crate::limits::Limits;

    const PIN: &str = "ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12";

    fn declaration(name: &str) -> Declaration {
        Declaration {
            name: name.to_owned(),
            source: Source::Path(format!("plugins/{name}")),
            sha256: PIN.to_owned(),
            grants: Vec::new(),
            limits: Limits::default(),
        }
    }

    const BASE: &str = "# shop\n[workspace]\nname = \"shop\" # keep\nversion = 1\n";

    #[test]
    fn a_table_is_appended_and_the_rest_kept() {
        let out = add_table(BASE, &declaration("hmac")).unwrap();
        assert_eq!(
            out,
            format!(
                "{BASE}\n[plugins.hmac]\nsource = \"plugins/hmac\"\nsha256 = \"{PIN}\"\ngrants = []\n"
            )
        );
        let err = add_table(&out, &declaration("hmac")).unwrap_err();
        assert_eq!(
            err.to_string(),
            "plugin hmac: already declared in rustman.toml (run `rustman plugin remove hmac` first)"
        );
    }

    #[test]
    fn crlf_files_stay_crlf() {
        let base = BASE.replace('\n', "\r\n");
        let out = add_table(&base, &declaration("hmac")).unwrap();
        assert!(!out.replace("\r\n", "").contains('\n'), "{out:?}");
        let back = remove_table(&out, "hmac").unwrap();
        assert_eq!(back, base);
    }

    #[test]
    fn removing_drops_the_table_and_its_sub_tables() {
        let text = add_table(BASE, &declaration("faker")).unwrap();
        let text = add_table(&text, &declaration("hmac")).unwrap();
        let text = text.replace(
            "[plugins.hmac]",
            "[plugins.hmac.limits]\ncpu_ms = 200\n\n[ plugins . \"hmac\" ]",
        );
        let out = remove_table(&text, "hmac").unwrap();
        assert_eq!(out, add_table(BASE, &declaration("faker")).unwrap());
        assert_eq!(remove_table(&out, "faker").unwrap(), BASE);
        let err = remove_table(BASE, "hmac").unwrap_err();
        assert_eq!(err.to_string(), "plugin hmac: not declared in rustman.toml");
    }

    #[test]
    fn inline_declarations_are_left_to_the_user() {
        let text =
            format!("{BASE}[plugins]\nhmac = {{ source = \"p.wasm\", sha256 = \"{PIN}\" }}\n");
        let err = remove_table(&text, "hmac").unwrap_err();
        assert_eq!(
            err.to_string(),
            "plugin hmac: is not declared as its own [plugins.hmac] table; edit rustman.toml by hand"
        );
        let err = add_table(&text, &declaration("hmac")).unwrap_err();
        assert!(err.to_string().contains("already declared"), "{err}");
    }

    #[test]
    fn headers_are_recognized() {
        assert_eq!(
            header_key(" [plugins.a] # c\n").as_deref(),
            Some("plugins.a")
        );
        assert_eq!(header_key("[[bench.stages]]"), None);
        assert_eq!(header_key("name = 1"), None);
        assert!(belongs_to("plugins.a.limits", "a"));
        assert!(!belongs_to("plugins.ab", "a"));
    }
}
