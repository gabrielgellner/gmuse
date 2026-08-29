//! The binding table — DESIGN §6.
//!
//! Lookup falls back from the current context to [`Context::Common`], so a
//! view only has to name the keys that differ from the global set.

use std::collections::HashMap;

use super::keys::{self, Key, Seq};
use super::{Command, Context, parse};

pub type Bindings = HashMap<(Context, Seq), Command>;

/// What a partially typed key sequence means so far.
#[derive(Debug, PartialEq)]
pub enum Resolved<'a> {
    /// A complete binding. Run it and clear the pending keys.
    Exact(&'a Command),
    /// The start of at least one longer binding. Keep waiting.
    Prefix,
    /// Nothing can match. Clear the pending keys.
    None,
}

/// The default bindings, written in rc-file syntax.
///
/// Deliberately text rather than constructed values: the defaults then go
/// through exactly the same parser a user's rc file does, so a default that
/// could not be expressed in the file — or that the parser gets wrong — fails
/// a test instead of shipping.
pub const DEFAULTS: &[&str] = &[
    // views — nvim tab style, because a count prefix needs the digits
    "bind common \"g t\" view-next",
    "bind common \"g T\" view-prev",
    "bind common \"g tab\" view-last",
    // navigation
    "bind common j down",
    "bind common k up",
    "bind common down down",
    "bind common up up",
    "bind common \"g g\" top",
    "bind common G bottom",
    "bind common home top",
    "bind common end bottom",
    "bind common ^f page-down",
    "bind common ^b page-up",
    "bind common pagedown page-down",
    "bind common pageup page-up",
    "bind common tab pane-next",
    "bind common h pane-left",
    "bind common l pane-right",
    // Drive the tree from wherever you are, so stepping album to album does
    // not need a Tab over and a Tab back each time.
    "bind library ^j in tree down",
    "bind library ^k in tree up",
    "bind common space expand",
    "bind common enter activate",
    "bind common D remove",
    "bind common u rescan",
    "bind common M-e expand-all",
    "bind common M-c collapse-all",
    // transport
    "bind common x play",
    "bind common c pause",
    "bind common v stop",
    "bind common b next",
    "bind common z prev",
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

/// Find the command for a key sequence, falling back to `Common`.
pub fn lookup<'a>(bindings: &'a Bindings, ctx: Context, keys: &[Key]) -> Option<&'a Command> {
    bindings
        .get(&(ctx, keys.to_vec()))
        .or_else(|| bindings.get(&(Context::Common, keys.to_vec())))
}

/// Resolve a partially typed sequence.
///
/// An exact match wins even when longer bindings share it as a prefix — `G`
/// must fire immediately rather than waiting to see whether a `G something`
/// binding was meant. Bind chords that extend an existing key only if you are
/// willing to lose the shorter one.
pub fn resolve<'a>(bindings: &'a Bindings, ctx: Context, keys: &[Key]) -> Resolved<'a> {
    if let Some(cmd) = lookup(bindings, ctx, keys) {
        return Resolved::Exact(cmd);
    }
    let is_prefix = bindings.keys().any(|(c, seq)| {
        (*c == ctx || *c == Context::Common) && seq.len() > keys.len() && seq.starts_with(keys)
    });
    if is_prefix {
        Resolved::Prefix
    } else {
        Resolved::None
    }
}

/// Every binding, sorted for display in view 7.
///
/// Sorted by context then key *name*, because `KeyCode` has no meaningful
/// ordering of its own and a settings view that reshuffles itself between
/// frames would be unusable.
pub fn sorted(bindings: &Bindings) -> Vec<(Context, String, &Command)> {
    let mut rows: Vec<(Context, String, &Command)> = bindings
        .iter()
        .map(|((ctx, seq), cmd)| (*ctx, keys::format_seq(seq), cmd))
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
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
        let key = vec![keys::Key::new(KeyCode::Char('j'))];

        // Nothing library-specific: the common binding answers.
        assert_eq!(
            lookup(&bindings, Context::Library, &key),
            Some(&Command::Down)
        );

        // A context-specific binding wins over it.
        bindings.insert((Context::Library, key.clone()), Command::Next);
        assert_eq!(
            lookup(&bindings, Context::Library, &key),
            Some(&Command::Next)
        );
        // ...and only in that context.
        assert_eq!(
            lookup(&bindings, Context::Queue, &key),
            Some(&Command::Down)
        );
    }

    #[test]
    fn unbound_keys_return_nothing() {
        let bindings = defaults();
        assert!(lookup(&bindings, Context::Common, &[keys::Key::new(KeyCode::F(9))]).is_none());
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
            .map(|(ctx, key, cmd)| {
                let quoted = if key.contains(' ') {
                    format!("\"{key}\"")
                } else {
                    key.clone()
                };
                format!("bind {} {} {}", ctx.name(), quoted, parse::format(cmd))
            })
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
        let a: Vec<String> = sorted(&bindings).iter().map(|r| r.1.clone()).collect();
        let b: Vec<String> = sorted(&bindings).iter().map(|r| r.1.clone()).collect();
        assert_eq!(a, b, "view 7 would reshuffle between frames");
    }
}
