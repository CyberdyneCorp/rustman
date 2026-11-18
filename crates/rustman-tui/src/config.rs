//! UI options: `tui.toml` in the platform config directory, command-line
//! overrides, and the detected colour mode.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::glyphs::{ASCII, Glyphs, UNICODE};
use crate::theme::{ColorMode, Theme, ThemeName};

/// The name of the TUI settings file in the config directory.
pub const FILE_NAME: &str = "tui.toml";

/// `borders` in `tui.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Borders {
    Unicode,
    Ascii,
}

/// `keymap` in `tui.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeymapPreset {
    Default,
    Vim,
}

/// The keys of `tui.toml` this version understands. Unknown keys are
/// ignored so that a newer file still loads.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct TuiConfig {
    pub theme: Option<ThemeName>,
    pub borders: Option<Borders>,
    pub mouse: Option<bool>,
    pub keymap: Option<KeymapPreset>,
}

impl TuiConfig {
    pub fn parse(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }

    /// Read `tui.toml` from `dir`. A missing file is the default; an
    /// unreadable or invalid one is the default plus a warning.
    pub fn load(dir: Option<&Path>) -> (Self, Option<String>) {
        let Some(path) = dir.map(|d| d.join(FILE_NAME)) else {
            return (Self::default(), None);
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return (Self::default(), None);
            }
            Err(err) => return (Self::default(), Some(warning(&path, &err.to_string()))),
        };
        match Self::parse(&text) {
            Ok(config) => (config, None),
            Err(message) => (Self::default(), Some(warning(&path, &message))),
        }
    }
}

fn warning(path: &Path, message: &str) -> String {
    let first_line = message.lines().next().unwrap_or_default();
    format!("{} ignored: {first_line}", path.display())
}

/// Command-line overrides (`--theme`, `--ascii`, `--vim`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Overrides {
    pub theme: Option<ThemeName>,
    pub ascii: bool,
    pub vim: bool,
}

/// The effective UI options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiOptions {
    pub theme: Theme,
    pub glyphs: Glyphs,
    /// Capture mouse events (`mouse = false` leaves them to the terminal).
    pub mouse: bool,
    /// Vim keys (`keymap = "vim"` or `--vim`).
    pub vim: bool,
}

impl Default for UiOptions {
    fn default() -> Self {
        Self {
            theme: Theme::default(),
            glyphs: UNICODE,
            mouse: true,
            vim: false,
        }
    }
}

impl UiOptions {
    /// Combine the file, the command line (which wins) and the colour mode.
    pub fn resolve(file: &TuiConfig, cli: Overrides, mode: ColorMode) -> Self {
        let theme = cli.theme.or(file.theme).unwrap_or_default();
        let ascii = cli.ascii || file.borders == Some(Borders::Ascii);
        Self {
            theme: Theme::new(theme, mode),
            glyphs: if ascii { ASCII } else { UNICODE },
            mouse: file.mouse.unwrap_or(true),
            vim: cli.vim || file.keymap == Some(KeymapPreset::Vim),
        }
    }
}

/// The platform config directory for Rustman, read through `var`:
/// `%APPDATA%\rustman` on Windows, else `$XDG_CONFIG_HOME/rustman` or
/// `$HOME/.config/rustman`.
pub fn config_dir_from(var: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let non_empty = |name: &str| var(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    let base = if cfg!(windows) {
        non_empty("APPDATA")
    } else {
        non_empty("XDG_CONFIG_HOME").or_else(|| non_empty("HOME").map(|h| h.join(".config")))
    };
    base.map(|b| b.join("rustman"))
}

/// [`config_dir_from`] over this process's environment.
pub fn config_dir() -> Option<PathBuf> {
    config_dir_from(|name| std::env::var(name).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_line_beats_the_file() {
        let file = TuiConfig::parse("theme = \"light\"\nborders = \"unicode\"").unwrap();
        let cli = Overrides {
            theme: Some(ThemeName::HighContrast),
            ascii: true,
            vim: true,
        };
        let options = UiOptions::resolve(&file, cli, ColorMode::Ansi256);
        assert_eq!(options.theme.name, ThemeName::HighContrast);
        assert_eq!(options.theme.mode, ColorMode::Ansi256);
        assert_eq!(options.glyphs, ASCII);
        assert!(options.mouse);
        assert!(options.vim);
    }

    #[test]
    fn the_file_sets_theme_borders_and_mouse() {
        let file = TuiConfig::parse(
            "theme = \"light\"\nborders = \"ascii\"\nmouse = false\nkeymap = \"vim\"",
        )
        .unwrap();
        let options = UiOptions::resolve(&file, Overrides::default(), ColorMode::TrueColor);
        assert_eq!(options.theme.name, ThemeName::Light);
        assert_eq!(options.glyphs, ASCII);
        assert!(!options.mouse);
        assert!(options.vim, "keymap = \"vim\"");
    }

    #[test]
    fn defaults_are_dark_unicode_with_the_mouse() {
        let options = UiOptions::resolve(
            &TuiConfig::default(),
            Overrides::default(),
            ColorMode::NoColor,
        );
        assert_eq!(options.theme.name, ThemeName::Dark);
        assert_eq!(options.glyphs, UNICODE);
        assert!(options.mouse);
        assert!(!options.vim);
    }

    #[test]
    fn loading_reports_bad_files_and_ignores_missing_ones() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            TuiConfig::load(Some(tmp.path())),
            (TuiConfig::default(), None)
        );
        assert_eq!(TuiConfig::load(None), (TuiConfig::default(), None));
        std::fs::write(tmp.path().join(FILE_NAME), "theme = \"neon\"").unwrap();
        let (config, warning) = TuiConfig::load(Some(tmp.path()));
        assert_eq!(config, TuiConfig::default());
        let warning = warning.unwrap();
        assert!(warning.contains("tui.toml ignored"), "{warning}");
        std::fs::write(tmp.path().join(FILE_NAME), "mouse = false").unwrap();
        assert_eq!(TuiConfig::load(Some(tmp.path())).0.mouse, Some(false));
    }

    #[test]
    fn the_config_dir_follows_the_platform() {
        let vars = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        assert_eq!(config_dir_from(vars(&[])), None);
        if cfg!(windows) {
            let dir = config_dir_from(vars(&[("APPDATA", r"C:\Users\a\AppData\Roaming")]));
            assert_eq!(
                dir,
                Some(PathBuf::from(r"C:\Users\a\AppData\Roaming\rustman"))
            );
        } else {
            let home = config_dir_from(vars(&[("HOME", "/home/a")]));
            assert_eq!(home, Some(PathBuf::from("/home/a/.config/rustman")));
            let xdg = config_dir_from(vars(&[("HOME", "/home/a"), ("XDG_CONFIG_HOME", "/cfg")]));
            assert_eq!(xdg, Some(PathBuf::from("/cfg/rustman")));
        }
    }
}
