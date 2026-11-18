//! The user's `keybindings.toml` (spec: Remappable keybindings).
//!
//! ```toml
//! [global]
//! "F5" = "request.send"
//! "Ctrl+R" = "none"      # unbind a default
//! ```
//!
//! Every problem (unknown context, unknown action id, unparsable key) is
//! reported as a warning and only that entry is skipped.

use std::path::Path;

use super::chord::{self, Keys};
use super::{ActionId, Context};

/// The name of the keybindings file in the config directory.
pub const FILE_NAME: &str = "keybindings.toml";

/// The value that removes a binding.
const UNBIND: &str = "none";

/// The valid entries of `keybindings.toml`, in file order. `None` unbinds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserLayer {
    pub bindings: Vec<(Context, Keys, Option<ActionId>)>,
}

impl UserLayer {
    /// Read `keybindings.toml` from `dir`. A missing file is an empty layer.
    pub fn load(dir: Option<&Path>) -> (Self, Vec<String>) {
        let Some(path) = dir.map(|d| d.join(FILE_NAME)) else {
            return (Self::default(), Vec::new());
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => (Self::default(), Vec::new()),
            Err(err) => (
                Self::default(),
                vec![format!("{} ignored: {err}", path.display())],
            ),
        }
    }

    /// Parse the file's text, keeping every valid entry.
    pub fn parse(text: &str) -> (Self, Vec<String>) {
        let mut layer = Self::default();
        let mut warnings = Vec::new();
        let table: toml::Table = match toml::from_str(text) {
            Ok(table) => table,
            Err(err) => {
                let first = err
                    .to_string()
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                return (layer, vec![format!("{FILE_NAME} ignored: {first}")]);
            }
        };
        for (name, value) in &table {
            let Some(context) = Context::from_name(name) else {
                warnings.push(format!(
                    "{FILE_NAME}: unknown context [{name}] skipped (use global, sidebar, editor or response)"
                ));
                continue;
            };
            let Some(entries) = value.as_table() else {
                warnings.push(format!("{FILE_NAME}: [{name}] must be a table"));
                continue;
            };
            let mut parsed = Vec::new();
            for (key, value) in entries {
                match entry(context, key, value) {
                    Ok(binding) => parsed.push((key.as_str(), binding)),
                    Err(message) => warnings.push(format!("{FILE_NAME}: {message}")),
                }
            }
            layer
                .bindings
                .extend(drop_same_keys(context, parsed, &mut warnings));
        }
        (layer, warnings)
    }
}

/// Entries that spell the same keys differently (`Ctrl+R` and `ctrl+r`)
/// conflict, and TOML gives them no order to settle it: all are skipped.
fn drop_same_keys(
    context: Context,
    parsed: Vec<(&str, Binding)>,
    warnings: &mut Vec<String>,
) -> Vec<Binding> {
    let mut kept = Vec::new();
    for (i, (_, binding)) in parsed.iter().enumerate() {
        let same: Vec<String> = parsed
            .iter()
            .filter(|(_, other)| other.1 == binding.1)
            .map(|(spelling, _)| format!("`{spelling}`"))
            .collect();
        let first = parsed.iter().position(|(_, b)| b.1 == binding.1) == Some(i);
        if same.len() == 1 {
            kept.push(binding.clone());
        } else if first {
            warnings.push(format!(
                "{FILE_NAME}: {} in [{context}] are the same key {}; all skipped",
                same.join(" and "),
                chord::display(&binding.1)
            ));
        }
    }
    kept
}

type Binding = (Context, Keys, Option<ActionId>);

fn entry(context: Context, key: &str, value: &toml::Value) -> Result<Binding, String> {
    let keys = chord::parse(key).map_err(|e| format!("invalid key `{key}` in [{context}]: {e}"))?;
    let id = value
        .as_str()
        .ok_or_else(|| format!("`{key}` in [{context}] must be an action id string"))?;
    if id == UNBIND {
        return Ok((context, keys, None));
    }
    let action = ActionId::from_id(id)
        .ok_or_else(|| format!("unknown action `{id}` for `{key}` in [{context}] skipped"))?;
    Ok((context, keys, Some(action)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_entries_load_and_bad_ones_are_reported() {
        let (layer, warnings) = UserLayer::parse(
            "[global]\n\"F5\" = \"request.send\"\n\"F6\" = \"request.sned\"\n\
             \"Ctrl+Foo\" = \"help.open\"\n\"F7\" = 3\n\"Ctrl+R\" = \"none\"\n\
             [toolbar]\n\"x\" = \"app.quit\"\n[editor]\n\"F8\" = \"request.save\"",
        );
        let mut parsed: Vec<_> = layer
            .bindings
            .iter()
            .map(|(c, k, a)| (c.name(), chord::display(k), a.map(ActionId::id)))
            .collect();
        parsed.sort();
        assert_eq!(
            parsed,
            [
                ("editor", "F8".to_owned(), Some("request.save")),
                ("global", "Ctrl+R".to_owned(), None),
                ("global", "F5".to_owned(), Some("request.send")),
            ]
        );
        assert_eq!(warnings.len(), 4, "{warnings:#?}");
        let all = warnings.join("\n");
        assert!(all.contains("unknown action `request.sned`"), "{all}");
        assert!(all.contains("invalid key `Ctrl+Foo`"), "{all}");
        assert!(
            all.contains("`F7` in [global] must be an action id"),
            "{all}"
        );
        assert!(all.contains("unknown context [toolbar]"), "{all}");
    }

    #[test]
    fn two_spellings_of_one_key_are_both_skipped() {
        let (layer, warnings) = UserLayer::parse(
            "[global]\n\"Ctrl+R\" = \"app.quit\"\n\"ctrl+r\" = \"help.open\"\n\"F5\" = \"help.open\"",
        );
        assert_eq!(layer.bindings.len(), 1, "{layer:?}");
        assert_eq!(chord::display(&layer.bindings[0].1), "F5");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("same key Ctrl+R"), "{warnings:?}");
    }

    #[test]
    fn an_invalid_file_is_one_warning() {
        let (layer, warnings) = UserLayer::parse("[global\n");
        assert_eq!(layer, UserLayer::default());
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].starts_with("keybindings.toml ignored"),
            "{warnings:?}"
        );
        let (_, warnings) = UserLayer::parse("global = 1");
        assert!(
            warnings[0].contains("[global] must be a table"),
            "{warnings:?}"
        );
    }

    #[test]
    fn loading_reads_the_config_dir() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            UserLayer::load(Some(tmp.path())),
            (UserLayer::default(), vec![])
        );
        assert_eq!(UserLayer::load(None), (UserLayer::default(), vec![]));
        std::fs::write(
            tmp.path().join(FILE_NAME),
            "[global]\n\"F5\" = \"request.send\"",
        )
        .unwrap();
        let (layer, warnings) = UserLayer::load(Some(tmp.path()));
        assert!(warnings.is_empty());
        assert_eq!(layer.bindings.len(), 1);
    }
}
