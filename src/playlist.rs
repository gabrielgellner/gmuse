//! Views 3 and 4: the playlist and the play queue — DESIGN §4.
//!
//! Both hold `TrackId`s, so the same track can sit in the library, a playlist
//! and the queue at once without being copied.

use std::collections::VecDeque;

use crate::store::TrackId;

/// View 3. Ordered, duplicates allowed — a playlist that silently refused a
/// repeat would be wrong.
#[derive(Debug, Default, Clone)]
pub struct Playlist {
    entries: Vec<TrackId>,
}

// Mutation arrives with the command layer in M6 (`:add`, `y`, `e`, `D`); the
// types and their semantics are defined here because §4 marks them INVARIANT
// and the views in M4 already render them.
#[allow(dead_code)]
impl Playlist {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn entries(&self) -> &[TrackId] {
        &self.entries
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn push(&mut self, id: TrackId) {
        self.entries.push(id);
    }
    /// Remove the entry at `index`, if there is one.
    ///
    /// Mirrors `VecDeque::remove`, which `Queue` gets for free. `Vec::remove`
    /// panics out of range, so the bounds check is ours to make.
    pub fn remove(&mut self, index: usize) -> Option<TrackId> {
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
    entries: VecDeque<TrackId>,
}

#[allow(dead_code)]
impl Queue {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn entries(&self) -> impl Iterator<Item = &TrackId> {
        self.entries.iter()
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn push_back(&mut self, id: TrackId) {
        self.entries.push_back(id);
    }
    /// The next queued track, without consuming it.
    ///
    /// `next()` is pure, so it reports what the queue would give; popping is
    /// the caller's job once it commits to playing.
    pub fn front(&self) -> Option<TrackId> {
        self.entries.front().copied()
    }

    pub fn pop_front(&mut self) -> Option<TrackId> {
        self.entries.pop_front()
    }
    /// Remove the entry at `index`, if there is one.
    pub fn remove(&mut self, index: usize) -> Option<TrackId> {
        self.entries.remove(index)
    }
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playlist_allows_duplicates_and_keeps_order() {
        let mut p = Playlist::new();
        p.push(TrackId(3));
        p.push(TrackId(1));
        p.push(TrackId(3));
        assert_eq!(p.entries(), [TrackId(3), TrackId(1), TrackId(3)]);
        assert_eq!(p.remove(1), Some(TrackId(1)));
        assert_eq!(p.entries(), [TrackId(3), TrackId(3)]);
        assert_eq!(p.remove(99), None);
    }

    #[test]
    fn queue_is_consumed_from_the_front() {
        let mut q = Queue::new();
        q.push_back(TrackId(1));
        q.push_back(TrackId(2));
        assert_eq!(q.pop_front(), Some(TrackId(1)));
        assert_eq!(q.len(), 1);
        assert_eq!(q.pop_front(), Some(TrackId(2)));
        assert_eq!(q.pop_front(), None);
        assert!(q.is_empty());
    }
}
