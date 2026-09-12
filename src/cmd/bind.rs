//! The binding table — DESIGN §6.
//!
//! Lookup falls back from the current context to [`Context::Common`] — unless
//! the context is a *mode* rather than a view; see [`Context::inherits_common`].
//! So a
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
    // `^d`/`^u` rather than `^f`/`^b` is what actually gets used: half a page
    // is the distance at which you can still see where you came from. They
    // recentre, so the cursor stays mid-screen and the text slides past it
    // instead of the cursor drifting to an edge.
    "bind common ^d half-page-down",
    "bind common ^u half-page-up",
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
    "bind common \"z A\" fold toggle-deep",
    "bind common \"z R\" fold all",
    "bind common \"z M\" fold none",
    "bind common \"z r\" fold more",
    "bind common \"z m\" fold less",
    // `zx` is vim's: close every fold, then put back the ones that were
    // holding the cursor. `zv` on its own is not bound because it could never
    // do anything here — the cursor always sits on a row the tree produced.
    "bind common \"z x\" fold only",
    // Where the cursor's row sits in the pane. vim keeps these on `z` because
    // they are about what you can see rather than where you are.
    "bind common \"z t\" place top",
    "bind common \"z z\" place centre",
    "bind common \"z b\" place bottom",
    // Structural motions — #31. `{`/`}` is the one that earns its place:
    // artist-to-artist without scrolling through 200 expanded tracks is the
    // most frequent move there is, and `j`/`k` cannot say it. vim's paragraph
    // motions are unused here and the semantics line up exactly.
    "bind common } go next-sibling",
    "bind common { go prev-sibling",
    "bind common \"] ]\" go next-section",
    "bind common \"[ [\" go prev-section",
    // vim's section motions are four keys, not two: `]]`/`[[` for starts and
    // `][`/`[]` for ends. Shipping half of them left no way to leave a section
    // from the bottom, which is the move you want when the next thing you do
    // is `j` into what follows.
    "bind common \"] [\" go next-section-end",
    "bind common \"[ ]\" go prev-section-end",
    // `-` is netrw's "up a directory", and free now that transport moved
    // behind the leader. `[z` is vim's name for the same move.
    "bind common - go parent",
    "bind common \"[ z\" go parent",
    "bind common \"] z\" go group-end",
    "bind common \"z j\" go next-fold",
    "bind common \"z k\" go prev-fold",
    // "where is the thing I am listening to?" — a question neither the cursor
    // nor a search asks. `g` is already the go-somewhere prefix.
    "bind common \"g p\" playing",
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
    // snacks binds its pickers under `<leader>f`, and the second letter says
    // which. `ff` is the library, `fg` the groups you have made — collections
    // and saved views (#54, #119).
    // vim's window commands, on vim's prefix. `s` and `v` split, `w` moves
    // the focus, `o` keeps only this pane — the four that matter (#97).
    "bind common \"^w s\" split",
    "bind common \"^w v\" vsplit",
    "bind common \"^w w\" other-pane",
    "bind common \"^w o\" only",
    "bind common \"<leader> f f\" find",
    "bind common \"<leader> f g\" find groups",
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
    // ── transport mode (#33) ────────────────────────────────────────────
    // `i` enters, `escape` leaves — vim's arrangement, not inverted. Normal
    // is navigation because the common *interaction* is looking for something
    // to play, even though playback running is the common *state*.
    "bind common i mode transport",
    "bind transport escape mode normal",
    // The letters are the same alphabet as `<leader>x` and friends, bare. One
    // alphabet learned once; only the prefix changes with the mode.
    "bind transport x play",
    "bind transport c pause",
    "bind transport v stop",
    "bind transport b next",
    "bind transport z prev",
    // `space` is the leader in normal mode and cannot be play/pause there.
    // Here it can, and it is the key every player in the world uses.
    "bind transport space pause",
    // What the mode is *for*: seeking gets a row it never had space for.
    // `h`/`l` are the horizontal motions and a track is a horizontal thing.
    "bind transport h seek -10",
    "bind transport l seek +10",
    "bind transport H seek -60",
    "bind transport L seek +60",
    "bind transport left seek -10",
    "bind transport right seek +10",
    // Volume on `j`/`k`, the vertical pair, for the same reason.
    "bind transport j vol -5%",
    "bind transport k vol +5%",
    "bind transport - vol -5%",
    "bind transport + vol +5%",
    "bind transport = vol +5%",
    // vim's `50%` goes half way through a file; here it goes half way through
    // the track. Bare it means halfway, and it takes a count like `12G`.
    "bind transport % seek-percent",
    // Playback modes are transport state, so they keep their letters here too.
    "bind transport s toggle shuffle",
    "bind transport r toggle repeat",
    "bind transport R toggle repeat_current",
    "bind transport C toggle continue",
    "bind transport m toggle scope",
    // A closed keymap has to carry its own way out and its own way to ask.
    // Out of the *mode* is `escape`; out of the *player* is `:q`, and it is
    // deliberately not a keystroke — see the note beside the search bindings.
    "bind transport ? help",
    // modes
    "bind common s toggle shuffle",
    "bind common r toggle repeat",
    "bind common R toggle repeat_current",
    "bind common C toggle continue",
    "bind common m toggle scope",
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
    // `<Esc>` stops the highlighting without losing the search, which is what
    // the local nvim config maps it to and what vim's `:nohlsearch` means.
    "bind common escape nohlsearch",
    "bind common n search-next",
    "bind common N search-prev",
    // **Quitting is `:q`, and not a key.** cmus binds `q`, and a single
    // unmodified letter beside `n` and `p` is one slip from ending a listening
    // session — the one action here with nothing behind it to undo. Two
    // keystrokes and a colon is the cost, and vim charges the same.
    // **`K` is vim's "look up the thing under the cursor"**, which is exactly
    // what this is, and it was unbound. Provisional: #126 says to decide once
    // the panel exists and can be lived with, and a week of using it is worth
    // more than an argument now.
    "bind common K info",
    // The leader's first tenant, and the reason it exists: `?` is reverse
    // search, as in vim, so help needed somewhere else to live.
    "bind common \"<leader> ?\" help",
    // The info panel's own keymap. `escape` leaves it the way you came in —
    // `view-last` *is* "where you were", so there is no second mechanism.
    "bind info escape view-last",
    // **`^o` is vim's jump-back**, and this is a jump list rather than a tree:
    // an artist can be arrived at from a recording, a release group or a
    // concept, and there is no parent to climb to. `h` would promise one.
    "bind info ^o back",
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
    bindings.get(&(ctx, keys.to_vec())).or_else(|| {
        ctx.inherits_common()
            .then(|| bindings.get(&(Context::Common, keys.to_vec())))
            .flatten()
    })
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
        (*c == ctx || (*c == Context::Common && ctx.inherits_common()))
            && seq.len() > keys.len()
            && seq.starts_with(keys)
    });
    if is_prefix {
        Resolved::Prefix
    } else {
        Resolved::None
    }
}

