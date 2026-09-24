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

/// An expansion taken apart, for sending — see [`Expansion::parts`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parts<K> {
    pub level: u16,
    pub eager: Option<u16>,
    pub opened: Vec<Path<K>>,
    pub closed: Vec<Path<K>>,
}

/// **Serialisable because it travels with a request** — #261. A view keeps its own fold state
/// and sends it; the librarian holds none. It is small for the reason the sets are exceptions
/// rather than members: twenty artists open by hand is about 600 bytes, and it does not grow
/// with the library.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(bound(
    serialize = "K: serde::Serialize",
    deserialize = "K: serde::Deserialize<'de> + std::hash::Hash + Eq"
))]
pub struct Expansion<K> {
    /// Everything **shallower** than this is open. `0` closes everything;
    /// `1` opens the top level; and so on.
    ///
    /// Kept unclamped. vim's `zr` past the deepest fold is a no-op rather
    /// than an error, and clamping here would make the next `zm` asymmetric —
    /// you would have to press it once for nothing before it moved.
    level: u16,
    /// Opened by hand at or beyond `level`.
    opened: HashSet<Path<K>>,
    /// Closed by hand shallower than `level`, or at a depth that opens by
    /// itself.
    closed: HashSet<Path<K>>,
    /// **The depth at which folds open by themselves** — #169.
    ///
    /// A disc header exists to say where a disc starts, not to hide it:
    /// opening a record and finding two shut rows is a keystroke spent to see
    /// the thing you just asked for. So the disc level is open unless somebody
    /// shuts it, which is the opposite default from artists and albums.
    ///
    /// A field rather than a special case in the navigator, because `is_open`
    /// and `close` both have to agree about it — a level the drawing treats as
    /// open and the toggle treats as closed makes the first keypress do
    /// nothing visible.
    eager: Option<u16>,
}

impl<K: std::hash::Hash + Eq> Expansion<K> {
    /// A fingerprint of what is open, for a cache keyed on the folds — #285.
    ///
    /// **Order-independent, because the sets are `HashSet`s.** Iteration order
    /// is not stable between runs or after a rehash, so the per-path hashes are
    /// combined with `^` and `wrapping_add` rather than fed to one hasher in
    /// sequence — a hash that depended on iteration order would miss a cache it
    /// should hit, silently and intermittently, which is worse than not caching.
    ///
    /// Collisions are possible in principle and would show a stale row count.
    /// They are not possible in practice at this size, and a wrong count is a
    /// scrollbar being off rather than a wrong answer to anything.
    pub fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let one = |path: &Path<K>| {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            path.hash(&mut h);
            h.finish()
        };
        let fold = |set: &HashSet<Path<K>>| {
            set.iter()
                .fold(0u64, |acc, p| (acc ^ one(p)).wrapping_add(1))
        };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.level.hash(&mut h);
        self.eager.hash(&mut h);
        h.finish() ^ fold(&self.opened).wrapping_mul(31) ^ fold(&self.closed)
    }
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
            eager: None,
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

    /// **Rebuild one from a client's own account of it** — #261.
    ///
    /// The wire carries paths as plain strings because `gmuse-proto` cannot name a `SortKey`,
    /// so the caller maps them to keys and hands them over here. §12's rule: a string becomes a
    /// typed value at the edge and nothing downstream sees a raw one.
    pub fn from_parts(
        level: u16,
        eager: Option<u16>,
        opened: impl IntoIterator<Item = Path<K>>,
        closed: impl IntoIterator<Item = Path<K>>,
    ) -> Self {
        Self {
            level,
            eager,
            opened: opened.into_iter().collect(),
            closed: closed.into_iter().collect(),
        }
    }

    /// What a client would have to send to get this state back.
    ///
    /// **Sorted**, so two equal fold states serialise alike — a `HashSet` iterates in whatever
    /// order it feels like, and a request that differs only by that would miss a cache and read
    /// as a different question in a log.
    pub fn parts(&self) -> Parts<K>
    where
        K: Ord,
    {
        let sorted = |set: &HashSet<Path<K>>| {
            let mut out: Vec<Path<K>> = set.iter().cloned().collect();
            out.sort();
            out
        };
        Parts {
            level: self.level,
            eager: self.eager,
            opened: sorted(&self.opened),
            closed: sorted(&self.closed),
        }
    }

    pub fn is_open(&self, path: &[K]) -> bool {
        if self.closed.contains(path) {
            return false;
        }
        if Self::depth(path) < self.level {
            return true;
        }
        if self.eager.is_some_and(|d| Self::depth(path) >= d) {
            return true;
        }
        self.opened.contains(path)
    }

    /// **From this depth down, a fold starts open** — #169.
    ///
    /// Set once when the navigator is built. `None` is the ordinary rule and
    /// what every level above the disc uses.
    pub fn opening_from(&mut self, depth: u16) {
        self.eager = Some(depth);
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
        // **Recorded where it would otherwise reopen itself.** Below the
        // level, or at a depth that opens by itself, dropping the override is
        // not enough — the rule would put it straight back.
        if Self::depth(path) < self.level || self.eager.is_some_and(|d| Self::depth(path) >= d) {
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
        assert!(
            !e.is_open(&p(&["burial"])),
            "`zm` at the top wrapped, and everything is open"
        );

        // Through `is_open` rather than a getter: what the level *is* only
        // matters through what it opens, and asserting on the number instead
        // was the only thing keeping the getter alive.
        e.more();
        assert!(e.is_open(&p(&["burial"])), "one `zr` should open the top");
        assert!(!e.is_open(&p(&["burial", "untrue"])), "and only the top");
    }
}
