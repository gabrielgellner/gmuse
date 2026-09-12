//! **What plays next, decided where the deadline is** — #232, §6, §7.7.
//!
//! ## Why this is the engine's and not a view's
//!
//! The decoder asks for the next track with `PlayerEvent::NeedNext` while the
//! current one is still playing, and the answer has to arrive before it ends or
//! the gapless join is missed. The TUI used to answer, which worked only because
//! a TUI was always attached — and I11 makes every view detachable, with
//! playback continuing across a detach being the property the whole milestone
//! exists for.
//!
//! §6 already assigned the state: the queue and `scope` are control state,
//! and says why for the second — "`scope` is control state because it
//! determines what plays next".
//!
//! ## The pool is materialised, not a query
//!
//! §4.4 forbids the control thread from running queries, so it cannot resolve a
//! pool when the decoder asks. It holds one instead, rebuilt when the librarian
//! publishes a change or when a view switches source — so answering `NeedNext`
//! is a lookup over a `Vec` rather than anything with a cost.
//!
//! What that gives up is the literal reading of `announce_next`'s comment,
//! "resolution reflects the pool as it stands right now". What it keeps is the
//! part that mattered: the pool is not *announced ahead of time*, so a change
//! between one track and the next is seen. A change during the final second of
//! a track is not, and that is a much smaller window than the one this replaces.

use std::sync::atomic::{AtomicU64, Ordering};

use gmuse_library::model::rows::CopyId;
use gmuse_library::next::{ActiveSource, NextInput, Playback, Scope, Source};
use gmuse_library::playlist::{Playlist, Queue};
use gmuse_library::shelf::Shelf;

/// The queue, the modes, and the pool they run over.
///
/// **Versioned, and that is not symmetry with the library.** §8.8 is explicit
/// that library writes get no `base_version` because an append-only log unions
/// rather than conflicts — and that "optimistic concurrency is right for the
/// queue, playlists, lanes and the mapping table". This is the queue: two views
/// reordering it at once *is* a lost update, so a mutation carries the version
/// it was computed against and is refused if the queue has moved.
pub struct Playing {
    queue: Queue,
    playlist: Playlist,
    /// The materialised pool. Ordered, because order is what `shuffle` is the
    /// absence of.
    pool: Shelf,
    playback: Playback,
    /// Bumped on every accepted mutation. Never reused, never reset.
    version: AtomicU64,
}

/// A mutation was computed against a version the queue has moved past.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stale {
    pub current: u64,
}

impl Playing {
    pub fn new(pool: Shelf) -> Self {
        Self {
            queue: Queue::default(),
            playlist: Playlist::default(),
            pool,
            playback: Playback::default(),
            version: AtomicU64::new(1),
        }
    }

    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    /// What a view needs to draw the queue.
    pub fn get(&self) -> (u64, Vec<CopyId>) {
        (self.version(), self.queue.entries().copied().collect())
    }

    /// Check a caller's version before letting it change anything.
    ///
    /// `None` means "I have not read the queue and I am appending", which is
    /// safe for `enqueue` and is why §7.7 makes `base_version` optional there
    /// and required for `move` and `remove`: appending to a list somebody else
    /// also appended to loses nothing, and removing index 3 from a list that
    /// has changed removes the wrong track.
    fn agreed(&self, base: Option<u64>) -> Result<(), Stale> {
        match base {
            Some(seen) if seen != self.version() => Err(Stale {
                current: self.version(),
            }),
            _ => Ok(()),
        }
    }

    fn moved(&self) -> u64 {
        self.version.fetch_add(1, Ordering::AcqRel) + 1
    }

    pub fn enqueue(&mut self, ids: &[CopyId], base: Option<u64>) -> Result<u64, Stale> {
        self.agreed(base)?;
        for id in ids {
            self.queue.push_back(*id);
        }
        Ok(self.moved())
    }

    pub fn remove(&mut self, index: usize, base: Option<u64>) -> Result<u64, Stale> {
        self.agreed(base)?;
        self.queue.remove(index);
        Ok(self.moved())
    }

    pub fn clear(&mut self, base: Option<u64>) -> Result<u64, Stale> {
        self.agreed(base)?;
        self.queue.clear();
        Ok(self.moved())
    }

    pub fn set_scope(&mut self, mode: Scope) -> u64 {
        self.playback.scope = mode;
        self.moved()
    }

    pub fn scope(&self) -> Scope {
        self.playback.scope
    }

    /// **The pool, rebuilt.** Called when the librarian publishes a change, so
    /// that a track imported while something is playing can be played next.
    pub fn refresh(&mut self, pool: Shelf) {
        self.pool = pool;
    }

    /// What the playhead is on, so selection knows where it is.
    pub fn now_playing(&mut self, current: Option<CopyId>) {
        self.playback.current = current;
    }

