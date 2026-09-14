//! **What the library has heard, written out** — #261.
//!
//! [`crate::query::filter::Listened`] is nine questions about a recording's history: how often it
//! was played and when, what it is rated, what has been claimed about it and on whose word, what
//! it was derived from, and which collections hold it. The `Catalogue` answered them, so a view
//! attached to an engine kept the whole `Catalogue` to ask — 11.8 MB folded here for a fold the
//! engine already has.
//!
//! This is the part it needs. Seventeen fields of the catalogue's thirty-odd, chosen by reading
//! what the impl below touches rather than by guessing: `recordings`, `encodings`, `bound`,
//! `artist_names`, `measurements` and `groups` are not among them, and they were half of what
//! was being carried.
//!
//! ## Why it ships whole rather than being asked
//!
//! Measured (`just memory-parts`) before it was designed, and the measurement changed the
//! design. The plan had been to split the nine — send the four flat tables, ask the graph
//! questions per recording — because `claims` and the derivation walks read edges and a filter
//! asks them per row, which is the access pattern that costs 226 µs an ask.
//!
//! | | |
//! | --- | --- |
//! | plays, ratings, acquired, last played | 0.3 MB |
//! | the assertion graph, with concept labels and all four derivation sets | 1.7 MB |
//! | views, collections, entries | 0.7 MB |
//! | **all of it** | **2.7 MB** |
//!
//! The graph is 1.7 MB. There is no expensive half, so this crosses in one reply and refreshes
//! on a `library` event — no per-recording asks, and no hot path to design around.
//!
//! **It crosses through `Client::call_typed`.** Sending it was tried once and abandoned
//! (`tree-over-the-wire.md` §11): the client built a `serde_json::Value` of the whole 4.5 MB
//! reply on the way, and an attached view came out *larger* than one that folded the log. The
//! typed path is §12, and this is what it was for.

use std::collections::{BTreeMap, BTreeSet};

use crate::model::entity::{Collection, Concept, Hierarchy, Recording, View, Work};
use crate::model::facet::Described;
use crate::model::id::Id;

/// **A map with a tuple key, as a list of entries.**
///
/// A JSON object takes string keys and nothing else, and two of the fields below are keyed by
/// `(subject, concept, source)`. serde_json does not quietly do something else with those: it
/// refuses, with `key must be a string`.
///
/// Which cost an afternoon, because of *where* it refused. `json!` unwraps, so the engine's
/// handler panicked, the connection died without a reply, and the client sat waiting — a
/// serialisation bug presenting as a hang. The lesson is the test rather than the encoding:
/// `Slip` had a round-trip test from the day it was written and this did not.
mod as_entries {
    use super::BTreeMap;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<K, V, S>(map: &BTreeMap<K, V>, s: S) -> Result<S::Ok, S::Error>
    where
        K: Serialize + Ord,
        V: Serialize,
        S: Serializer,
    {
        s.collect_seq(map.iter())
    }

    pub fn deserialize<'de, K, V, D>(d: D) -> Result<BTreeMap<K, V>, D::Error>
    where
        K: Deserialize<'de> + Ord,
        V: Deserialize<'de>,
        D: Deserializer<'de>,
    {
        Ok(Vec::<(K, V)>::deserialize(d)?.into_iter().collect())
    }
}

/// The nine `Listened` answers, and the three maps a view reads directly.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Heard {
    pub plays: BTreeMap<Id<Recording>, u32>,
    pub last_played: BTreeMap<Id<Recording>, i64>,
    pub acquired: BTreeMap<Id<Recording>, i64>,
    pub ratings: BTreeMap<Id<Recording>, u8>,

    #[serde(with = "as_entries")]
    pub assertions: BTreeMap<
        (Described, Id<Concept>, crate::model::reference::Source),
        crate::model::ingest::Confidence,
    >,
    #[serde(with = "as_entries")]
    pub assertions_retracted: BTreeMap<(Described, Id<Concept>), crate::model::reference::Source>,
    pub concepts: BTreeMap<Id<Concept>, crate::model::facet::ConceptRecord>,
    pub narrows: BTreeSet<(Id<Hierarchy>, Id<Concept>, Id<Concept>)>,
    pub narrows_retracted: BTreeSet<(Id<Hierarchy>, Id<Concept>, Id<Concept>)>,

    pub derived: BTreeSet<(
        Id<Recording>,
        Id<Recording>,
        crate::model::spine::RecordingDerivationKind,
    )>,
    pub derived_retracted: BTreeSet<(
        Id<Recording>,
        Id<Recording>,
        crate::model::spine::RecordingDerivationKind,
    )>,
    /// Which works a recording performs — `work_derived_from` starts from these.
    pub performances: BTreeSet<(Id<Recording>, Id<Work>)>,
    pub work_derived: BTreeSet<(Id<Work>, Id<Work>, crate::model::spine::WorkDerivationKind)>,
    pub work_derived_retracted:
        BTreeSet<(Id<Work>, Id<Work>, crate::model::spine::WorkDerivationKind)>,

    pub views: BTreeMap<Id<View>, crate::projection::catalogue::SavedView>,
    pub views_dropped: BTreeSet<Id<View>>,
    pub collections: BTreeMap<Id<Collection>, crate::projection::catalogue::Collected>,
    pub entries: BTreeSet<(Id<Collection>, crate::model::ord::OrdKey, Id<Recording>)>,
    pub entries_retracted: BTreeSet<(Id<Collection>, crate::model::ord::OrdKey, Id<Recording>)>,
}

