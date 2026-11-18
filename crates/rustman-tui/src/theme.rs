//! Themes and colour capability detection (design D7).
//!
//! Widget code asks for semantic [`Token`]s, never raw colours. A [`Theme`]
//! maps tokens to colours for the terminal's [`ColorMode`]: 24-bit RGB, the
//! nearest xterm-256 index, or no colour at all (`NO_COLOR`), where text
//! attributes carry the meaning instead.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use ratatui::style::{Color, Modifier, Style};
use serde::Deserialize;

/// A semantic colour role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Token {
    Text,
    Muted,
    Accent,
    Border,
    /// The border and highlight of the focused pane.
    Focus,
    /// The background of selected rows.
    Selection,
    Ok,
    Info,
    Warn,
    Error,
    /// The text colour on badges, drawn over another token's colour.
    Background,
}

impl Token {
    pub const ALL: [Self; 11] = [
        Self::Text,
        Self::Muted,
        Self::Accent,
        Self::Border,
        Self::Focus,
        Self::Selection,
        Self::Ok,
        Self::Info,
        Self::Warn,
        Self::Error,
        Self::Background,
    ];

    /// The key used in theme files.
    pub fn key(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Muted => "muted",
            Self::Accent => "accent",
            Self::Border => "border",
            Self::Focus => "focus",
            Self::Selection => "selection",
            Self::Ok => "ok",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
            Self::Background => "background",
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|&t| t == self).unwrap_or(0)
    }

    /// The text attribute that stands in for the colour without colour.
    fn monochrome(self) -> Modifier {
        match self {
            Self::Muted => Modifier::DIM,
            Self::Accent | Self::Focus | Self::Warn | Self::Error => Modifier::BOLD,
            Self::Selection => Modifier::REVERSED,
            _ => Modifier::empty(),
        }
    }
}

/// The built-in themes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeName {
    #[default]
    Dark,
    Light,
    HighContrast,
}

impl ThemeName {
    pub const ALL: [Self; 3] = [Self::Dark, Self::Light, Self::HighContrast];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
            Self::HighContrast => "high-contrast",
        }
    }

    fn source(self) -> &'static str {
        match self {
            Self::Dark => include_str!("../themes/dark.toml"),
            Self::Light => include_str!("../themes/light.toml"),
            Self::HighContrast => include_str!("../themes/high-contrast.toml"),
        }
    }
}

impl fmt::Display for ThemeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ThemeName {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|t| t.as_str() == s)
            .ok_or_else(|| format!("unknown theme `{s}` (expected dark, light or high-contrast)"))
    }
}

/// How the terminal can show colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    TrueColor,
    Ansi256,
    NoColor,
}

impl ColorMode {
    /// Detect the mode from environment variables, read through `var`:
    /// a non-empty `NO_COLOR` wins (no-color.org), then `COLORTERM=truecolor|24bit`, else
    /// xterm-256.
    pub fn detect(var: impl Fn(&str) -> Option<String>) -> Self {
        if var("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            return Self::NoColor;
        }
        match var("COLORTERM").map(|v| v.to_ascii_lowercase()).as_deref() {
            Some("truecolor" | "24bit") => Self::TrueColor,
            _ => Self::Ansi256,
        }
    }

    /// Detect the mode from this process's environment.
    pub fn from_env() -> Self {
        Self::detect(|name| std::env::var_os(name).map(|v| v.to_string_lossy().into_owned()))
    }
}

pub type Rgb = (u8, u8, u8);

/// One colour per token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette([Rgb; Token::ALL.len()]);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ThemeFile {
    colors: BTreeMap<String, String>,
}

impl Palette {
    /// Parse a theme file: a `[colors]` table with one `#rrggbb` per token.
    pub fn parse(text: &str) -> Result<Self, String> {
        let file: ThemeFile = toml::from_str(text).map_err(|e| e.to_string())?;
        if let Some(unknown) = file
            .colors
            .keys()
            .find(|k| !Token::ALL.iter().any(|t| t.key() == k.as_str()))
        {
            return Err(format!("unknown colour token `{unknown}`"));
        }
        let mut colors = [(0, 0, 0); Token::ALL.len()];
        for token in Token::ALL {
            let hex = file
                .colors
                .get(token.key())
                .ok_or_else(|| format!("missing colour token `{}`", token.key()))?;
            colors[token.index()] = parse_hex(hex)?;
        }
        Ok(Self(colors))
    }

