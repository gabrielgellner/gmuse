//! Where a [`Handle`] points — issue #77, `domain-model.md` §3.
//!
//! `Handle<T>` has existed since #65 as an index with nothing to index into.
//! This is the thing it indexes, and the reason both halves of identity exist:
//! an `Id` says *which entity*, a `Handle` says *where it currently sits*, and
//! only the first survives a rebuild.
//!
//! ## The failure this shape prevents
//!
//! `history.rs` warns that "a history keyed by id would silently reattach to
//! different tracks". That is true of an **arena index**, and it is exactly
//! what happens here: [`Arena::rebuild`] hands out a fresh generation, after
//! which every handle from the old one is refused rather than resolving to
//! whatever moved into its slot.
//!
//! Refused, not wrong. A stale handle is a bug in the caller, and a bug that
//! returns `None` is one you can find.

use std::collections::BTreeMap;

use crate::model::id::{Handle, Id};

/// A generation-checked arena.
///
/// The generation is what makes a stale handle detectable rather than merely
/// unlucky. Without it a rebuild that reordered entities would silently
/// reattach every held handle to a different one — the precise failure the
/// `Id`/`Handle` split exists to make impossible.
#[derive(Debug)]
pub struct Arena<T> {
    items: Vec<(Id<T>, T)>,
    by_id: BTreeMap<Id<T>, u32>,
    generation: u32,
}

impl<T> Default for Arena<T> {
    fn default() -> Self {
        Arena {
            items: Vec::new(),
            by_id: BTreeMap::new(),
            generation: 0,
        }
    }
}

impl<T> Arena<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Which generation the handles it is currently handing out belong to.
    pub fn generation(&self) -> u32 {
        self.generation
    }

    /// Put something in, or replace what is already there under that id.
    pub fn insert(&mut self, id: Id<T>, value: T) -> Handle<T> {
        match self.by_id.get(&id) {
            Some(&index) => {
                self.items[index as usize] = (id, value);
                Handle::new(self.key(index))
            }
            None => {
                let index = self.items.len() as u32;
                self.items.push((id, value));
                self.by_id.insert(id, index);
                Handle::new(self.key(index))
            }
        }
    }

    /// Resolve an identity to where it currently sits.
    pub fn handle(&self, id: Id<T>) -> Option<Handle<T>> {
        self.by_id.get(&id).map(|&i| Handle::new(self.key(i)))
    }

    /// Look something up by identity. Always correct, never stale.
    pub fn get(&self, id: Id<T>) -> Option<&T> {
        self.by_id.get(&id).map(|&i| &self.items[i as usize].1)
    }

    /// Look something up by handle — the fast path, and the one that can go
    /// stale.
    ///
    /// Returns `None` for a handle from a previous generation, rather than the
    /// entity that has since moved into that slot.
    pub fn at(&self, handle: Handle<T>) -> Option<&T> {
        let (era, index) = Self::split(handle.index());
        if era != self.generation & 0xff {
            return None;
        }
        self.items.get(index as usize).map(|(_, v)| v)
    }

    /// What a handle names, if it is still current.
    pub fn id_at(&self, handle: Handle<T>) -> Option<Id<T>> {
        let (era, index) = Self::split(handle.index());
        if era != self.generation & 0xff {
            return None;
        }
        self.items.get(index as usize).map(|(id, _)| *id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (Id<T>, &T)> {
        self.items.iter().map(|(id, v)| (*id, v))
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Empty it and invalidate every handle ever handed out.
    ///
    /// The generation moves, so a handle held across this comes back `None`.
    /// Ids are untouched, which is the whole division of labour.
    pub fn rebuild(&mut self) {
        self.items.clear();
        self.by_id.clear();
        self.generation = self.generation.wrapping_add(1);
    }

    /// Pack the generation into the top byte, so a handle carries the era it
    /// was minted in and staleness is one comparison.
    fn key(&self, index: u32) -> u32 {
        ((self.generation & 0xff) << 24) | (index & 0x00ff_ffff)
    }

    fn split(key: u32) -> (u32, u32) {
        (key >> 24, key & 0x00ff_ffff)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::entity::Recording;
    use crate::model::spine::Recording as Rec;
    use std::time::Duration;

    /// The arena is keyed *and* valued by its type: an `Arena<Recording>`
    /// holds recordings under `Id<Recording>`.
    fn rec(title: &str) -> Rec {
        Rec {
            id: Id::derived(title.as_bytes()),
            title: title.into(),
            work: None,
            recorded: None,
            bpm: None,
            length: Duration::from_secs(1),
            pinned: None,
        }
    }

    #[test]
    fn an_id_finds_a_handle_and_a_handle_finds_the_value() {
        let mut a = Arena::<Recording>::new();
        let callisto = rec("Callisto");
        let id = callisto.id;
        let h = a.insert(id, callisto);

        assert_eq!(a.get(id).map(|r| r.title.as_str()), Some("Callisto"));
        assert_eq!(a.at(h).map(|r| r.title.as_str()), Some("Callisto"));
        assert_eq!(a.handle(id), Some(h));
        assert_eq!(a.id_at(h), Some(id));
    }

    /// **The failure the split exists to prevent.** A handle held across a
    /// rebuild must not resolve to whatever moved into its slot.
    #[test]
    fn a_handle_from_before_a_rebuild_is_refused_not_reused() {
        let mut a = Arena::<Recording>::new();
        let (one, two) = (rec("Callisto"), rec("Luna"));
        let (id_one, id_two) = (one.id, two.id);
        let stale = a.insert(id_one, one);

        a.rebuild();
        // Something else now occupies slot 0.
        a.insert(id_two, two);

        assert!(
            a.at(stale).is_none(),
            "silently returning Luna here is the bug history.rs warns about"
        );
        assert_eq!(
            a.id_at(stale),
            None,
            "and it does not name the wrong entity"
        );
        assert!(a.get(id_one).is_none(), "the old entity really is gone");
        assert_eq!(
            a.get(id_two).map(|r| r.title.as_str()),
            Some("Luna"),
            "while ids still work perfectly"
        );
    }

    /// An id is durable across any number of rebuilds; a handle is not.
    #[test]
    fn identity_survives_what_position_does_not() {
        let mut a = Arena::<Recording>::new();
        let id = rec("stable").id;
        let first = a.insert(id, rec("stable"));

        for _ in 0..3 {
            a.rebuild();
            a.insert(id, rec("stable"));
        }

        assert!(a.get(id).is_some(), "the identity never moved");
        assert!(a.at(first).is_none(), "the index moved three times");
        assert!(a.handle(id).is_some(), "and can be found again");
    }

    /// Replacing under an existing id keeps the handle valid, because nothing
    /// moved.
    #[test]
    fn replacing_a_value_does_not_invalidate_its_handle() {
        let mut a = Arena::<Recording>::new();
        let id = rec("r").id;
        let h = a.insert(id, rec("before"));
        let again = a.insert(id, rec("after"));

        assert_eq!(h, again, "the same slot");
        assert_eq!(a.at(h).map(|r| r.title.as_str()), Some("after"));
        assert_eq!(a.len(), 1, "and not two entries");
    }

    #[test]
    fn iteration_is_by_insertion_and_covers_everything() {
        let mut a = Arena::<Recording>::new();
        for n in ["a", "b", "c"] {
            let r = rec(n);
            a.insert(r.id, r);
        }
        let seen: Vec<_> = a.iter().map(|(_, r)| r.title.clone()).collect();
        assert_eq!(seen, vec!["a", "b", "c"]);
    }
}