impl Heard {
    /// Take the part of a fold that answers about listening.
    pub fn of(c: &crate::projection::catalogue::Catalogue) -> Self {
        Heard {
            plays: c.plays.clone(),
            last_played: c.last_played.clone(),
            acquired: c.acquired.clone(),
            ratings: c.ratings.clone(),
            assertions: c.assertions.clone(),
            assertions_retracted: c.assertions_retracted.clone(),
            concepts: c.concepts.clone(),
            narrows: c.narrows.clone(),
            narrows_retracted: c.narrows_retracted.clone(),
            derived: c.derived.clone(),
            derived_retracted: c.derived_retracted.clone(),
            performances: c.performances.clone(),
            work_derived: c.work_derived.clone(),
            work_derived_retracted: c.work_derived_retracted.clone(),
            views: c.views.clone(),
            views_dropped: c.views_dropped.clone(),
            collections: c.collections.clone(),
            entries: c.entries.clone(),
            entries_retracted: c.entries_retracted.clone(),
        }
    }

    /// Every source that asserted `facet` about `subject`, retractions applied.
    pub fn asserted(
        &self,
        subject: Described,
        facet: Id<crate::model::entity::Facet>,
    ) -> Vec<(
        Id<Concept>,
        crate::model::reference::Source,
        crate::model::ingest::Confidence,
    )> {
        self.assertions
            .iter()
            .filter(|((s, concept, _), _)| {
                *s == subject
                    && self
                        .concepts
                        .get(concept)
                        .is_some_and(|record| record.facet == facet)
            })
            .filter(|((s, concept, source), _)| {
                self.assertions_retracted.get(&(*s, *concept)) != Some(source)
            })
            .map(|((_, concept, source), confidence)| (*concept, *source, *confidence))
            .collect()
    }
}

/// **The one implementation** — #261.
///
/// It was on `Catalogue`, which meant a reader holding only what these nine questions need could
/// not answer them. Moved rather than copied: two implementations of one seam is two answers to
/// one question, and this trait exists so `filter` need not know which.
impl crate::query::filter::Listened for Heard {
    fn plays(&self, recording: Id<Recording>) -> u32 {
        self.plays.get(&recording).copied().unwrap_or(0)
    }

    fn last_played(&self, recording: Id<Recording>) -> Option<i64> {
        self.last_played.get(&recording).copied()
    }

    fn acquired(&self, recording: Id<Recording>) -> Option<i64> {
        self.acquired.get(&recording).copied()
    }

    fn rating(&self, recording: Id<Recording>) -> Option<u8> {
        self.ratings.get(&recording).copied()
    }

    fn derived_from(&self, recording: Id<Recording>) -> Vec<(String, i64)> {
        climb(recording, |here| {
            self.derived
                .iter()
                .filter(move |e| e.1 == here && !self.derived_retracted.contains(e))
                .map(|(from, _, kind)| (*from, format!("{kind:?}").to_lowercase()))
                .collect()
        })
    }

    fn work_derived_from(&self, recording: Id<Recording>) -> Vec<(String, i64)> {
        // **Through the work the recording performs.** A row is a recording,
        // and the other traversal is one level up — so it is reached rather
        // than merged into the recording graph.
        let mut out = Vec::new();
        for work in self
            .performances
            .iter()
            .filter(|(r, _)| *r == recording)
            .map(|(_, w)| *w)
        {
            out.extend(climb(work, |here| {
                self.work_derived
                    .iter()
                    .filter(move |e| e.1 == here && !self.work_derived_retracted.contains(e))
                    .map(|(from, _, kind)| (*from, format!("{kind:?}").to_lowercase()))
                    .collect()
            }));
        }
        out
    }

    fn claims(&self, recording: Id<Recording>, facet_key: &str) -> Vec<(String, String, String)> {
        let facet = Id::<crate::model::entity::Facet>::of_key(facet_key);
        self.asserted(crate::model::facet::Described::Recording(recording), facet)
            .into_iter()
            .filter_map(|(concept, basis, confidence)| {
                let label = self.concepts.get(&concept)?.label.clone();
                Some((
                    label,
                    basis.written().to_string(),
                    confidence.written().to_string(),
                ))
            })
            .collect()
    }

