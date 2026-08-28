//! The TOML config file.
//!
//! Settings and theming are declarative and want structure; commands are
//! imperative and want a language. So the file is TOML, and the *values* of
//! its key bindings are command strings run through the §6 parser — the same
//! one `:bind` and the `:` line use. There is still exactly one command
//! language, and now something sensible holds it.
//!
//! Borrowed from eilmeldung; see `docs/inspiration/eilmeldung.md`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::cmd::{Command, Context, keys, parse};
use crate::ui::theme::{StyleSpec, Theme};

/// `$XDG_CONFIG_HOME/gmuse/config.toml`, falling back to `~/.config`.
pub fn default_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("gmuse").join("config.toml")
}

/// One binding's value: a single command, or a list run in order.
///
/// A list is a macro, which falls out of TOML for free.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Binding {
    One(String),
    Many(Vec<String>),
}

impl Binding {
    fn commands(&self) -> Vec<&str> {
        match self {
            Binding::One(s) => vec![s.as_str()],
            Binding::Many(v) => v.iter().map(String::as_str).collect(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeTable {
    #[serde(default)]
    pub palette: HashMap<String, String>,
    #[serde(default)]
    pub styles: HashMap<String, StyleSpec>,
}

/// The whole file.
///
/// `deny_unknown_fields` throughout: a typo in a config should be an error you
/// are told about, not a setting that silently does nothing. This is the main
/// thing TOML buys us over the old line-oriented rc file.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// `settings.shuffle = true` and friends, as `set key=value` commands.
    #[serde(default)]
    pub settings: HashMap<String, toml::Value>,
    /// `[keys.common] j = "win-down"`.
    #[serde(default)]
    pub keys: HashMap<String, HashMap<String, Binding>>,
    #[serde(default)]
    pub theme: ThemeTable,
    /// Commands run once at startup, after everything else is applied.
    #[serde(default)]
    pub startup_commands: Vec<String>,
    /// `[catalogs.general] roots = [...]`.
    #[serde(default)]
    pub catalogs: HashMap<String, CatalogTable>,
    /// Which catalogs start active. Omit, or `["all"]`, for everything.
    #[serde(default)]
    pub active_catalogs: Vec<String>,
    /// Library roots, for when you do not want named catalogs.
    ///
    /// The durable home for roots: the cache is derived data and is thrown
    /// away whenever its format changes, so roots that live only there are
    /// lost on an upgrade.
    #[serde(default)]
    pub library: Vec<PathBuf>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogTable {
    #[serde(default)]
    pub roots: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConfigError {
    pub where_: String,
    pub message: String,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.where_, self.message)
    }
}

/// What a loaded config turns into: commands to run, and a theme to draw with.
#[derive(Debug, Default)]
pub struct Loaded {
    pub commands: Vec<Command>,
    pub theme: Theme,
    pub catalogs: crate::catalog::Catalogs,
    /// Whether the config named `active_catalogs` at all.
    ///
    /// Kept separate from its *value*, because `["all"]` is a legitimate
    /// explicit choice and is indistinguishable from the default if you only
    /// look at the resulting `Active`. Without this, writing `["all"]` would
    /// silently lose to whatever was remembered from the last session.
    pub active_specified: bool,
    pub errors: Vec<ConfigError>,
}

/// Parse config text into commands and a theme.
///
/// Every problem is collected rather than fatal: one bad line should not cost
/// the rest of the file.
pub fn parse_text(text: &str) -> Loaded {
    let mut out = Loaded {
        theme: Theme::default(),
        ..Default::default()
    };

    let config: Config = match toml::from_str(text) {
        Ok(c) => c,
        Err(e) => {
            out.errors.push(ConfigError {
                where_: "config".into(),
                message: e.message().to_string(),
            });
            return out;
        }
    };

    // Settings become `set` commands, so they go through exactly the same
    // typed parsing and the same execute() a `:set` does.
    for (key, value) in &config.settings {
        let text = match value {
            toml::Value::String(s) => s.clone(),
            toml::Value::Boolean(b) => b.to_string(),
            toml::Value::Integer(i) => i.to_string(),
            toml::Value::Float(f) => f.to_string(),
            other => {
                out.errors.push(ConfigError {
                    where_: format!("settings.{key}"),
                    message: format!("cannot be {other}"),
                });
                continue;
            }
        };
        match parse::command(&format!("set {key}={text}")) {
            Ok(cmd) => out.commands.push(cmd),
            Err(e) => out.errors.push(ConfigError {
                where_: format!("settings.{key}"),
                message: e.to_string(),
            }),
        }
    }

    for (ctx_name, table) in &config.keys {
        let Some(ctx) = Context::parse(ctx_name) else {
            out.errors.push(ConfigError {
                where_: format!("keys.{ctx_name}"),
                message: "unknown context".into(),
            });
            continue;
        };
        for (key_name, binding) in table {
            let key = match keys::parse_seq(key_name) {
                Ok(k) => k,
                Err(e) => {
                    out.errors.push(ConfigError {
                        where_: format!("keys.{ctx_name}.{key_name}"),
                        message: e.to_string(),
                    });
                    continue;
                }
            };
            for text in binding.commands() {
                match parse::command(text) {
                    Ok(cmd) => out.commands.push(Command::Bind {
                        ctx,
                        key: key.clone(),
                        cmd: Box::new(cmd),
                    }),
                    Err(e) => out.errors.push(ConfigError {
                        where_: format!("keys.{ctx_name}.{key_name}"),
                        message: e.to_string(),
                    }),
                }
            }
        }
    }

    for e in out.theme.apply(&config.theme.palette, &config.theme.styles) {
        out.errors.push(ConfigError {
            where_: "theme".into(),
            message: e.to_string(),
        });
    }

    if !config.library.is_empty() {
        out.catalogs.define(
            crate::catalog::DEFAULT,
            config.library.iter().map(|r| expand_tilde(r)).collect(),
        );
    }

    // Catalogs. `~` is expanded here because a config file is where people
    // write it, and nothing downstream would know what to do with it.
    let mut defined: Vec<(String, Vec<PathBuf>)> = config
        .catalogs
        .iter()
        .map(|(name, table)| {
            (
                name.clone(),
                table.roots.iter().map(|r| expand_tilde(r)).collect(),
            )
        })
        .collect();
    // HashMap order is arbitrary; the label and the settings view read better
    // in a stable one.
    defined.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, roots) in defined {
        if roots.is_empty() {
            out.errors.push(ConfigError {
                where_: format!("catalogs.{name}"),
                message: "has no roots".into(),
            });
            continue;
        }
        out.catalogs.define(&name, roots);
    }

    out.active_specified = !config.active_catalogs.is_empty();
    if !config.active_catalogs.is_empty() {
        let spec = if config.active_catalogs.iter().any(|n| n == "all") {
            crate::catalog::Spec::All
        } else {
            crate::catalog::Spec::Only(config.active_catalogs.clone())
        };
        if let Err(e) = out.catalogs.apply(spec) {
            out.errors.push(ConfigError {
                where_: "active_catalogs".into(),
                message: e,
            });
        }
    }

    for text in &config.startup_commands {
        match parse::command(text) {
            Ok(cmd) => out.commands.push(cmd),
            Err(e) => out.errors.push(ConfigError {
                where_: "startup_commands".into(),
                message: e.to_string(),
            }),
        }
    }

    out
}

fn expand_tilde(path: &Path) -> PathBuf {
    let Ok(rest) = path.strip_prefix("~") else {
        return path.to_path_buf();
    };
    match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join(rest),
        None => path.to_path_buf(),
    }
}

/// Read and parse the config. A missing file is not an error.
pub fn load(path: &Path) -> Loaded {
    match std::fs::read_to_string(path) {
        Ok(text) => parse_text(&text),
        Err(_) => Loaded {
            theme: Theme::default(),
            ..Default::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::{SeekSpec, SettingKey, SettingValue};
    use ratatui::style::Color;

    #[test]
    fn settings_become_set_commands() {
        let loaded = parse_text(
            r#"
            [settings]
            shuffle = true
            aaa_mode = "album"
            "#,
        );
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        assert!(loaded.commands.contains(&Command::Set {
            key: SettingKey::Shuffle,
            value: SettingValue::Bool(true)
        }));
        assert!(loaded.commands.contains(&Command::Set {
            key: SettingKey::AaaMode,
            value: SettingValue::Aaa(crate::next::AaaMode::Album)
        }));
    }

    #[test]
    fn a_key_holds_a_command_string() {
        let loaded = parse_text(
            r#"
            [keys.common]
            "^n" = "player-next"
            "#,
        );
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        assert_eq!(loaded.commands.len(), 1);
        let Command::Bind { ctx, key, cmd } = &loaded.commands[0] else {
            panic!("not a bind")
        };
        assert_eq!(*ctx, Context::Common);
        assert_eq!(keys::format_seq(key), "^n");
        assert_eq!(**cmd, Command::PlayerNext);
    }

    /// A list value is a macro. That falls out of TOML for free, and it is the
    /// main thing the flat rc file could not express.
    #[test]
    fn a_list_binds_several_commands_to_one_key() {
        let loaded = parse_text(
            r#"
            [keys.library]
            o = ["add-queue", "player-next"]
            "#,
        );
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        assert_eq!(loaded.commands.len(), 2, "both commands should bind");
        for cmd in &loaded.commands {
            let Command::Bind { ctx, .. } = cmd else {
                panic!("not a bind")
            };
            assert_eq!(*ctx, Context::Library);
        }
    }

    #[test]
    fn the_theme_resolves_through_the_palette() {
        let loaded = parse_text(
            r#"
            [theme.palette]
            accent = "magenta"
            [theme.styles]
            border_focused = { fg = "accent", mods = ["bold"] }
            "#,
        );
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        assert_eq!(
            loaded.theme.get(crate::ui::theme::Role::BorderFocused).fg,
            Some(Color::Magenta)
        );
    }

    #[test]
    fn startup_commands_run_last() {
        let loaded = parse_text(
            r#"
            startup_commands = ["view 2", "toggle shuffle"]
            "#,
        );
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        assert_eq!(loaded.commands.len(), 2);
        assert_eq!(
            loaded.commands[0],
            Command::View(crate::cmd::ViewId::Sorted)
        );
    }

    /// A typo in a config should be reported, not silently ignored. This is
    /// the main thing TOML plus `deny_unknown_fields` buys over the rc file,
    /// where an unknown line was only ever a parse error.
    #[test]
    fn an_unknown_table_is_rejected() {
        let loaded = parse_text(
            r#"
            [setings]
            shuffle = true
            "#,
        );
        assert_eq!(loaded.errors.len(), 1, "{:?}", loaded.errors);
        assert!(
            loaded.errors[0].message.contains("unknown field"),
            "{:?}",
            loaded.errors[0]
        );
    }

    #[test]
    fn problems_are_located_and_do_not_stop_the_rest() {
        let loaded = parse_text(
            r#"
            [settings]
            shuffle = true
            nosuchsetting = 1

            [keys.common]
            "^n" = "frobnicate"
            j = "win-down"

            [keys.nosuchcontext]
            x = "quit"

            [theme.styles]
            no_such_style = { fg = "red" }
            "#,
        );
        // The good lines still loaded.
        assert!(loaded.commands.iter().any(|c| matches!(
            c,
            Command::Set {
                key: SettingKey::Shuffle,
                ..
            }
        )));
        assert!(loaded.commands.iter().any(|c| matches!(
            c,
            Command::Bind { cmd, .. } if **cmd == Command::WinDown
        )));

        // And each problem is reported with where it is.
        let places: Vec<&str> = loaded.errors.iter().map(|e| e.where_.as_str()).collect();
        assert!(places.contains(&"settings.nosuchsetting"), "{places:?}");
        assert!(places.contains(&"keys.common.^n"), "{places:?}");
        assert!(places.contains(&"keys.nosuchcontext"), "{places:?}");
        assert!(places.contains(&"theme"), "{places:?}");
    }

    #[test]
    fn a_missing_file_is_not_an_error() {
        let loaded = load(Path::new("/definitely/not/here/config.toml"));
        assert!(loaded.commands.is_empty());
        assert!(loaded.errors.is_empty());
    }

    #[test]
    fn an_empty_config_is_valid() {
        let loaded = parse_text("");
        assert!(loaded.errors.is_empty());
        assert!(loaded.commands.is_empty());
    }

    /// Seek arguments survive the trip through TOML quoting.
    #[test]
    fn command_arguments_survive() {
        let loaded = parse_text(
            r#"
            [keys.common]
            "M-f" = "seek +30"
            "#,
        );
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        let Command::Bind { cmd, .. } = &loaded.commands[0] else {
            panic!("not a bind")
        };
        assert_eq!(**cmd, Command::Seek(SeekSpec::Relative(30.0)));
    }
    /// `["all"]` is an explicit choice, not an absence of one.
    ///
    /// The two were told apart by inspecting the resulting `Active`, which
    /// cannot distinguish them — so writing `["all"]` silently lost to
    /// whatever the last session had remembered.
    #[test]
    fn choosing_all_explicitly_is_recorded_as_a_choice() {
        let named = parse_text(
            r#"
            active_catalogs = ["all"]
            [catalogs.a]
            roots = ["/a"]
            "#,
        );
        assert!(
            named.active_specified,
            "explicit [\"all\"] must be a choice"
        );
        assert_eq!(named.catalogs.active(), &crate::catalog::Active::All);

        let silent = parse_text(
            r#"
            [catalogs.a]
            roots = ["/a"]
            "#,
        );
        assert!(!silent.active_specified, "an absent key is not a choice");
        // Same resulting value, which is exactly why the flag has to be
        // separate from it.
        assert_eq!(silent.catalogs.active(), &crate::catalog::Active::All);
    }

    #[test]
    fn a_named_subset_is_also_a_choice() {
        let loaded = parse_text(
            r#"
            active_catalogs = ["a"]
            [catalogs.a]
            roots = ["/a"]
            [catalogs.b]
            roots = ["/b"]
            "#,
        );
        assert!(loaded.active_specified);
        assert_eq!(loaded.catalogs.active_names(), vec!["a"]);
    }

    /// Top-level keys must precede tables in TOML. Worth a test because the
    /// failure is confusing: the key silently joins the preceding table.
    #[test]
    fn a_top_level_key_after_a_table_is_rejected_clearly() {
        let loaded = parse_text(
            r#"
            [catalogs.a]
            roots = ["/a"]
            active_catalogs = ["a"]
            "#,
        );
        assert_eq!(loaded.errors.len(), 1, "{:?}", loaded.errors);
        assert!(
            loaded.errors[0].message.contains("active_catalogs"),
            "{:?}",
            loaded.errors[0]
        );
    }

    #[test]
    fn library_roots_become_the_default_catalog() {
        let loaded = parse_text(r#"library = ["/music"]"#);
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        assert_eq!(loaded.catalogs.defined().len(), 1);
        assert_eq!(loaded.catalogs.defined()[0].name, crate::catalog::DEFAULT);
    }
}
