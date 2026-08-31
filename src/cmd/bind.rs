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
    // Folds only, at every level. `h` on something already closed closes its
    // parent, so repeated `h` walks up and out — snacks' rule.
    // `l` is a second binding for `activate`, not a command of its own —
    // snacks binds it to `confirm` too. `h` keeps its own, because closing is
    // one act while opening bottoms out in playing the track.
    "bind common l activate",
    "bind common h fold close",
    // vim's fold vocabulary, whole. The levels here are homogeneous — every
    // node at depth 0 is an artist — so `zr`/`zm` mean "expand to albums" and
    // "collapse to artists", which is an operation a file explorer cannot
    // offer and does not implement.
    "bind common \"z o\" fold open",
    "bind common \"z c\" fold close",
    "bind common \"z a\" fold toggle",
    "bind common \"z O\" fold open-deep",
    "bind common \"z C\" fold close-deep",
    "bind common \"z R\" fold all",
    "bind common \"z M\" fold none",
    "bind common \"z r\" fold more",
    "bind common \"z m\" fold less",
    // `o` for open, not `space`: space is the leader, and a chord cannot
    // extend a key that is bound on its own. It is `za` under another name,
    // and outside the tree it is what ticks a checkbox.
    "bind common o fold toggle",
    "bind common enter activate",
    // vim's `dd`, and `D` for delete-to-end. `D` alone was cmus's, and sat
    // oddly in a keymap whose navigation is otherwise nvim's — in vim `D`
    // deletes to the end of a line, never the line itself.
    "bind common \"d d\" remove",
    "bind common D remove-to-end",
    // `u` is undo, as in vim. Rescan moved behind the leader: it is rare, and
    // `:rescan` is how it is really reached.
    "bind common u undo",
    "bind common ^r redo",
    "bind common \"<leader> u\" rescan",
    // `Z` was snacks' close-all, and `M-e` its opposite. Both are gone: `zM`
    // and `zR` say the same thing in the vocabulary the rest of the keymap
    // already speaks, and `zm`/`zr` add the graduated version neither had.
    // transport
    // Transport is `<leader>`-prefixed in normal mode. The letters are
    // Winamp's row, unchanged — what changes is that reaching them says you
    // are stepping outside browsing, which is what you are doing in view 1.
    // #33 adds the mode where they are bare again; the letters stay the same,
    // so there is one alphabet rather than two.
    //
    // It frees `z` for vim's fold vocabulary and `-` for the parent motion,
    // which is the immediate reason to do it now.
    "bind common \"<leader> x\" play",
    "bind common \"<leader> c\" pause",
    "bind common \"<leader> v\" stop",
    "bind common \"<leader> b\" next",
    "bind common \"<leader> z\" prev",
    "bind common \"<leader> >\" seek +60",
    "bind common \"<leader> <\" seek -60",
    "bind common \"<leader> +\" vol +5%",
    "bind common \"<leader> =\" vol +5%",
    "bind common \"<leader> -\" vol -5%",
    // The arrows stay bare: nothing else wants them, and seeking with them is
    // what every player does.
    "bind common right seek +10",
    "bind common left seek -10",
    // modes
    "bind common s toggle shuffle",
    "bind common r toggle repeat",
    "bind common R toggle repeat_current",
    "bind common C toggle continue",
    "bind common m toggle aaa_mode",
    // library
    // `a` is the common one, so it gets the obvious key. `y` went to yank,
    // which is what it means in vim, and adding to the *library* is rare —
    // it is how view 5 reaches music the library does not have yet.
    "bind common a add-playlist",
    "bind common A add",
    "bind common e add-queue",
    // vim's registers. `yy`/`Y` mirror `dd`/`D`, and the register lives on the
    // Ui rather than on a playlist, exactly as vim's are global — that is what
    // makes yank here, open another playlist, put there work.
    "bind common \"y y\" yank",
    "bind common Y yank-to-end",
    "bind common p put",
    "bind common P put-before",
    // search and quit
    "bind common n search-next",
    "bind common N search-prev",
    "bind common q quit",
    // The leader's first tenant, and the reason it exists: `?` is reverse
    // search, as in vim, so help needed somewhere else to live.
    "bind common \"<leader> ?\" help",
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