    pub fn get(&self, token: Token) -> Rgb {
        self.0[token.index()]
    }
}

fn parse_hex(hex: &str) -> Result<Rgb, String> {
    let bad = || format!("`{hex}` is not a #rrggbb colour");
    let digits = hex
        .strip_prefix('#')
        .filter(|d| d.len() == 6)
        .ok_or_else(bad)?;
    let channel = |i: usize| {
        digits
            .get(i..i + 2)
            .and_then(|s| u8::from_str_radix(s, 16).ok())
            .ok_or_else(bad)
    };
    Ok((channel(0)?, channel(2)?, channel(4)?))
}

/// The six levels of each channel in the xterm-256 colour cube.
const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// The xterm-256 index (16..=255) closest to `rgb`, from the colour cube and
/// the grey ramp. The first 16 entries are left out because terminals
/// redefine them.
pub fn nearest_xterm256(rgb: Rgb) -> u8 {
    let level = |v: u8| {
        (0..CUBE.len())
            .min_by_key(|&i| CUBE[i].abs_diff(v))
            .unwrap_or(0)
    };
    let (r, g, b) = (level(rgb.0), level(rgb.1), level(rgb.2));
    let cube_rgb = (CUBE[r], CUBE[g], CUBE[b]);
    let cube_index = 16 + 36 * r + 6 * g + b;
    let average = (u16::from(rgb.0) + u16::from(rgb.1) + u16::from(rgb.2)) / 3;
    let step = (average.saturating_sub(3) / 10).min(23);
    let grey = u8::try_from(8 + 10 * step).unwrap_or(238);
    let grey_index = 232 + usize::from(step);
    let index = if distance(rgb, (grey, grey, grey)) < distance(rgb, cube_rgb) {
        grey_index
    } else {
        cube_index
    };
    u8::try_from(index).unwrap_or(u8::MAX)
}

fn distance(a: Rgb, b: Rgb) -> u32 {
    let d = |x: u8, y: u8| u32::from(x.abs_diff(y)).pow(2);
    d(a.0, b.0) + d(a.1, b.1) + d(a.2, b.2)
}

/// A built-in theme rendered for one colour mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    pub name: ThemeName,
    pub mode: ColorMode,
    palette: Palette,
}

impl Default for Theme {
    fn default() -> Self {
        Self::new(ThemeName::Dark, ColorMode::TrueColor)
    }
}

impl Theme {
    pub fn new(name: ThemeName, mode: ColorMode) -> Self {
        // The built-in files are checked by the tests below.
        let palette = Palette::parse(name.source()).expect("built-in theme parses");
        Self {
            name,
            mode,
            palette,
        }
    }

    /// The colour for `token`, or `None` without colour.
    pub fn color(&self, token: Token) -> Option<Color> {
        let (r, g, b) = self.palette.get(token);
        match self.mode {
            ColorMode::TrueColor => Some(Color::Rgb(r, g, b)),
            ColorMode::Ansi256 => Some(Color::Indexed(nearest_xterm256((r, g, b)))),
            ColorMode::NoColor => None,
        }
    }

    /// Text in the colour of `token` (or its attribute without colour).
    pub fn fg(&self, token: Token) -> Style {
        match self.color(token) {
            Some(color) => Style::new().fg(color),
            None => Style::new().add_modifier(token.monochrome()),
        }
    }

    /// A selected row.
    pub fn selection(&self) -> Style {
        match (self.color(Token::Selection), self.color(Token::Text)) {
            (Some(bg), Some(fg)) => Style::new().bg(bg).fg(fg),
            _ => Style::new().add_modifier(Modifier::REVERSED),
        }
    }

