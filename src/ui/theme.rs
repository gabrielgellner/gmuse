//! Theming: semantic roles resolved against a colour palette.
//!
//! The indirection is the point. Widgets ask for a *role* — "the focused pane
//! border", "the selected row" — and never name a colour. A theme is then data:
//! a palette of named colours, and a set of styles that reference those names.
//!
//! Taken from eilmeldung (see `docs/inspiration/eilmeldung.md`); before this,
//! the UI had 21 hardcoded `Color::` literals, each one a place a theme could
//! not reach.

use std::collections::HashMap;
use std::str::FromStr;

use ratatui::style::{Color, Modifier, Style};
use serde::Deserialize;

/// Everywhere the UI asks for a style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Role {
    TabActive,
    TabInactive,
    BorderFocused,
    BorderUnfocused,
    SelectionFocused,
    SelectionUnfocused,
    StatusText,
    StatusDim,
    StatusPlaying,
    StatusNotice,
    StatusModes,
    StatusUnderrun,
    PromptSigil,
    SettingKey,
    BindingContext,
    BindingKey,
    AlbumHeader,
    Gutter,
    Playing,
}

impl Role {
    pub fn name(self) -> &'static str {
        match self {
            Role::TabActive => "tab_active",
            Role::TabInactive => "tab_inactive",
            Role::BorderFocused => "border_focused",
            Role::BorderUnfocused => "border_unfocused",
            Role::SelectionFocused => "selection_focused",
            Role::SelectionUnfocused => "selection_unfocused",
            Role::StatusText => "status_text",
            Role::StatusDim => "status_dim",
            Role::StatusPlaying => "status_playing",
            Role::StatusNotice => "status_notice",
            Role::StatusModes => "status_modes",
            Role::StatusUnderrun => "status_underrun",
            Role::PromptSigil => "prompt_sigil",
            Role::SettingKey => "setting_key",
            Role::BindingContext => "binding_context",
            Role::BindingKey => "binding_key",
            Role::AlbumHeader => "album_header",
            Role::Gutter => "gutter",
            Role::Playing => "playing",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|r| r.name() == s)
    }

    pub const ALL: &'static [Role] = &[
        Role::TabActive,
        Role::TabInactive,
        Role::BorderFocused,
        Role::BorderUnfocused,
        Role::SelectionFocused,
        Role::SelectionUnfocused,
        Role::StatusText,
        Role::StatusDim,
        Role::StatusPlaying,
        Role::StatusNotice,
        Role::StatusModes,
        Role::StatusUnderrun,
        Role::PromptSigil,
        Role::SettingKey,
        Role::BindingContext,
        Role::BindingKey,
        Role::AlbumHeader,
        Role::Gutter,
        Role::Playing,
    ];
}

/// One style, as written in the config.
///
/// `fg`/`bg` may name a palette entry or a colour directly, so a small tweak
/// does not require inventing a palette name for it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StyleSpec {
    pub fg: Option<String>,
    pub bg: Option<String>,
    #[serde(default)]
    pub mods: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ThemeError {
    #[error("no style called {0:?}")]
    NoSuchRole(String),
    #[error("{0:?} is not a colour or a palette entry")]
    BadColour(String),
    #[error("{0:?} is not a modifier (bold, dim, italic, underlined, reversed, crossed_out)")]
    BadModifier(String),
}

/// Resolved styles, with a built-in default for every role.
///
/// A config overrides only the roles it names, so a two-line theme is a valid
/// theme.
#[derive(Debug, Clone)]
pub struct Theme {
    styles: HashMap<Role, Style>,
}

