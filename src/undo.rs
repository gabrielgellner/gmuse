//! Undo for the lists you edit by hand.
//!
//! Per **list**, not one stack for the app, because vim's undo is per buffer
//! and a playlist is a buffer (§13). Undoing in one playlist after editing
//! another would otherwise reach back into a list you are no longer looking
//! at, which is exactly the surprise `u` must not spring.
//!
//! A whole snapshot rather than an inverse operation. A playlist is tens or
//! hundreds of `TrackId`s — four bytes each — so storing the *state* costs
//! almost nothing and cannot get the inverse wrong, which a hand-written
//! "undo an insert by deleting at index n" eventually does.

use std::collections::HashMap;

use crate::store::TrackId;

/// How many steps a list remembers. vim's `undolevels` defaults to 1000; a
/// playlist is edited by hand at human speed, so far fewer is plenty and it
/// bounds the memory without anyone noticing.
const DEPTH: usize = 100;

/// Which list a step belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Key {
    /// By name, so a playlist keeps its history across being closed and
    /// reopened — as a vim buffer does.
    Playlist(String),
    Queue,
}

/// A list as it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub entries: Vec<TrackId>,
    /// Restored with the entries, so undo puts the cursor back where the edit
    /// happened rather than wherever it drifted to.
    pub selected: usize,
}

#[derive(Debug, Default)]
struct History {
    done: Vec<Snapshot>,
    undone: Vec<Snapshot>,
}

#[derive(Debug, Default)]
pub struct Undo {
    per: HashMap<Key, History>,
}

impl Undo {
    /// Remember the state *before* an edit.
    ///
    /// Clears the redo stack, as vim does: once you edit after undoing, the
    /// branch you undid is gone. Keeping it would mean `C-r` restoring
    /// something that never followed from what is on screen.
    pub fn record(&mut self, key: Key, before: Snapshot) {
        let h = self.per.entry(key).or_default();
        // An edit that changed nothing is not a step. Otherwise `p` into a
        // rule, or a `dd` on an empty list, would cost a press of `u`.
        if h.done.last() == Some(&before) {
            return;
        }
        h.done.push(before);
        if h.done.len() > DEPTH {
            h.done.remove(0);
        }
        h.undone.clear();
    }

    /// Step back. `now` is what is on screen, so redo can return to it.
    pub fn undo(&mut self, key: &Key, now: Snapshot) -> Option<Snapshot> {
        let h = self.per.get_mut(key)?;
        let previous = h.done.pop()?;
        h.undone.push(now);
        Some(previous)
    }

    /// Step forward again.
    pub fn redo(&mut self, key: &Key, now: Snapshot) -> Option<Snapshot> {
        let h = self.per.get_mut(key)?;
        let next = h.undone.pop()?;
        h.done.push(now);
        Some(next)
    }

    /// Forget a list's history — it no longer describes anything.
    pub fn forget(&mut self, key: &Key) {
        self.per.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(ids: &[u32], selected: usize) -> Snapshot {
        Snapshot {
            entries: ids.iter().map(|n| crate::store::row_of_test(*n)).collect(),
            selected,
        }
    }

    #[test]
    fn undo_then_redo_returns_to_where_it_started() {
        let mut u = Undo::default();
        let key = Key::Playlist("mix".into());

        u.record(key.clone(), snap(&[1, 2, 3], 0));
        let after = snap(&[1, 3], 1);

        let back = u.undo(&key, after.clone()).expect("nothing to undo");
        assert_eq!(back, snap(&[1, 2, 3], 0));
        assert_eq!(u.redo(&key, back).unwrap(), after);
    }

    /// Steps come back in reverse order, and running out is `None` rather than
    /// an error — `u` on a fresh list simply says so.
    #[test]
    fn steps_unwind_in_order_and_then_stop() {
        let mut u = Undo::default();
        let key = Key::Queue;
        u.record(key.clone(), snap(&[1], 0));
        u.record(key.clone(), snap(&[1, 2], 0));

        assert_eq!(u.undo(&key, snap(&[1, 2, 3], 0)).unwrap(), snap(&[1, 2], 0));
        assert_eq!(u.undo(&key, snap(&[1, 2], 0)).unwrap(), snap(&[1], 0));
        assert_eq!(u.undo(&key, snap(&[1], 0)), None);
    }

    /// vim's rule: editing after an undo discards the branch you undid.
    /// Keeping it would let `C-r` restore something that does not follow from
    /// what is on screen.
    #[test]
    fn editing_after_an_undo_discards_the_redo_branch() {
        let mut u = Undo::default();
        let key = Key::Playlist("mix".into());
        u.record(key.clone(), snap(&[1, 2], 0));
        u.undo(&key, snap(&[1], 0));
        assert!(u.redo(&key, snap(&[1], 0)).is_some());

        u.undo(&key, snap(&[1], 0));
        u.record(key.clone(), snap(&[9], 0));
        assert_eq!(u.redo(&key, snap(&[9], 0)), None, "the branch survived");
    }

    /// Per list, like vim's per buffer: undoing in one must not reach into
    /// another you are no longer looking at.
    #[test]
    fn each_list_has_its_own_history() {
        let mut u = Undo::default();
        let (a, b) = (Key::Playlist("a".into()), Key::Playlist("b".into()));
        u.record(a.clone(), snap(&[1], 0));

        assert_eq!(u.undo(&b, snap(&[2], 0)), None, "b reached into a");
        assert_eq!(u.undo(&a, snap(&[], 0)).unwrap(), snap(&[1], 0));
    }

    /// An edit that changed nothing is not a step — a refused `p` should not
    /// cost a press of `u`.
    #[test]
    fn an_unchanged_state_is_not_recorded_twice() {
        let mut u = Undo::default();
        let key = Key::Queue;
        u.record(key.clone(), snap(&[1], 0));
        u.record(key.clone(), snap(&[1], 0));

        assert!(u.undo(&key, snap(&[1], 0)).is_some());
        assert_eq!(u.undo(&key, snap(&[1], 0)), None, "one edit, two steps");
    }

    #[test]
    fn history_is_bounded() {
        let mut u = Undo::default();
        let key = Key::Queue;
        for n in 0..DEPTH as u32 + 50 {
            u.record(key.clone(), snap(&[n], 0));
        }
        let mut steps = 0;
        while u.undo(&key, snap(&[999], 0)).is_some() {
            steps += 1;
        }
        assert_eq!(steps, DEPTH);
    }

    #[test]
    fn forgetting_a_list_drops_its_history() {
        let mut u = Undo::default();
        let key = Key::Playlist("mix".into());
        u.record(key.clone(), snap(&[1], 0));
        u.forget(&key);
        assert_eq!(u.undo(&key, snap(&[], 0)), None);
    }
}