    /// A badge: bold text on the colour of `token` (reverse video without
    /// colour).
    pub fn badge(&self, token: Token) -> Style {
        let style = Style::new().add_modifier(Modifier::BOLD);
        match (self.color(token), self.color(Token::Background)) {
            (Some(bg), Some(fg)) => style.bg(bg).fg(fg),
            _ => style.add_modifier(Modifier::REVERSED),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    #[test]
    fn colour_mode_detection() {
        assert_eq!(ColorMode::detect(env(&[])), ColorMode::Ansi256);
        let truecolor = [("COLORTERM", "truecolor")];
        assert_eq!(ColorMode::detect(env(&truecolor)), ColorMode::TrueColor);
        let bits = [("COLORTERM", "24BIT")];
        assert_eq!(ColorMode::detect(env(&bits)), ColorMode::TrueColor);
        let xterm = [("TERM", "xterm-256color")];
        assert_eq!(ColorMode::detect(env(&xterm)), ColorMode::Ansi256);
        let no_color = [("NO_COLOR", "1"), ("COLORTERM", "truecolor")];
        assert_eq!(ColorMode::detect(env(&no_color)), ColorMode::NoColor);
        // no-color.org: an empty NO_COLOR does not disable colour.
        let empty = [("NO_COLOR", ""), ("COLORTERM", "truecolor")];
        assert_eq!(ColorMode::detect(env(&empty)), ColorMode::TrueColor);
    }

    #[test]
    fn nearest_xterm256_picks_the_cube_or_the_grey_ramp() {
        assert_eq!(nearest_xterm256((0, 0, 0)), 16);
        assert_eq!(nearest_xterm256((255, 255, 255)), 231);
        assert_eq!(nearest_xterm256((255, 0, 0)), 196);
        assert_eq!(nearest_xterm256((95, 135, 175)), 67);
        assert_eq!(nearest_xterm256((128, 128, 128)), 244);
        assert_eq!(nearest_xterm256((8, 8, 8)), 232);
        assert_eq!(nearest_xterm256((238, 238, 238)), 255);
    }

    #[test]
    fn every_built_in_theme_parses_and_differs() {
        let palettes: Vec<Palette> = ThemeName::ALL
            .iter()
            .map(|t| Palette::parse(t.source()).unwrap())
            .collect();
        assert_ne!(palettes[0], palettes[1]);
        assert_ne!(palettes[1], palettes[2]);
    }

    #[test]
    fn a_256_colour_terminal_gets_an_index_for_every_token() {
        // Scenario "256-colour terminal": COLORTERM unset, TERM=xterm-256color.
        let mode = ColorMode::detect(env(&[("TERM", "xterm-256color")]));
        for name in ThemeName::ALL {
            let theme = Theme::new(name, mode);
            for token in Token::ALL {
                let expected = nearest_xterm256(theme.palette.get(token));
                assert_eq!(theme.color(token), Some(Color::Indexed(expected)));
            }
        }
    }

    #[test]
    fn truecolor_uses_the_palette_rgb() {
        let theme = Theme::new(ThemeName::Dark, ColorMode::TrueColor);
        assert_eq!(
            theme.color(Token::Error),
            Some(Color::Rgb(0xf7, 0x76, 0x8e))
        );
        assert_eq!(theme.fg(Token::Ok).fg, Some(Color::Rgb(0x9e, 0xce, 0x6a)));
    }

    #[test]
    fn no_color_uses_attributes_only() {
        let theme = Theme::new(ThemeName::Light, ColorMode::NoColor);
        for token in Token::ALL {
            let style = theme.fg(token);
            assert_eq!((style.fg, style.bg), (None, None), "{token:?}");
        }
        assert!(theme.selection().add_modifier.contains(Modifier::REVERSED));
        assert!(
            theme
                .badge(Token::Warn)
                .add_modifier
                .contains(Modifier::REVERSED)
        );
        assert!(theme.fg(Token::Muted).add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn badges_put_the_background_colour_on_the_token_colour() {
        let theme = Theme::new(ThemeName::HighContrast, ColorMode::TrueColor);
        let badge = theme.badge(Token::Warn);
        assert_eq!(badge.bg, Some(Color::Rgb(0xff, 0xff, 0)));
        assert_eq!(badge.fg, Some(Color::Rgb(0, 0, 0)));
    }

    #[test]
    fn bad_theme_files_are_rejected() {
        assert!(Palette::parse("[colors]\ntext = \"#fff\"").is_err());
        assert!(
            Palette::parse("[colors]\nshiny = \"#ffffff\"")
                .unwrap_err()
                .contains("shiny")
        );
        let missing = Palette::parse("[colors]\ntext = \"#ffffff\"").unwrap_err();
        assert!(
            missing.contains("missing colour token `muted`"),
            "{missing}"
        );
        assert!(parse_hex("#zzzzzz").is_err());
    }

    #[test]
    fn theme_names_round_trip() {
        for name in ThemeName::ALL {
            assert_eq!(name.as_str().parse::<ThemeName>(), Ok(name));
        }
        assert!("neon".parse::<ThemeName>().is_err());
    }
}