impl Default for Theme {
    /// Catppuccin Frappé, expressed through the ANSI sixteen.
    ///
    /// **Named colours, not hex, on purpose.** A terminal themed to Frappé —
    /// as kitty is here — resolves `blue` to exactly `#8caaee`, so names give
    /// the true palette *and* follow the terminal if it is later themed to
    /// something else. Hex would be identical everywhere and would ignore a
    /// carefully set palette. `examples/catppuccin-frappe.toml` has the hex
    /// variant for terminals that are not themed.
    ///
    /// | name       | Frappé              | used for              |
    /// |------------|---------------------|-----------------------|
    /// | `Reset`    | base / text         | the terminal's own    |
    /// | `DarkGray` | surface2 `#626880`  | selections, dividers  |
    /// | `Gray`     | subtext1 `#b5bfe2`  | unfocused selection   |
    /// | `Blue`     | `#8caaee`           | titles, borders       |
    /// | `Magenta`  | pink `#f4b8e4`      | the cursor            |
    /// | `Cyan`     | teal `#81c8be`      | album lines, keys     |
    /// | `Yellow`   | `#e5c890`           | notices               |
    /// | `Green`    | `#a6d189`           | playing               |
    /// | `Red`      | `#e78284`           | errors                |
    ///
    /// **Nothing here puts a dark foreground on a light background.** The ANSI
    /// sixteen carry no colour dark enough to read against blue or pink, so a
    /// selection is a surface with a coloured foreground — which is how a dark
    /// theme wants to be read anyway. (This is the rule from the cmus rc this
    /// palette was lifted from; the previous defaults broke it, with black on
    /// cyan for both the active tab and the selection.)
    ///
    /// One deliberate deviation: that rc uses `black` (surface1) as the
    /// selection background, which is right on a Frappé-themed terminal and
    /// invisible on an unthemed one where `black` is the background. `DarkGray`
    /// (surface2) is a Frappé surface too and reads on both.
    fn default() -> Self {
        let mut styles = HashMap::new();
        let surface = Color::DarkGray;
        for role in Role::ALL.iter().copied() {
            let style = match role {
                Role::TabActive => Style::default()
                    .bg(surface)
                    .fg(Color::Blue)
                    .add_modifier(Modifier::BOLD),
                Role::TabInactive => Style::default().fg(Color::DarkGray),
                Role::BorderFocused => Style::default().fg(Color::Blue),
                Role::BorderUnfocused => Style::default().fg(Color::DarkGray),
                Role::SelectionFocused => Style::default()
                    .bg(surface)
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
                // Dimmer, so you can still see where the cursor was left after
                // a Tab, without it competing with the focused pane.
                Role::SelectionUnfocused => Style::default().bg(surface).fg(Color::Gray),
                Role::StatusText => Style::default().fg(Color::Reset),
                Role::StatusDim => Style::default().fg(Color::DarkGray),
                Role::StatusPlaying => Style::default().fg(Color::Green),
                Role::StatusNotice => Style::default().fg(Color::Yellow),
                Role::StatusModes => Style::default().fg(Color::Magenta),
                Role::StatusUnderrun => Style::default().fg(Color::Red),
                Role::PromptSigil => Style::default()
                    .fg(Color::Blue)
                    .add_modifier(Modifier::BOLD),
                Role::SettingKey => Style::default().fg(Color::Cyan),
                Role::BindingContext => Style::default().fg(Color::DarkGray),
                Role::BindingKey => Style::default().fg(Color::Yellow),
                // cmus styles its track-window album line separately, and sets
                // it to cyan in the rc this came from.
                Role::AlbumHeader => Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
                Role::Gutter => Style::default().fg(Color::DarkGray),
                // The same green as the status line's play indicator, so
                // "playing" means one thing wherever it appears.
                Role::Playing => Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            };
            styles.insert(role, style);
        }
        Self { styles }
    }
}

impl Theme {
    /// The style for a role. Always succeeds: every role has a default.
    pub fn get(&self, role: Role) -> Style {
        self.styles.get(&role).copied().unwrap_or_default()
    }

