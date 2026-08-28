//! The binding table — DESIGN §6.
//!
//! Lookup falls back from the current context to [`Context::Common`], so a
//! view only has to name the keys that differ from the global set.

use std::collections::HashMap;

use super::keys::Key;
use super::{Command, Context, parse};

pub type Bindings = HashMap<(Context, Key), Command>;

/// The default bindings, written in rc-file syntax.
///
/// Deliberately text rather than constructed values: the defaults then go
/// through exactly the same parser a user's rc file does, so a default that
/// could not be expressed in the file — or that the parser gets wrong — fails
/// a test instead of shipping.
pub const DEFAULTS: &[&str] = &[
    // views
    "bind common 1 view 1",
    "bind common 2 view 2",
    "bind common 3 view 3",
    "bind common 4 view 4",
    "bind common 5 view 5",
    "bind common 6 view 6",
    "bind common 7 view 7",
    // navigation
    "bind common j win-down",
    "bind common k win-up",
    "bind common down win-down",
    "bind common up win-up",
    "bind common g win-top",
    "bind common G win-bottom",
    "bind common home win-top",
    "bind common end win-bottom",
    "bind common ^f win-page-down",
    "bind common ^b win-page-up",
    "bind common pagedown win-page-down",
    "bind common pageup win-page-up",
    "bind common tab win-next-pane",
    "bind common space win-toggle",
    "bind common enter win-activate",
    "bind common D win-remove",
    "bind common u win-update",
    "bind common M-e win-expand-all",
    "bind common M-c win-collapse-all",
    // transport
    "bind common x player-play",
    "bind common c player-pause",
    "bind common v player-stop",
    "bind common b player-next",
    "bind common z player-prev",
    "bind common right seek +10",
    "bind common left seek -10",
    "bind common > seek +60",
    "bind common < seek -60",
    "bind common + vol +5%",
    "bind common = vol +5%",
    "bind common - vol -5%",
    // modes
    "bind common s toggle shuffle",
    "bind common r toggle repeat",
    "bind common R toggle repeat_current",
    "bind common C toggle continue",
    "bind common m toggle aaa_mode",
    // library
    "bind common a add",
    "bind common y add-playlist",
    "bind common e add-queue",
    // search and quit
    "bind common n search-next",
    "bind common N search-prev",
    "bind common q quit",
];

/// Build the default table.
///
/// Panics if a default fails to parse — that is a bug in this file, and
/// failing at startup beats shipping a table with a hole in it. The test
/// below is what actually catches it.
pub fn defaults() -> Bindings {
    let mut bindings = Bindings::new();
    for line in DEFAULTS {
        match parse::command(line) {
            Ok(Command::Bind { ctx, key, cmd }) => {
                bindings.insert((ctx, key), *cmd);
            }
            Ok(other) => panic!("default {line:?} is not a bind: {other:?}"),
            Err(e) => panic!("default {line:?} failed to parse: {e}"),
        }
    }
    bindings
}

/// Find the command for a key, falling back to `Common`.
pub fn lookup(bindings: &Bindings, ctx: Context, key: Key) -> Option<&Command> {
    bindings
        .get(&(ctx, key))
        .or_else(|| bindings.get(&(Context::Common, key)))
}

/// Every binding, sorted for display in view 7.
///
/// Sorted by context then key *name*, because `KeyCode` has no meaningful
/// ordering of its own and a settings view that reshuffles itself between
/// frames would be unusable.
pub fn sorted(bindings: &Bindings) -> Vec<(Context, Key, &Command)> {
    let mut rows: Vec<(Context, Key, &Command)> = bindings
        .iter()
        .map(|((ctx, key), cmd)| (*ctx, *key, cmd))
        .collect();
    rows.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.to_string().cmp(&b.1.to_string()))
    });
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::keys;
    use ratatui::crossterm::event::KeyCode;

    /// The defaults are text, so they are only correct if the parser agrees.
    #[test]
    fn every_default_parses() {
        for line in DEFAULTS {
            let parsed =
                parse::command(line).unwrap_or_else(|e| panic!("default {line:?} failed: {e}"));
            assert!(
                matches!(parsed, Command::Bind { .. }),
                "{line:?} is not a bind"
            );
        }
        assert_eq!(
            defaults().len(),
            DEFAULTS.len(),
            "a default was overwritten"
        );
    }

    #[test]
    fn lookup_falls_back_to_common() {
        let mut bindings = defaults();
        let key = keys::Key::new(KeyCode::Char('j'));

        // Nothing library-specific: the common binding answers.
        assert_eq!(
            lookup(&bindings, Context::Library, key),
            Some(&Command::WinDown)
        );

        // A context-specific binding wins over it.
        bindings.insert((Context::Library, key), Command::PlayerNext);
        assert_eq!(
            lookup(&bindings, Context::Library, key),
            Some(&Command::PlayerNext)
        );
        // ...and only in that context.
        assert_eq!(
            lookup(&bindings, Context::Queue, key),
            Some(&Command::WinDown)
        );
    }

    #[test]
    fn unbound_keys_return_nothing() {
        let bindings = defaults();
        assert!(lookup(&bindings, Context::Common, keys::Key::new(KeyCode::F(9))).is_none());
    }

    /// §9's M6 criterion: a rebinding must survive the rc file.
    #[test]
    fn a_rebinding_round_trips_through_rc_text() {
        let mut bindings = defaults();

        // Rebind, as a user would.
        let Command::Bind { ctx, key, cmd } = parse::command("bind library J seek +30").unwrap()
        else {
            panic!("not a bind")
        };
        bindings.insert((ctx, key), *cmd);

        // Write the whole table out the way view 7 and a saved rc would.
        let text: Vec<String> = sorted(&bindings)
            .iter()
            .map(|(ctx, key, cmd)| format!("bind {} {} {}", ctx.name(), key, parse::format(cmd)))
            .collect();

        // Read it back.
        let mut reloaded = Bindings::new();
        for line in &text {
            let Command::Bind { ctx, key, cmd } = parse::command(line).unwrap() else {
                panic!("{line:?} is not a bind")
            };
            reloaded.insert((ctx, key), *cmd);
        }

        assert_eq!(reloaded.len(), bindings.len());
        for (k, v) in &bindings {
            assert_eq!(
                reloaded.get(k),
                Some(v),
                "{k:?} changed across the round trip"
            );
        }
    }

    #[test]
    fn sorted_is_stable() {
        let bindings = defaults();
        let a: Vec<String> = sorted(&bindings).iter().map(|r| r.1.to_string()).collect();
        let b: Vec<String> = sorted(&bindings).iter().map(|r| r.1.to_string()).collect();
        assert_eq!(a, b, "view 7 would reshuffle between frames");
    }
}
