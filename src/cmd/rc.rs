//! The rc file: a list of commands run at startup.
//!
//! Same syntax as the `:` line, because §6 makes them one mechanism. A line
//! that fails is reported and skipped rather than aborting the file — one
//! typo in a config should not cost every binding after it.

use std::path::{Path, PathBuf};

use super::{Command, parse};

/// `$XDG_CONFIG_HOME/gmuse/rc`, falling back to `~/.config`.
pub fn default_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("gmuse").join("rc")
}

/// A problem with one line, reported with its number so it can be found.
#[derive(Debug, Clone, PartialEq)]
pub struct RcError {
    pub line: usize,
    pub text: String,
    pub message: String,
}

impl std::fmt::Display for RcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}: {}", self.line, self.message, self.text)
    }
}

/// Parse rc text into commands, collecting per-line errors.
pub fn parse_text(text: &str) -> (Vec<Command>, Vec<RcError>) {
    let mut commands = Vec::new();
    let mut errors = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        match parse::line(raw) {
            Ok(Some(cmd)) => commands.push(cmd),
            Ok(None) => {}
            Err(e) => errors.push(RcError {
                line: i + 1,
                text: raw.trim().to_string(),
                message: e.to_string(),
            }),
        }
    }
    (commands, errors)
}

/// Read and parse the rc file. A missing file is not an error.
pub fn load(path: &Path) -> (Vec<Command>, Vec<RcError>) {
    match std::fs::read_to_string(path) {
        Ok(text) => parse_text(&text),
        Err(_) => (Vec::new(), Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::{Context, SettingKey, keys};
    use ratatui::crossterm::event::KeyCode;

    #[test]
    fn reads_commands_and_skips_comments() {
        let (cmds, errs) = parse_text(
            "# my config\n\
             \n\
             set shuffle=true\n\
             bind common ^n player-next\n",
        );
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(cmds.len(), 2);
        assert_eq!(
            cmds[0],
            Command::Set {
                key: SettingKey::Shuffle,
                value: crate::cmd::SettingValue::Bool(true)
            }
        );
        assert_eq!(
            cmds[1],
            Command::Bind {
                ctx: Context::Common,
                key: keys::Key::ctrl(KeyCode::Char('n')),
                cmd: Box::new(Command::PlayerNext),
            }
        );
    }

    /// One bad line must not cost the rest of the file.
    #[test]
    fn a_bad_line_is_reported_and_the_rest_still_loads() {
        let (cmds, errs) = parse_text(
            "set shuffle=true\n\
             frobnicate the widget\n\
             set repeat=true\n",
        );
        assert_eq!(cmds.len(), 2, "later lines were dropped");
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].line, 2);
        assert!(errs[0].message.contains("frobnicate"), "{:?}", errs[0]);
    }

    #[test]
    fn a_missing_file_is_not_an_error() {
        let (cmds, errs) = load(Path::new("/definitely/not/here/rc"));
        assert!(cmds.is_empty());
        assert!(errs.is_empty());
    }
}