/// How the panel is ordered: by the key case-insensitively, then unshifted
/// before shifted, then the key itself to break the tie.
type Order = (String, bool, String);

/// What one key could be followed by, and what it would then mean.
#[derive(Debug, PartialEq)]
pub enum Next<'a> {
    /// Pressing it runs this.
    Runs(&'a Command),
    /// It is itself a prefix, with this many bindings under it.
    More(usize),
}

/// The keys that can follow a partially typed sequence — which-key's panel.
///
/// Derived from the binding table rather than written down, so a `:bind` takes
/// effect in the panel the moment it takes effect in the keymap. Same reason
/// the pending-key indicator is derived.
///
/// Context shadowing is applied exactly as [`lookup`] applies it: a view's own
/// binding hides the common one, and the panel must not offer both.
pub fn continuations<'a>(
    bindings: &'a Bindings,
    ctx: Context,
    prefix: &[Key],
) -> Vec<(String, Next<'a>)> {
    let mut best: HashMap<&Seq, (Context, &Command)> = HashMap::new();
    for ((c, seq), cmd) in bindings {
        let inherited = *c == Context::Common && ctx.inherits_common();
        if (*c != ctx && !inherited) || seq.len() <= prefix.len() {
            continue;
        }
        if !seq.starts_with(prefix) {
            continue;
        }
        match best.get(seq) {
            Some((held, _)) if *held != Context::Common => {}
            _ => {
                best.insert(seq, (*c, cmd));
            }
        }
    }

    // Case-insensitively, unshifted first on a tie, so `c` is followed by `C`.
    // They are a pair to anyone reading the panel; two separate alphabets —
    // every capital, then every minuscule — is how ASCII orders them and not
    // how a keymap is read.
    let mut groups: std::collections::BTreeMap<Order, Vec<(&Seq, &'a Command)>> =
        std::collections::BTreeMap::new();
    for (seq, (_, cmd)) in best {
        let key = keys::format_seq(&seq[prefix.len()..prefix.len() + 1]);
        let shifted = key.chars().next().is_some_and(char::is_uppercase);
        groups
            .entry((key.to_lowercase(), shifted, key.clone()))
            .or_default()
            .push((seq, cmd));
    }

    groups
        .into_iter()
        .map(|((_, _, key), under)| (key, under))
        .map(|(key, under)| {
            // A key bound on its own wins outright — `resolve` fires it
            // without waiting — so anything extending it is unreachable and
            // showing it as a prefix would be a lie.
            let direct = under
                .iter()
                .find(|(seq, _)| seq.len() == prefix.len() + 1)
                .map(|(_, cmd)| *cmd);
            let next = match direct {
                Some(cmd) => Next::Runs(cmd),
                None => Next::More(under.len()),
            };
            (key, next)
        })
        .collect()
}

/// Every binding, sorted for the Index guide.
///
/// Sorted by context then key *name*, because `KeyCode` has no meaningful
/// ordering of its own and a listing that reshuffled itself between frames
/// would be unusable.
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

    fn seq(text: &str) -> Seq {
        keys::parse_seq(text).unwrap_or_else(|e| panic!("{text:?}: {e}"))
    }

    fn offered(rows: &[(String, Next<'_>)], key: &str) -> bool {
        rows.iter().any(|(k, _)| k == key)
    }

    /// The panel is a list of what pressing one more key would do, so it must
    /// not list keys that belong to some other prefix entirely.
    #[test]
    fn only_what_extends_the_prefix_is_offered() {
        let b = defaults();
        let rows = continuations(&b, Context::Common, &seq("z"));

        assert!(offered(&rows, "o"), "`zo` extends `z`");
        assert!(
            !offered(&rows, "d"),
            "`dd` does not extend `z`, and offering it would be offering a key \
             that does nothing: {rows:?}"
        );
        assert!(!offered(&rows, "y"), "nor does `yy`: {rows:?}");
        assert!(!offered(&rows, "g"), "nor does `gg`: {rows:?}");
    }

    /// A binding that belongs to another view is not reachable from this one,
    /// so the panel must not advertise it.
    #[test]
    fn another_views_bindings_are_not_offered() {
        let mut b = defaults();
        b.insert((Context::Queue, seq("z 9")), Command::Quit);

        let here = continuations(&b, Context::Library, &seq("z"));
        assert!(!offered(&here, "9"), "the queue's binding leaked: {here:?}");

        let there = continuations(&b, Context::Queue, &seq("z"));
        assert!(offered(&there, "9"), "and it should be there: {there:?}");
    }

    /// A key that only ever appears deeper is a prefix, not a command — the
    /// panel says `+n more` rather than naming something one press away.
    #[test]
    fn a_key_that_only_goes_deeper_is_shown_as_a_prefix() {
        let mut b = defaults();
        b.insert((Context::Common, seq("z q w")), Command::Quit);
        b.insert((Context::Common, seq("z q e")), Command::Top);

        let rows = continuations(&b, Context::Common, &seq("z"));
        let q = rows
            .iter()
            .find(|(k, _)| k == "q")
            .expect("`q` is reachable");
        assert_eq!(
            q.1,
            Next::More(2),
            "`z q` runs nothing on its own — two bindings live under it"
        );
    }

    /// Context shadowing, as `lookup` applies it. Several keys at once because
    /// the wrong implementation — keeping whichever the hash map yielded first
    /// — is right about half the time on any single key.
    #[test]
    fn a_views_own_binding_hides_the_common_one() {
        let mut b = defaults();
        for (key, cmd) in [
            ("z o", Command::Quit),
            ("z c", Command::Quit),
            ("z a", Command::Quit),
            ("z r", Command::Quit),
            ("z m", Command::Quit),
        ] {
            b.insert((Context::Queue, seq(key)), cmd);
        }

        let rows = continuations(&b, Context::Queue, &seq("z"));
        for key in ["o", "c", "a", "r", "m"] {
            let found = rows.iter().find(|(k, _)| k == key).expect("still offered");
            assert_eq!(
                found.1,
                Next::Runs(&Command::Quit),
                "{key:?} should be the queue's, not the common one"
            );
            assert_eq!(
                rows.iter().filter(|(k, _)| k == key).count(),
                1,
                "{key:?} must be offered once, not once per context"
            );
        }
    }

    /// `c` then `C`, not every capital and then every minuscule.
    #[test]
    fn a_key_is_followed_by_its_shifted_twin() {
        let b = defaults();
        let rows = continuations(&b, Context::Common, &seq("z"));
        let at = |k: &str| {
            rows.iter()
                .position(|(key, _)| key == k)
                .unwrap_or_else(|| panic!("{k:?} is not offered: {rows:?}"))
        };
        assert_eq!(
            at("c") + 1,
            at("C"),
            "`zc` and `zC` should be adjacent: {rows:?}"
        );
        assert_eq!(at("r") + 1, at("R"));
        assert!(at("c") < at("m"), "and the alphabet still runs a to z");
    }
}
