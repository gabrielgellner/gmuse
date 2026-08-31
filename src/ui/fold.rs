//! Expansion state for a tree — vim's fold model.
//!
//! Three fields rather than a set of open nodes, because vim's folds are two
//! ideas at once: a **level** everything shallower than is open, and explicit
//! overrides either side of it. A bare `HashSet` of open paths cannot express
//! `zr` — "open one more level" — without walking the whole tree to insert
//! every node at that depth, which then loses the distinction between "open
//! because the level says so" and "open because you opened it".
//!
//! The payoff is `zr`/`zm`, which almost no file explorer implements and which
//! this tree *can*: its levels are **homogeneous**. In a filesystem, depth 3
//! means nothing in particular. Here every node at depth 0 is an artist and
//! every node at depth 1 is an album, so "collapse to artists" is a real
//! operation and `foldlevel` is exactly the word for it.
//!
//! Keyed on the **ident path**, not on a row index: expansion has to survive a
//! re-sort, a filter, and a rescan, for the same reason a file explorer keys on
//! path rather than line number.

use std::collections::HashSet;

/// One step in a node's identity, from the root down.
pub type Path<K> = Vec<K>;

#[derive(Debug)]
pub struct Expansion<K> {
    /// Everything **shallower** than this is open. `0` closes everything;
    /// `1` opens the top level; and so on.
    ///
    /// Stored unclamped. vim's `zr` past the deepest fold is a no-op rather
    /// than an error, and clamping here would make the next `zm` asymmetric —
    /// you would have to press it once for nothing before it moved.
    level: u16,
    /// Opened by hand at or beyond `level`.
    opened: HashSet<Path<K>>,
    /// Closed by hand shallower than `level`.
    closed: HashSet<Path<K>>,
}

/// By hand, not derived: `#[derive(Default)]` would demand `K: Default`, which
/// a key type has no reason to satisfy — the empty state does not contain a
/// `K` at all.
impl<K> Default for Expansion<K> {
    fn default() -> Self {
        Self {
            level: 0,
            opened: HashSet::new(),
            closed: HashSet::new(),
        }
    }
}

impl<K: std::hash::Hash + Eq + Clone> Expansion<K> {
    /// A node's depth: an artist's path is one key long and it sits at depth
    /// **0**, so `level = 1` opens the artists. Reading `level` against the
    /// path's *length* instead is off by one, and makes `level = 1` open
    /// nothing at all.
    fn depth(path: &[K]) -> u16 {
        path.len().saturating_sub(1) as u16
    }

    pub fn is_open(&self, path: &[K]) -> bool {
        if self.closed.contains(path) {
            return false;
        }
        if Self::depth(path) < self.level {
            return true;
        }
        self.opened.contains(path)
    }

    /// `zo`. Recorded as an override only when the level does not already say
    /// so, which keeps `opened` meaning exactly "open *despite* the level".
    ///
    /// Not observable through `is_open` — a shallower entry would never be
    /// consulted, and `set_level` clears the set anyway. It keeps the state
    /// canonical rather than fixing a bug, which is why no test pins it.
    pub fn open(&mut self, path: &[K]) {
        self.closed.remove(path);
        if Self::depth(path) >= self.level {
            self.opened.insert(path.to_vec());
        }
    }

    /// `zc`.
    pub fn close(&mut self, path: &[K]) {
        self.opened.remove(path);
        if Self::depth(path) < self.level {
            self.closed.insert(path.to_vec());
        }
    }

    /// `za`.
    pub fn toggle(&mut self, path: &[K]) {
        if self.is_open(path) {
            self.close(path);
        } else {
            self.open(path);
        }
    }

    /// Bound by #32, which adds the scroll-and-reveal half of the `z` family.
    #[allow(dead_code)]
    pub fn level(&self) -> u16 {
        self.level
    }

    /// `zr`, `zm`, `zR`, `zM`.
    ///
    /// Discards the hand-made overrides, as vim's `foldlevel` does: the point
    /// of a level command is to say what the whole tree looks like, and
    /// leaving exceptions behind would make `zM` not actually close
    /// everything.
    pub fn set_level(&mut self, level: u16) {
        self.level = level;
        self.opened.clear();
        self.closed.clear();
    }

    /// `zr` — one more level. Saturating, so it cannot wrap at the top.
    pub fn more(&mut self) {
        self.set_level(self.level.saturating_add(1));
    }

    /// `zm` — one fewer.
    pub fn less(&mut self) {
        self.set_level(self.level.saturating_sub(1));
    }