    /// Apply a config's palette and styles on top of the defaults.
    ///
    /// Every problem is collected rather than returned on the first one: a
    /// config with three typos should report three, not make you find them one
    /// run at a time.
    pub fn apply(
        &mut self,
        palette: &HashMap<String, String>,
        styles: &HashMap<String, StyleSpec>,
    ) -> Vec<ThemeError> {
        let mut errors = Vec::new();

        // Resolve the palette first, so styles can reference it.
        let mut colours: HashMap<&str, Color> = HashMap::new();
        for (name, value) in palette {
            match Color::from_str(value) {
                Ok(c) => {
                    colours.insert(name.as_str(), c);
                }
                Err(_) => errors.push(ThemeError::BadColour(value.clone())),
            }
        }

        for (name, spec) in styles {
            let Some(role) = Role::parse(name) else {
                errors.push(ThemeError::NoSuchRole(name.clone()));
                continue;
            };
            let mut style = Style::default();
            let resolve = |text: &String, errors: &mut Vec<ThemeError>| -> Option<Color> {
                // A palette name wins over a colour name, so a palette entry
                // called "red" means what the theme says it means.
                if let Some(c) = colours.get(text.as_str()) {
                    return Some(*c);
                }
                match Color::from_str(text) {
                    Ok(c) => Some(c),
                    Err(_) => {
                        errors.push(ThemeError::BadColour(text.clone()));
                        None
                    }
                }
            };
            if let Some(fg) = &spec.fg
                && let Some(c) = resolve(fg, &mut errors)
            {
                style = style.fg(c);
            }
            if let Some(bg) = &spec.bg
                && let Some(c) = resolve(bg, &mut errors)
            {
                style = style.bg(c);
            }
            for m in &spec.mods {
                match modifier(m) {
                    Some(m) => style = style.add_modifier(m),
                    None => errors.push(ThemeError::BadModifier(m.clone())),
                }
            }
            self.styles.insert(role, style);
        }

        errors
    }
}

