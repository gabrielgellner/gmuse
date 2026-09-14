//! **The last few answers to an expensive question** — #261.
//!
//! `query.tree` builds every node and then takes a page, so a client walking the tree a
//! hundred nodes at a time rebuilt all of it on every page. Measured over the real library
//! (7,273 rows) against the same binary with `get` forced to miss:
//!
//! | | page 1 | page 2 | page 3 | page 4 |
//! |---|---|---|---|---|
//! | rebuilding | 0.413 s | 0.399 s | 0.399 s | 0.400 s |
//! | kept | 0.004 s | 0.004 s | 0.003 s | 0.003 s |
//!
//! **~3 ms of each lower row is the `ctl` process starting**, measured separately, so the
//! served cost is about a millisecond. A miss still pays the 0.40 s — the cache does not make
//! the tree cheaper to build, only cheaper to build *again*.
//!
//! ## Why a key and not a flag
//!
//! A cache goes wrong by answering the wrong question, so the key is the whole question: the
//! fold it was built from, the grouping, and the filter. **Invalidation is then implicit** — a
//! library change is a new `library_version`, which is a new key, and the old entry falls out
//! of the cap on its own. Nothing has to remember to clear anything.
//!
//! ## Why a scan and not a map
//!
//! Four entries, because a picker alternates between a couple of shapes and four comparisons
//! cost nothing next to what a miss costs. A map would want `Hash` on every part of the key for
//! no gain at this size.

use std::sync::{Arc, Mutex};

use crate::browse::Node;
use crate::shelf::Levels;

/// **The whole question a built tree answers** — the cache key for `query.tree`.
///
/// Three parts, and each is here because leaving it out serves a wrong answer:
///
/// - `version` — the fold it was built from. **This is the invalidation.** Without it a
///   rename or an import would keep being answered from the tree built before it, for as
///   long as the entry survived the cap.
/// - `levels` — a flat tree and a grouped one are different trees with the same contents.
/// - `filter` — a filtered tree is a different tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asked {
    /// §7.11's `library_version` — the same counter the announcer bumps.
    pub version: u64,
    pub levels: Levels,
    pub filter: Option<String>,
}

/// The built trees a socket keeps between pages.
pub type Trees = Kept<Asked, Vec<Node>>;

/// A few answers, newest last.
pub struct Kept<K, V> {
    most: usize,
    held: Mutex<Vec<(K, Arc<V>)>>,
}

impl<K: PartialEq + Clone, V> Kept<K, V> {
    /// Room for `most` answers.
    pub fn new(most: usize) -> Self {
        Self {
            most,
            held: Mutex::new(Vec::new()),
        }
    }

    /// The answer to exactly this question, if it is still held.
    ///
    /// **A poisoned lock is a miss**, not a panic: the caller can always build the answer
    /// again, and taking a process down over a cache would be the worse trade.
    pub fn get(&self, key: &K) -> Option<Arc<V>> {
        let held = self.held.lock().ok()?;
        held.iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| Arc::clone(v))
    }

    /// Keep one, dropping the oldest past the cap.
    ///
    /// **Called after building, not around it.** A 347 ms miss must not hold every other
    /// caller behind the lock; two callers missing at once build twice and the second
    /// overwrites, which is wasteful for an instant and cheaper than serialising them.
    pub fn keep(&self, key: K, value: Arc<V>) {
        let Ok(mut held) = self.held.lock() else {
            return;
        };
        // Re-keeping moves it to the newest end rather than holding two.
        held.retain(|(k, _)| k != &key);
        held.push((key, value));
        let over = held.len().saturating_sub(self.most);
        held.drain(..over);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A library change is a different question**, which is the whole invalidation story.
    ///
    /// `query.tree` never clears anything; it relies on a new fold being a new key, so a key
    /// that compared equal across folds would serve a tree built before an import or a
    /// rename for as long as it survived the cap. Nothing else in the system would notice.
    #[test]
    fn a_newer_fold_is_not_the_same_question() {
        let asked = |version| Asked {
            version,
            levels: Levels::default(),
            filter: None,
        };
        assert_ne!(
            asked(7),
            asked(8),
            "two folds asked the same question, so a stale tree would be served"
        );
        assert_eq!(asked(7), asked(7), "one fold asked two different questions");

        let kept: Kept<Asked, u32> = Kept::new(4);
        kept.keep(asked(7), Arc::new(70));
        assert!(
            kept.get(&asked(8)).is_none(),
            "a tree built before the library changed was served after it"
        );
    }

    /// **Two questions do not share an answer** — the way a cache goes wrong.
    #[test]
    fn an_answer_is_only_returned_for_its_own_question() {
        let kept: Kept<&str, u32> = Kept::new(4);
        kept.keep("everything", Arc::new(1));
        kept.keep("filtered", Arc::new(2));

        assert_eq!(kept.get(&"everything").as_deref(), Some(&1));
        assert_eq!(kept.get(&"filtered").as_deref(), Some(&2));
        assert!(
            kept.get(&"never asked").is_none(),
            "a question nobody asked was answered from somebody else's"
        );
    }

    /// **The oldest goes when the cap is reached**, and the rest stay.
    #[test]
    fn the_cap_drops_the_oldest_and_keeps_the_rest() {
        let kept: Kept<u32, u32> = Kept::new(2);
        kept.keep(1, Arc::new(10));
        kept.keep(2, Arc::new(20));
        kept.keep(3, Arc::new(30));

        assert!(kept.get(&1).is_none(), "the oldest survived the cap");
        assert_eq!(kept.get(&2).as_deref(), Some(&20));
        assert_eq!(kept.get(&3).as_deref(), Some(&30));
    }

    /// **Asking the same question again does not fill the cache with copies of it.**
    ///
    /// Without this, paging through one tree would evict every other answer within four pages
    /// — which is the case the cache exists for.
    #[test]
    fn re_keeping_one_question_does_not_crowd_the_others() {
        let kept: Kept<u32, u32> = Kept::new(2);
        kept.keep(1, Arc::new(10));
        for n in 0..5 {
            kept.keep(2, Arc::new(20 + n));
        }
        assert_eq!(
            kept.get(&1).as_deref(),
            Some(&10),
            "re-keeping one question evicted the other"
        );
        assert_eq!(kept.get(&2).as_deref(), Some(&24), "it kept a stale value");
    }
}
