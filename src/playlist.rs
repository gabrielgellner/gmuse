//! Views 3 and 4: the playlist and the play queue — DESIGN §4.
//!
//! Both hold `CopyId`s, so the same track can sit in the library, a playlist
//! and the queue at once without being copied.

use std::collections::VecDeque;

use crate::model::rows::CopyId;

/// View 3. Ordered, duplicates allowed — a playlist that silently refused a
/// repeat would be wrong.
#[derive(Debug, Default, Clone)]
pub struct Playlist {
    entries: Vec<CopyId>,
}

// Mutation arrives with the command layer in M6 (`:add`, `y`, `e`, `D`); the
// types and their semantics are defined here because §4 marks them INVARIANT
// and the views in M4 already render them.
#[allow(dead_code)]
impl Playlist {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn entries(&self) -> &[CopyId] {
        &self.entries
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn push(&mut self, id: CopyId) {
        self.entries.push(id);
    }
    /// Insert at `index`, clamped to the end.
    ///
    /// Clamped rather than panicking: `p` on the last row puts after it, which
    /// is one past the end, and that is the ordinary case rather than an error.
    pub fn insert(&mut self, index: usize, id: CopyId) {
        self.entries.insert(index.min(self.entries.len()), id);
    }
    /// Remove the entry at `index`, if there is one.
    ///
    /// Mirrors `VecDeque::remove`, which `Queue` gets for free. `Vec::remove`
    /// panics out of range, so the bounds check is ours to make.
    pub fn remove(&mut self, index: usize) -> Option<CopyId> {
        if index < self.entries.len() {
            Some(self.entries.remove(index))
        } else {
            None
        }
    }
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// View 4. Consumed as it plays: DESIGN §5 makes the queue win unconditionally
/// over the active source, and popping is what makes that a one-shot.
#[derive(Debug, Default, Clone)]
pub struct Queue {
    entries: VecDeque<CopyId>,
}

#[allow(dead_code)]
impl Queue {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn entries(&self) -> impl Iterator<Item = &CopyId> {
        self.entries.iter()
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn push_back(&mut self, id: CopyId) {
        self.entries.push_back(id);
    }
    /// Insert at `index`, clamped to the end — see [`Playlist::insert`].
    pub fn insert(&mut self, index: usize, id: CopyId) {
        self.entries.insert(index.min(self.entries.len()), id);
    }
    /// The next queued track, without consuming it.
    ///
    /// `next()` is pure, so it reports what the queue would give; popping is
    /// the caller's job once it commits to playing.
    pub fn front(&self) -> Option<CopyId> {
        self.entries.front().copied()
    }

    pub fn pop_front(&mut self) -> Option<CopyId> {
        self.entries.pop_front()
    }
    /// Remove the entry at `index`, if there is one.
    pub fn remove(&mut self, index: usize) -> Option<CopyId> {
        self.entries.remove(index)
    }
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

// **The `.m3u8` directory store lived here and is gone** (#118).
//
// `Playlists` listed a folder, `Saved` named a file in it, and `file_stem`
// made a name safe to be one. Between them they made a *path* authoritative
// for what a playlist contained: saved here, opened on another machine, it
// resolved nothing — the same failure `roots` had for catalogs (#94), in the
// one corner it survived.
//
// A playlist is an ordered collection now: `CollectionNamed { ordered: true }`
// and one entry per recording, which is SKOS's `OrderedCollection` and is a
// fact rather than a file. m3u is what `:export` writes, and nothing reads one
// back.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::rows::CopyId;

    fn id(n: u128) -> CopyId {
        CopyId::from_uuid(uuid::Uuid::from_u128(n))
    }

    /// **A playlist admits a repeat**, which is the difference between a
    /// sequence and a set and the reason view 3 is not a `BTreeSet`.
    #[test]
    fn a_playlist_holds_the_same_track_twice() {
        let mut p = Playlist::new();
        p.push(id(1));
        p.push(id(2));
        p.push(id(1));
        assert_eq!(p.len(), 3);
        assert_eq!(p.entries(), [id(1), id(2), id(1)]);
    }

    /// `p` on the last row puts *after* it, which is one past the end — the
    /// ordinary case rather than an error, so the index is clamped.
    #[test]
    fn inserting_past_the_end_appends_rather_than_panicking() {
        let mut p = Playlist::new();
        p.push(id(1));
        p.insert(99, id(2));
        assert_eq!(p.entries(), [id(1), id(2)]);
    }

    /// `Vec::remove` panics out of range, so the bounds check is ours.
    #[test]
    fn removing_a_row_that_is_not_there_is_none_not_a_panic() {
        let mut p = Playlist::new();
        p.push(id(1));
        assert_eq!(p.remove(5), None);
        assert_eq!(p.remove(0), Some(id(1)));
        assert!(p.is_empty());
    }

    /// The queue is consumed as it plays — §5 makes it win unconditionally
    /// over the active source, and popping is what makes that a one-shot.
    #[test]
    fn the_queue_is_consumed_as_it_plays() {
        let mut q = Queue::new();
        q.push_back(id(1));
        q.push_back(id(2));
        assert_eq!(q.pop_front(), Some(id(1)));
        assert_eq!(q.pop_front(), Some(id(2)));
        assert_eq!(q.pop_front(), None);
    }
}