    /// `zv` — open exactly the ancestors of `path`, and nothing else.
    ///
    /// Not `zR`-then-scroll: revealing the playing track should leave the rest
    /// of the tree as you had it.
    #[allow(dead_code)]
    pub fn reveal(&mut self, path: &[K]) {
        for depth in 1..path.len() {
            self.open(&path[..depth]);
        }
    }

    /// Open every path given — `zO`, whose descendants the caller finds,
    /// because only the navigator knows what is under a node.
    pub fn open_all(&mut self, paths: impl IntoIterator<Item = Path<K>>) {
        for p in paths {
            self.open(&p);
        }
    }

    /// `zC`.
    pub fn close_all(&mut self, paths: impl IntoIterator<Item = Path<K>>) {
        for p in paths {
            self.close(&p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn nothing_is_open_at_level_zero() {
        let e: Expansion<String> = Expansion::default();
        assert!(!e.is_open(&p(&["burial"])));
        assert!(!e.is_open(&p(&["burial", "untrue"])));
    }

    /// The whole reason for a level rather than a set: one step opens every
    /// node at a depth without enumerating them.
    #[test]
    fn a_level_opens_a_whole_depth_without_naming_it() {
        let mut e: Expansion<String> = Expansion::default();
        e.more();
        assert!(e.is_open(&p(&["burial"])), "artists should be open");
        assert!(e.is_open(&p(&["anyone-at-all"])), "including unseen ones");
        assert!(
            !e.is_open(&p(&["burial", "untrue"])),
            "albums should not be"
        );

        e.more();
        assert!(e.is_open(&p(&["burial", "untrue"])));
    }

    #[test]
    fn opening_and_closing_by_hand_overrides_the_level() {
        let mut e: Expansion<String> = Expansion::default();
        // Below the level: closing has to be recorded, or it would spring open.
        e.more();
        e.close(&p(&["burial"]));
        assert!(!e.is_open(&p(&["burial"])));
        assert!(e.is_open(&p(&["caribou"])), "only the one closed");

        // At or beyond the level: opening has to be recorded instead.
        e.open(&p(&["burial", "untrue"]));
        assert!(e.is_open(&p(&["burial", "untrue"])));
        assert!(!e.is_open(&p(&["caribou", "swim"])));
    }

    #[test]
    fn toggle_reverses_whichever_way_it_currently_is() {
        let mut e: Expansion<String> = Expansion::default();
        let burial = p(&["burial"]);
        e.toggle(&burial);
        assert!(e.is_open(&burial));
        e.toggle(&burial);
        assert!(!e.is_open(&burial));

        // And against the level, not just against the set.
        e.more();
        assert!(e.is_open(&burial));
        e.toggle(&burial);
        assert!(!e.is_open(&burial), "toggle ignored the level");
    }

    /// vim's rule: a level command says what the whole tree looks like, so it
    /// discards exceptions. `zM` that left something open would not be `zM`.
    #[test]
    fn a_level_command_discards_hand_made_exceptions() {
        let mut e: Expansion<String> = Expansion::default();
        e.open(&p(&["burial", "untrue"]));
        e.more();
        e.close(&p(&["caribou"]));

        e.set_level(0);
        assert!(
            !e.is_open(&p(&["burial", "untrue"])),
            "an override survived"
        );
        e.more();
        assert!(e.is_open(&p(&["caribou"])), "a closed override survived");
    }

    /// vim stores `foldlevel` unclamped: `zr` past the bottom is a no-op, and
    /// clamping would make the next `zm` do nothing visible first.
    #[test]
    fn the_level_saturates_rather_than_wrapping() {
        let mut e: Expansion<String> = Expansion::default();
        e.less();
        assert_eq!(e.level(), 0, "`zm` at the top wrapped");
        for _ in 0..5 {
            e.more();
        }
        assert_eq!(e.level(), 5);
    }

    /// `zv` opens the way to one row and leaves the rest alone — the whole
    /// difference between revealing and `zR`-then-scrolling.
    #[test]
    fn reveal_opens_only_the_ancestors() {
        let mut e: Expansion<String> = Expansion::default();
        e.reveal(&p(&["burial", "untrue", "archangel"]));

        assert!(e.is_open(&p(&["burial"])));
        assert!(e.is_open(&p(&["burial", "untrue"])));
        assert!(!e.is_open(&p(&["caribou"])), "it opened something else");
        assert_eq!(e.level(), 0, "it changed the level");
    }
}
