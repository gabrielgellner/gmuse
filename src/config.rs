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
    /// `[keys.common] j = "down"`.
    #[serde(default)]
    pub keys: HashMap<String, HashMap<String, Binding>>,
    #[serde(default)]
    pub theme: ThemeTable,
    /// `[format] now_playing = "{artist} — {title}"`.
    #[serde(default)]
    pub format: FormatTable,
    /// Commands run once at startup, after everything else is applied.
    #[serde(default)]
    pub startup_commands: Vec<String>,
    /// `[catalogs.general]` — **gone, and kept only to say so** (#119).
    ///
    /// A catalog was a saved view with a second name, and it lived in the
    /// config where it could not sync, merge or carry provenance. Parsed here
    /// purely so an old config produces a migration message instead of
    /// silently doing nothing.
    #[serde(default)]
    pub catalogs: HashMap<String, toml::Value>,
    /// Also gone. Same reason, same message.
    #[serde(default)]
    pub active_catalogs: Vec<String>,
    /// `view = 'in="Dance"'` — the view in force at startup.
    ///
    /// One source line, because that is all a view is. Wins over what the
    /// last session remembered, which is the same rule every other setting
    /// follows: the config speaks, the session fills the rest.
    #[serde(default)]
    pub view: Option<String>,
    /// `[playlists.fresh] where = "days_since_added<30"` — smart playlists.
    ///
    /// Beside the catalogs because both are named expressions living in the
    /// config; what separates them is what they *do* with the expression, and
    /// that is documented on [`crate::smart`].
    #[serde(default)]
    pub playlists: HashMap<String, PlaylistTable>,
    /// Where everything *you* have decided about your music lives — ratings,
    /// play history, and playlists once #22 lands.
    ///
    /// A setting rather than a fixed path because this is the half worth
    /// backing up, and people back up different places. Pointing it inside the
    /// library makes ratings travel with the music. It cannot *default* there:
    /// gmuse has no concept of "the library folder" — roots may be several, and
    /// on different disks, and a playlist crosses them by design.
    ///
    /// The session and the audit log stay in `$XDG_STATE_HOME`: losing where
    /// you were is not a loss.
    #[serde(default)]
    pub data_dir: Option<PathBuf>,
    /// The key `<leader>` stands for in a binding. Default `space`.
    ///
    /// A config key rather than a `:set`, because changing it would have to
    /// rewrite every binding already resolved through it.
    #[serde(default)]
    pub leader: Option<String>,
    /// Shelf roots, for when you do not want named catalogs.
    ///
    /// The durable home for roots: the cache is derived data and is thrown
    /// away whenever its format changes, so roots that live only there are
    /// lost on an upgrade.
    #[serde(default)]
    pub library: Vec<PathBuf>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlaylistTable {
    /// A §7 filter expression. Required: a smart playlist with no rule would
    /// be the whole library under another name.
    #[serde(default, rename = "where")]
    pub filter: Option<String>,
    /// A sort order. Omitted means the library's own — see
    /// [`crate::query::sort::Sort::library_order`].
    #[serde(default)]
    pub sort: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FormatTable {
    /// How a track row reads in the library tree.
    ///
    /// Its own template because a track sits beside an artist row there and
    /// has to look different — the same job a file explorer gives icons.
    pub track: Option<String>,
    /// The now-playing description in the status line. The state icon, times
    /// and mode flags around it stay structured, so they keep their own theme
    /// roles rather than being flattened into one string.
    pub now_playing: Option<String>,
    /// The transport panel's headline (#34). Its own key rather than reusing
    /// `now_playing`, because the panel has a whole line where the status
    /// line has whatever is left after the times and the mode flags — the
    /// point of the panel is that there is room for more.
    pub transport: Option<String>,
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
    /// Parsed once, here, so the status line never parses while rendering.
    pub now_playing: crate::query::format::Template,
    /// The transport panel's headline, same discipline.
    pub transport: crate::query::format::Template,
    /// How a track row reads in the library tree.
    pub track_line: Option<crate::query::format::Template>,
    /// Where the music is, from `library`. **The only source of scan roots**
    /// now that a catalog has none (#94).
    pub library: Vec<PathBuf>,
    /// The view in force at startup, as its source line (#119).
    pub view: Option<String>,
    pub playlists: crate::smart::SmartPlaylists,
    /// Resolved: the config's `data_dir` with `~` expanded, or the default.
    pub data_dir: PathBuf,
    /// Resolved from `leader`, defaulting to `space`.
    ///
    /// `Option` only so `Loaded` can derive `Default`; `parse_text` and `load`
    /// both fill it in, so nothing downstream sees `None`.
    pub leader: Option<crate::cmd::keys::Key>,
    // **`active_specified` lived here and is gone** (#119). It existed
    // because `["all"]` was an explicit choice indistinguishable from the
    // default once folded into an `Active`. A source line has no such
    // ambiguity: `Some("")` is not `None`.
    pub errors: Vec<ConfigError>,
}

/// Parse config text into commands and a theme.
///
/// Every problem is collected rather than fatal: one bad line should not cost
/// the rest of the file.
pub fn parse_text(text: &str) -> Loaded {
    let mut out = Loaded {
        theme: Theme::default(),
        transport: crate::query::format::Template::transport_default(),
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

    // **Two spellings of one chord are a duplicate, and TOML cannot see it** —
    // #227. A repeated key *name* is a TOML error before this runs, but
    // `space` and `<leader>` are different names for the same sequence, and the
    // binding table is a map, so the loser vanished with nothing said. Keyed by
    // the parsed sequence, and recorded per TOML entry rather than per emitted
    // command — a list value is one entry that emits several, so counting
    // commands would report every list as a duplicate of itself (#266).
    let mut bound: std::collections::HashMap<(Context, keys::Seq), String> =
        std::collections::HashMap::new();

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
            if let Some(first) = bound.insert((ctx, key.clone()), key_name.clone()) {
                out.errors.push(ConfigError {
                    where_: format!("keys.{ctx_name}.{key_name}"),
                    message: format!(
                        "{} is already bound in this context as `{first}`",
                        keys::format_seq(&key)
                    ),
                });
            }
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

    // Parsed here, at the boundary, so a bad template is a reported config
    // problem rather than something the status line discovers per frame.
    if let Some(source) = &config.format.now_playing {
        match crate::query::format::Template::parse(source) {
            Ok(t) => out.now_playing = t,
            Err(e) => out.errors.push(ConfigError {
                where_: "format.now_playing".into(),
                message: e.to_string(),
            }),
        }
    }
    if let Some(source) = &config.format.transport {
        match crate::query::format::Template::parse(source) {
            Ok(t) => out.transport = t,
            Err(e) => out.errors.push(ConfigError {
                where_: "format.transport".into(),
                message: e.to_string(),
            }),
        }
    }
    if let Some(source) = &config.format.track {
        match crate::query::format::Template::parse(source) {
            Ok(t) => out.track_line = Some(t),
            Err(e) => out.errors.push(ConfigError {
                where_: "format.track".into(),
                message: e.to_string(),
            }),
        }
    }

    out.library = config.library.iter().map(|r| expand_tilde(r)).collect();

    // **A config written the old way says so** (#119). Silently ignoring a
    // `[catalogs.*]` table would leave somebody with an empty view 6 and no
    // idea why.
    let mut old: Vec<&String> = config.catalogs.keys().collect();
    old.sort();
    for name in old {
        out.errors.push(ConfigError {
            where_: format!("catalogs.{name}"),
            message: "catalogs moved into the library — compose the view and \
                      `:view save <name>`, then use `view = 'saved=\"<name>\"'`"
                .into(),
        });
    }

    // Resolved here, at the boundary: nothing downstream sees a `~` or has to
    // know what the default was.
    out.data_dir = config
        .data_dir
        .as_deref()
        .map(expand_tilde)
        .unwrap_or_else(default_data_dir);

    out.leader = Some(match config.leader.as_deref() {
        None => default_leader(),
        Some(text) => match crate::cmd::keys::parse_seq(text).as_deref() {
            Ok([one]) => *one,
            // A leader is one key: a chord could not be extended, and a bad
            // name should not silently leave `<leader>` meaning space.
            _ => {
                out.errors.push(ConfigError {
                    where_: "leader".into(),
                    message: format!("{text:?} is not a single key"),
                });
                default_leader()
            }
        },
    });

    if !config.active_catalogs.is_empty() {
        out.errors.push(ConfigError {
            where_: "active_catalogs".into(),
            message: "replaced by `view` — one source line, e.g. view = 'saved=\"dance\"'".into(),
        });
    }
    out.view = config.view.clone();

    // Smart playlists. Sorted for the same reason catalogs are: a HashMap's
    // order is arbitrary and completion reads better in a stable one.
    let mut playlists: Vec<(&String, &PlaylistTable)> = config.playlists.iter().collect();
    playlists.sort_by(|a, b| a.0.cmp(b.0));
    for (name, table) in playlists {
        let Some(src) = table.filter.as_deref() else {
            out.errors.push(ConfigError {
                where_: format!("playlists.{name}"),
                message: "has no where — a smart playlist is its rule".into(),
            });
            continue;
        };
        // Both languages parse here, at the boundary, so a bad expression
        // names the playlist it came from rather than failing in a redraw.
        let filter = match crate::query::filter::Filter::parse(src) {
            Ok(f) => f,
            Err(e) => {
                out.errors.push(ConfigError {
                    where_: format!("playlists.{name}.where"),
                    message: e.to_string(),
                });
                continue;
            }
        };
        let sort = match table.sort.as_deref().map(crate::query::sort::Sort::parse) {
            Some(Ok(s)) => Some(s),
            None => None,
            Some(Err(e)) => {
                out.errors.push(ConfigError {
                    where_: format!("playlists.{name}.sort"),
                    message: e.to_string(),
                });
                continue;
            }
        };
        out.playlists.define(name, filter, sort);
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

/// Space, as in vim and nvim, where it is the conventional leader.
pub fn default_leader() -> crate::cmd::keys::Key {
    crate::cmd::keys::Key::new(crossterm::event::KeyCode::Char(' '))
}

/// `$XDG_STATE_HOME/gmuse`, falling back to `~/.local/state`.
///
/// Where you were, not what you decided — the session, the audit log, and the
/// machine id. Deliberately *not* synced, which is what makes the id per
/// machine rather than shared.
pub fn default_state_dir() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("state")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("gmuse")
}

/// `$XDG_DATA_HOME/gmuse`, falling back to `~/.local/share`.
///
/// Data rather than state, and the distinction is worth: state is where you
/// were, data is what you decided. A rating cannot be recovered by using the
/// program again.
pub fn default_data_dir() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("gmuse")
}

/// Expand a leading `~`.
///
/// Shared with `cli.rs` rather than duplicated: a config root and a `--library`
/// root are the same kind of thing, and having only one of them expand is
/// exactly the surprise #21 recorded.
pub(crate) fn expand_tilde(path: &Path) -> PathBuf {
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
        // No config file at all still needs the defaults resolved — a `Loaded`
        // with an empty `data_dir` would write the history to the process's
        // working directory.
        Err(_) => Loaded {
            theme: Theme::default(),
            data_dir: default_data_dir(),
            leader: Some(default_leader()),
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
            scope = "album"
            "#,
        );
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        assert!(loaded.commands.contains(&Command::Set {
            key: SettingKey::Shuffle,
            value: SettingValue::Bool(true)
        }));
        assert!(loaded.commands.contains(&Command::Set {
            key: SettingKey::Scope,
            value: SettingValue::Scope(crate::next::Scope::Inner)
        }));
    }

    #[test]
    fn a_key_holds_a_command_string() {
        let loaded = parse_text(
            r#"
            [keys.common]
            "^n" = "next"
            "#,
        );
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        assert_eq!(loaded.commands.len(), 1);
        let Command::Bind { ctx, key, cmd } = &loaded.commands[0] else {
            panic!("not a bind")
        };
        assert_eq!(*ctx, Context::Common);
        assert_eq!(keys::format_seq(key), "^n");
        assert_eq!(**cmd, Command::Next);
    }

    /// **Two spellings of one chord are a duplicate** — #227.
    ///
    /// TOML rejects a repeated key name before this runs, so the only duplicate
    /// that can reach here is one written two ways — and `keys::parse` has real
    /// aliases (`^x`/`C-x`, `enter`/`return`, `pgup`/`pageup`) that make it an
    /// ordinary mistake rather than a contrived one. The binding table is a map,
    /// so the loser used to vanish with nothing said.
    ///
    /// **The first half is the instrument.** Both lines here are individually
    /// valid — a good command, a parseable chord — so a test that only checked
    /// "some error" would pass on an implementation that rejected either one for
    /// the wrong reason.
    #[test]
    fn one_chord_written_two_ways_is_reported_not_silently_overwritten() {
        let fine = parse_text(
            r#"
            [keys.library]
            "^x" = "play"
            "^y" = "pause"
            "#,
        );
        assert!(
            fine.errors.is_empty(),
            "two different chords are not a duplicate: {:?}",
            fine.errors
        );
        assert_eq!(fine.commands.len(), 2, "both should bind");

        let clash = parse_text(
            r#"
            [keys.library]
            "^x" = "play"
            "C-x" = "pause"
            "#,
        );
        assert_eq!(
            clash.errors.len(),
            1,
            "one chord under two spellings should be reported once: {:?}",
            clash.errors
        );
        let said = clash.errors[0].to_string();
        assert!(
            said.contains("already bound"),
            "the error should say what is wrong: {said}"
        );
        assert!(
            said.contains("^x") && said.contains("C-x"),
            "the error should name both spellings so it can be found: {said}"
        );
    }

    /// A list is one entry, so it is not a duplicate of itself — #266.
    #[test]
    fn a_list_is_not_reported_as_a_duplicate() {
        let loaded = parse_text(
            r#"
            [keys.library]
            o = ["add-queue", "next"]
            "#,
        );
        assert!(
            loaded.errors.is_empty(),
            "a list value was mistaken for a repeated binding: {:?}",
            loaded.errors
        );
        assert_eq!(loaded.commands.len(), 2, "both commands should still bind");
    }

    /// A list value is a macro. That falls out of TOML for free, and it is the
    /// main thing the flat rc file could not express.
    #[test]
    fn a_list_binds_several_commands_to_one_key() {
        let loaded = parse_text(
            r#"
            [keys.library]
            o = ["add-queue", "next"]
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
            j = "down"

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
            Command::Bind { cmd, .. } if **cmd == Command::Down
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
    /// A track row sits beside an artist row in the tree, so it gets its own
    /// template — and a bad one names the key it came from.
    #[test]
    fn the_track_template_is_configurable() {
        let loaded = parse_text("[format]\ntrack = \"{artist} — {title}\"\n");
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        assert!(loaded.track_line.is_some());

        // Omitted: the built-in stays, rather than rendering nothing.
        assert!(parse_text("").track_line.is_none());

        let bad = parse_text("[format]\ntrack = \"{nonsense}\"\n");
        assert_eq!(bad.errors[0].where_, "format.track");
    }

    #[test]
    fn data_dir_defaults_and_expands_a_tilde() {
        // Absent: the default, never an empty path — that would write the
        // history into whatever directory gmuse happened to start in.
        let bare = parse_text("");
        assert_eq!(bare.data_dir, default_data_dir());
        assert!(bare.data_dir.is_absolute() || bare.data_dir.starts_with("."));

        let home = std::env::var("HOME").expect("HOME");
        let set = parse_text(r#"data_dir = "~/MusicLibrary/.gmuse""#);
        assert_eq!(
            set.data_dir,
            PathBuf::from(&home).join("MusicLibrary/.gmuse"),
            "a config `~` must expand, as it does for library and catalog roots"
        );
        assert!(set.errors.is_empty(), "{:?}", set.errors);
    }

    /// A missing config file still has to resolve its defaults.
    #[test]
    fn a_missing_config_still_has_a_data_dir() {
        let loaded = load(Path::new("/nonexistent/gmuse/config.toml"));
        assert_eq!(loaded.data_dir, default_data_dir());
    }

    #[test]
    fn playlist_tables_become_smart_playlists() {
        let loaded = parse_text(
            r#"
            [playlists.fresh]
            where = "days_since_added<30"
            sort = "-added"

            [playlists.neglected]
            where = "play_count=0"
            "#,
        );
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        assert_eq!(loaded.playlists.names(), ["fresh", "neglected"]);

        let fresh = loaded.playlists.get("fresh").expect("defined");
        assert_eq!(fresh.filter.source, "days_since_added<30");
        assert_eq!(fresh.sort.source, "-added");
        // An omitted sort falls back rather than erroring.
        assert_eq!(
            loaded.playlists.get("neglected").unwrap().sort,
            crate::query::sort::Sort::library_order()
        );
    }

    /// Both languages parse at the boundary, so a bad expression names the
    /// playlist and the key it came from rather than failing during a redraw.
    #[test]
    fn a_bad_playlist_names_where_it_came_from() {
        let no_rule = parse_text("[playlists.a]\nsort = \"artist\"\n");
        assert_eq!(no_rule.errors[0].where_, "playlists.a");
        assert!(no_rule.errors[0].message.contains("no where"));

        let bad_filter = parse_text("[playlists.a]\nwhere = \"nonsense=\"\n");
        assert_eq!(bad_filter.errors[0].where_, "playlists.a.where");

        let bad_sort = parse_text("[playlists.a]\nwhere = \"year>0\"\nsort = \"nope\"\n");
        assert_eq!(bad_sort.errors[0].where_, "playlists.a.sort");
        assert!(bad_sort.errors[0].message.contains("unknown sort key"));

        // A rule using the sort language's date spelling is caught here too,
        // with the message that names the right one.
        let crossed = parse_text("[playlists.a]\nwhere = \"added>3\"\n");
        assert!(
            crossed.errors[0].message.contains("days_since_added"),
            "{:?}",
            crossed.errors
        );
    }

    /// **A config written the old way says so rather than doing nothing**
    /// (#119).
    ///
    /// Silently ignoring `[catalogs.*]` would leave somebody with an empty
    /// view 6 and no idea why — and both keys have a direct replacement, so
    /// the message can name it.
    #[test]
    fn the_old_catalog_keys_say_where_they_went() {
        let loaded = parse_text(
            r#"
            active_catalogs = ["a"]
            [catalogs.a]
            collection = "A"
            "#,
        );
        let said = |what: &str| {
            loaded
                .errors
                .iter()
                .find(|e| e.where_ == what)
                .unwrap_or_else(|| panic!("{what} said nothing: {:?}", loaded.errors))
        };
        assert!(said("catalogs.a").message.contains(":view save"));
        assert!(said("active_catalogs").message.contains("`view`"));
    }

    /// The view in force is one source line, and the config's wins.
    #[test]
    fn the_config_names_the_view_in_force() {
        let loaded = parse_text("view = 'saved=\"dance\"'");
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        assert_eq!(loaded.view.as_deref(), Some(r#"saved="dance""#));
    }

    #[test]
    fn library_says_where_the_music_is() {
        let loaded = parse_text(r#"library = ["/music"]"#);
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        assert_eq!(loaded.library, vec![PathBuf::from("/music")]);
    }
}