    /// **The answer to `NeedNext`** — a lookup, not a query.
    ///
    /// Named `take_next` rather than `next` because it consumes: the queue is
    /// popped by being played from, so calling it twice gives two different
    /// answers. `next` would also read as `Iterator::next`, which this is not.
    ///
    /// Takes from the queue first and falls back to the pool under `scope`,
    /// which is `next::next_with_source`'s existing logic unchanged: this moved
    /// the *caller*, not the rule.
    pub fn take_next(&mut self) -> Option<(CopyId, Source)> {
        let input = NextInput {
            queue: &self.queue,
            active: ActiveSource::Library,
            library: &self.pool,
            playlist: &self.playlist,
            current: self.playback.current,
            scope: self.playback.scope,
            shuffle: self.playback.shuffle,
            repeat: self.playback.repeat,
            repeat_current: self.playback.repeat_current,
            rng_state: self.playback.rng_state,
        };
        let picked = gmuse_library::next::next_with_source(input)?;
        // The queue is consumed by being played from, exactly as the view did
        // it — `announce_next` pops here too.
        if picked.1 == Source::Queue {
            self.queue.pop_front();
        }
        if picked.1 == Source::Wrapped && self.playback.shuffle {
            self.playback.reshuffle();
        }
        Some(picked)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty() -> Playing {
        Playing::new(Shelf::default())
    }

    fn copy(n: u128) -> CopyId {
        gmuse_library::model::rows::CopyId::Loose(gmuse_library::model::id::Id::from_uuid(
            uuid::Uuid::from_u128(n),
        ))
    }

    #[test]
    fn a_fresh_queue_is_version_one_and_empty() {
        let (version, ids) = empty().get();
        assert_eq!(version, 1);
        assert!(ids.is_empty());
    }

    #[test]
    fn enqueuing_bumps_the_version_and_shows_up() {
        let mut playing = empty();
        let after = playing
            .enqueue(&[copy(1), copy(2)], None)
            .expect("appended");
        let (version, ids) = playing.get();
        assert_eq!(version, after);
        assert_eq!(version, 2, "one mutation, one bump");
        assert_eq!(ids.len(), 2);
    }

    /// **§8.8's one place for optimistic concurrency.** Two views reordering at
    /// once is a lost update, unlike two appends to the log.
    #[test]
    fn a_mutation_against_a_stale_version_is_refused() {
        let mut playing = empty();
        playing
            .enqueue(&[copy(1), copy(2)], None)
            .expect("appended");
        let seen = playing.version();

        // Somebody else got there first.
        playing.enqueue(&[copy(3)], Some(seen)).expect("in step");

        // And now our version is behind.
        let refused = playing.remove(0, Some(seen)).expect_err("stale");
        assert_eq!(refused.current, playing.version());
        assert_eq!(playing.get().1.len(), 3, "and nothing was removed");
    }

    /// Appending without having read the queue is allowed — §7.7 makes
    /// `base_version` optional there, because two appends lose nothing.
    #[test]
    fn appending_without_a_version_is_allowed() {
        let mut playing = empty();
        playing
            .enqueue(&[copy(1)], None)
            .expect("no version needed");
        playing.enqueue(&[copy(2)], None).expect("still fine");
        assert_eq!(playing.get().1.len(), 2);
    }

    /// Removing index 3 from a list that has changed removes the wrong track,
    /// which is why this one is not optional in practice.
    #[test]
    fn removing_against_the_right_version_takes_the_right_track() {
        let mut playing = empty();
        playing
            .enqueue(&[copy(1), copy(2), copy(3)], None)
            .expect("appended");
        let seen = playing.version();
        playing.remove(1, Some(seen)).expect("in step");
        let ids = playing.get().1;
        assert_eq!(ids, vec![copy(1), copy(3)]);
    }

    #[test]
    fn the_queue_is_played_from_before_the_pool() {
        let mut playing = empty();
        playing.enqueue(&[copy(7)], None).expect("appended");
        let (picked, source) = playing.take_next().expect("the queue has one");
        assert_eq!(picked, copy(7));
        assert_eq!(source, Source::Queue);
        assert!(playing.get().1.is_empty(), "and it was consumed");
    }

    /// An empty queue over an empty pool has nothing to say, and says so rather
    /// than repeating the current track.
    #[test]
    fn nothing_queued_and_nothing_in_the_pool_is_the_end() {
        assert!(empty().take_next().is_none());
    }

    #[test]
    fn the_scope_is_control_state_and_bumps_the_version() {
        let mut playing = empty();
        let before = playing.version();
        let after = playing.set_scope(Scope::Inner);
        assert!(after > before);
        assert_eq!(playing.scope(), Scope::Inner);
    }
}