    fn broader(&self, facet_key: &str, label: &str) -> Vec<String> {
        use crate::model::entity::Concept;
        use std::collections::BTreeSet;

        // How far a walk may climb before the data is called cyclic. Sixteen
        // is `vocabulary::MAX_DEPTH`, for the same reason: deeper than any
        // real music taxonomy, shallower than a loop can hide in.
        const MAX_DEPTH: usize = 16;

        let mut seen: BTreeSet<Id<Concept>> = BTreeSet::new();
        let mut here = vec![Id::<Concept>::in_facet(facet_key, label)];
        let mut out = Vec::new();
        for _ in 0..MAX_DEPTH {
            let mut next = Vec::new();
            for child in here.drain(..) {
                for (_, parent, _) in self.narrows.iter().filter(|(h, p, c)| {
                    *c == child && !self.narrows_retracted.contains(&(*h, *p, *c))
                }) {
                    // `seen` is what stops a cycle rather than the bound: the
                    // bound is the backstop for a chain long enough that
                    // walking it was a mistake either way.
                    if seen.insert(*parent) {
                        if let Some(record) = self.concepts.get(parent) {
                            out.push(record.label.clone());
                        }
                        next.push(*parent);
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            here = next;
        }
        out
    }

    /// **Distinct collections, not entries** — a playlist holding one track
    /// twice is one collection that holds it, and counting entries would make
    /// `collected>1` mean "repeated somewhere" instead of "in two crates".
    fn collected(&self, recording: Id<Recording>) -> u32 {
        self.entries
            .iter()
            .filter(|(_, _, r)| *r == recording)
            .map(|(c, _, _)| *c)
            .collect::<std::collections::BTreeSet<_>>()
            .len() as u32
    }
}

/// **Walk up a derivation graph, bounded** — #79.
///
/// Breadth-first, so `depth` is the shortest way back to each ancestor, and `seen` is what stops
/// a cycle rather than the bound. **The bound is the backstop**: edges are rows and rows union,
/// so two devices can author a cycle neither of them meant, and a render must answer rather than
/// hang.
///
/// Returns `(kind, depth)` per edge taken, which is what makes the bracket correlated: the kind
/// and the depth come from the same hop.
///
/// **Its doc used to open with four paragraphs about compacting holdings assertions** — a
/// run-on from `Instance::compact` that had lost its function and attached itself to the next
/// item. Moving this file's worth of code is what made it visible.
fn climb<T: Ord + Copy, F>(start: T, parents: F) -> Vec<(String, i64)>
where
    F: Fn(T) -> Vec<(T, String)>,
{
    /// Sixteen, as everywhere else here: deeper than any real chain of
    /// remixes, shallower than a loop can hide in.
    const MAX_DEPTH: i64 = 16;

    let mut seen = std::collections::BTreeSet::from([start]);
    let mut here = vec![start];
    let mut out = Vec::new();
    for depth in 1..=MAX_DEPTH {
        let mut next = Vec::new();
        for node in here.drain(..) {
            for (parent, kind) in parents(node) {
                if seen.insert(parent) {
                    out.push((kind, depth));
                    next.push(parent);
                }
            }
        }
        if next.is_empty() {
            break;
        }
        here = next;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **It has to survive the wire, which is the only reason it exists** — #261.
    ///
    /// Written before anything sent one, because the first attempt at this was not: two of these
    /// fields are maps with *tuple keys*, JSON objects take string keys only, and the failure
    /// arrived as a nine-minute hang rather than an error.
    #[test]
    fn a_heard_survives_being_written_down() {
        let mut i =
            crate::model::instance::Instance::new(crate::model::id::Id::derived(b"test-device"));
        let song = crate::model::id::Id::<Recording>::derived(b"a song");
        i.rate(100, song, Some(4));
        i.play(200, song, crate::model::id::Id::derived(b"a copy"));
        i.assert_concept(
            300,
            Described::Recording(song),
            crate::model::facet::GENRE,
            "Techno",
            crate::model::reference::Source::Manual,
            crate::model::ingest::Confidence::Confirmed,
        );
        let heard = Heard::of(&i.catalogue());

        // The instrument: an empty `Heard` round-trips trivially, and a tuple-keyed map that is
        // empty serialises whatever encoding it is given.
        assert!(!heard.ratings.is_empty(), "no rating to carry");
        assert!(!heard.assertions.is_empty(), "no assertion to carry");

        let json = serde_json::to_string(&heard).expect("a heard serialises");
        let back: Heard = serde_json::from_str(&json).expect("and comes back");
        assert_eq!(back, heard, "a heard did not survive the wire");
    }
}