fn modifier(name: &str) -> Option<Modifier> {
    Some(match name {
        "bold" => Modifier::BOLD,
        "dim" => Modifier::DIM,
        "italic" => Modifier::ITALIC,
        "underlined" | "underline" => Modifier::UNDERLINED,
        "reversed" | "reverse" => Modifier::REVERSED,
        "crossed_out" | "strikethrough" => Modifier::CROSSED_OUT,
        "slow_blink" => Modifier::SLOW_BLINK,
        "hidden" => Modifier::HIDDEN,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn palette(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn every_role_has_a_default() {
        let theme = Theme::default();
        for role in Role::ALL.iter().copied() {
            // `get` cannot fail, but an unset role would silently render as
            // plain text, which is the bug this guards.
            assert!(
                theme.styles.contains_key(&role),
                "{} has no default",
                role.name()
            );
        }
    }

    #[test]
    fn role_names_round_trip() {
        for role in Role::ALL.iter().copied() {
            assert_eq!(Role::parse(role.name()), Some(role));
        }
        assert_eq!(Role::parse("no_such_role"), None);
    }

    #[test]
    fn a_style_can_reference_the_palette() {
        let mut theme = Theme::default();
        let styles = HashMap::from([(
            "border_focused".to_string(),
            StyleSpec {
                fg: Some("accent".into()),
                bg: None,
                mods: vec!["bold".into()],
            },
        )]);
        let errors = theme.apply(&palette(&[("accent", "magenta")]), &styles);
        assert!(errors.is_empty(), "{errors:?}");

        let style = theme.get(Role::BorderFocused);
        assert_eq!(style.fg, Some(Color::Magenta));
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn a_palette_name_wins_over_a_colour_name() {
        let mut theme = Theme::default();
        let styles = HashMap::from([(
            "status_text".to_string(),
            StyleSpec {
                fg: Some("red".into()),
                ..Default::default()
            },
        )]);
        // A palette entry called "red" that is actually blue must be honoured,
        // or a theme could not rename anything.
        theme.apply(&palette(&[("red", "blue")]), &styles);
        assert_eq!(theme.get(Role::StatusText).fg, Some(Color::Blue));
    }

    #[test]
    fn a_literal_colour_works_without_a_palette() {
        let mut theme = Theme::default();
        let styles = HashMap::from([(
            "status_text".to_string(),
            StyleSpec {
                fg: Some("#ff8800".into()),
                ..Default::default()
            },
        )]);
        let errors = theme.apply(&HashMap::new(), &styles);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            theme.get(Role::StatusText).fg,
            Some(Color::Rgb(0xff, 0x88, 0x00))
        );
    }

    #[test]
    fn partial_themes_leave_the_rest_alone() {
        let defaults = Theme::default();
        let mut theme = Theme::default();
        theme.apply(
            &HashMap::new(),
            &HashMap::from([(
                "tab_active".to_string(),
                StyleSpec {
                    fg: Some("green".into()),
                    ..Default::default()
                },
            )]),
        );
        assert_eq!(theme.get(Role::TabActive).fg, Some(Color::Green));
        // Untouched roles keep their defaults, so a two-line theme is valid.
        assert_eq!(
            theme.get(Role::StatusPlaying),
            defaults.get(Role::StatusPlaying)
        );
    }

    /// Every mistake at once, so a config with three typos reports three.
    #[test]
    fn all_problems_are_reported_together() {
        let mut theme = Theme::default();
        let styles = HashMap::from([
            ("no_such_role".to_string(), StyleSpec::default()),
            (
                "status_text".to_string(),
                StyleSpec {
                    fg: Some("nosuchcolour".into()),
                    bg: None,
                    mods: vec!["squiggly".into()],
                },
            ),
        ]);
        let errors = theme.apply(&palette(&[("bad", "alsonotacolour")]), &styles);
        assert_eq!(errors.len(), 4, "{errors:?}");
        assert!(errors.contains(&ThemeError::NoSuchRole("no_such_role".into())));
        assert!(errors.contains(&ThemeError::BadColour("nosuchcolour".into())));
        assert!(errors.contains(&ThemeError::BadModifier("squiggly".into())));
        // The broken palette entry counts too — it is a mistake in the same file.
        assert!(errors.contains(&ThemeError::BadColour("alsonotacolour".into())));
    }
    /// The rule from the cmus rc this palette came from: the ANSI sixteen
    /// carry no colour dark enough to read against blue or pink, so nothing
    /// may put a dark foreground on a light background. The previous defaults
    /// broke it — black on cyan, for both the active tab and the selection.
    #[test]
    fn no_dark_foreground_on_a_light_background() {
        let theme = Theme::default();
        let dark = [Color::Black, Color::DarkGray, Color::Reset];
        let light = [
            Color::Cyan,
            Color::Blue,
            Color::Magenta,
            Color::Yellow,
            Color::Green,
            Color::Red,
            Color::White,
            Color::Gray,
        ];
        for role in Role::ALL.iter().copied() {
            let style = theme.get(role);
            if let (Some(fg), Some(bg)) = (style.fg, style.bg) {
                assert!(
                    !(dark.contains(&fg) && light.contains(&bg)),
                    "{}: {fg:?} on {bg:?} is unreadable in the ANSI sixteen",
                    role.name()
                );
            }
        }
    }

    /// A selection has to be visible without relying on a background that may
    /// be the terminal's own.
    #[test]
    fn selections_are_distinguishable() {
        let theme = Theme::default();
        let focused = theme.get(Role::SelectionFocused);
        let unfocused = theme.get(Role::SelectionUnfocused);
        assert!(focused.bg.is_some(), "the focused selection has no surface");
        assert_ne!(
            focused.fg, unfocused.fg,
            "focused and unfocused selections look the same"
        );
        assert_ne!(
            focused.bg,
            Some(Color::Reset),
            "the surface is the background"
        );
    }

    /// The shipped hex theme must apply cleanly — a typo there is a broken
    /// example rather than a broken program, which is easy to miss.
    #[test]
    fn the_shipped_frappe_theme_parses() {
        let text = include_str!("../../examples/catppuccin-frappe.toml");
        let loaded = crate::config::parse_text(text);
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        // And it actually reaches the roles, rather than parsing into nothing.
        assert_eq!(
            loaded.theme.get(Role::AlbumHeader).fg,
            Some(Color::Rgb(0x81, 0xc8, 0xbe)),
            "teal did not resolve"
        );
    }

    /// Every role the shipped theme names must exist, and vice versa — a role
    /// added in code and forgotten in the example silently keeps its default.
    #[test]
    fn the_shipped_theme_covers_every_role() {
        let text = include_str!("../../examples/catppuccin-frappe.toml");
        for role in Role::ALL.iter().copied() {
            assert!(
                text.contains(role.name()),
                "{} is missing from examples/catppuccin-frappe.toml",
                role.name()
            );
        }
    }
}
