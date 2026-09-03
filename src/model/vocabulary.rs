//! Genre as a codelist — issue #74, `domain-model.md` §2.
//!
//! Genre is the one vocabulary in this model that is **data**, by the
//! promotion test: adding a value needs no behaviour written for it, only
//! grouping and filtering. So it gets identity, aliases, hierarchy, merge and
//! external bindings — everything an entity gets — where a musical key, which
//! needs its position on the circle of fifths written before it can exist, does
//! not.
//!
//! ## What this promotes
//!
//! `tools/genre/propose.py` holds a real concept scheme in Python
//! dictionaries that gmuse cannot see: **81 assertions** — 53 `IMPLIES`
//! (narrower-of), 20 `BROAD` (top concepts), 3 `RENAME` (alternate labels) and
//! 5 `DROP` (not genres at all). Moving them here is what stops the compound
//! `Broad; Style` string from being load-bearing.
//!
//! ## The hierarchy is a separate artefact
//!
//! As in SDMX, and unlike SKOS's `broader` on the concept itself. One genre
//! list can carry a browse hierarchy and a filter hierarchy at the same time
//! without the concepts changing — and a concept can therefore be moved in one
//! hierarchy without being moved in the other.

use std::collections::{BTreeMap, BTreeSet};

use crate::model::entity::{Genre, Hierarchy};
use crate::model::id::{Id, Ref};
use crate::model::reference::Source;

/// A concept in the vocabulary. The id is meaningless; the label is mutable.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GenreRecord {
    pub id: Id<Genre>,
    pub name: String,
}

/// One grouping of the concepts. A list may carry several.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HierarchyRecord {
    pub id: Id<Hierarchy>,
    pub name: String,
}

/// One narrower-of edge, in one hierarchy.
///
/// Row existence, so it unions across devices and there is no
/// `Genre.children` to collide over.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct HierarchyEdge {
    pub hierarchy: Ref<Hierarchy>,
    pub parent: Ref<Genre>,
    pub child: Ref<Genre>,
}

/// What a genre is assigned to.
///
/// The release group by default, because that is how this library is curated
/// and where the data lives: querying the release found genres for 3 of 49
/// records, the group for 130 of 429.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum GenreOn {
    ReleaseGroup(Id<crate::model::entity::ReleaseGroup>),
    Recording(Id<crate::model::entity::Recording>),
}

/// One genre on one subject, with why it was assigned and how confidently.
///
/// A bare reference would have nowhere to record either, which is exactly what
/// `genre-uncertain.tsv` tracks today and throws away at the end of every pass.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GenreAssignment {
    pub subject: GenreOn,
    pub genre: Ref<Genre>,
    pub basis: Source,
    pub confidence: crate::model::ingest::Confidence,
}

/// A cycle someone's data acquired without anyone authoring it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cycle {
    pub hierarchy: Id<Hierarchy>,
    pub at: Id<Genre>,
}

/// The vocabulary, its hierarchies, and what has been assigned from it.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(bound = "")]
pub struct Vocabulary {
    pub genres: BTreeMap<Id<Genre>, GenreRecord>,
    pub hierarchies: BTreeMap<Id<Hierarchy>, HierarchyRecord>,
    pub edges: BTreeSet<HierarchyEdge>,
    pub assignments: Vec<GenreAssignment>,
}

/// How deep a hierarchy walk may go before it is called a cycle.
const MAX_DEPTH: usize = 16;

impl Vocabulary {
    pub fn define(&mut self, name: &str) -> Id<Genre> {
        let id = Id::<Genre>::derived(crate::model::ingest::normalise(name).as_bytes());
        self.genres.entry(id).or_insert_with(|| GenreRecord {
            id,
            name: name.to_string(),
        });
        id
    }

    pub fn hierarchy(&mut self, name: &str) -> Id<Hierarchy> {
        let id = Id::<Hierarchy>::derived(name.as_bytes());
        self.hierarchies
            .entry(id)
            .or_insert_with(|| HierarchyRecord {
                id,
                name: name.to_string(),
            });
        id
    }

    /// `child` is narrower than `parent`, in this hierarchy.
    pub fn narrows(&mut self, hierarchy: Id<Hierarchy>, child: Id<Genre>, parent: Id<Genre>) {
        self.edges.insert(HierarchyEdge {
            hierarchy: Ref::to(hierarchy),
            parent: Ref::to(parent),
            child: Ref::to(child),
        });
    }

    /// Everything at or under a concept, in one hierarchy.
    ///
    /// This is what `genre="*electronic*"` has been faking: a substring match
    /// that finds `Electronic` inside `Electronic; Techno` by luck of spelling
    /// rather than by meaning.
    pub fn under(&self, hierarchy: Id<Hierarchy>, root: Id<Genre>) -> BTreeSet<Id<Genre>> {
        let mut seen = BTreeSet::from([root]);
        let mut frontier = vec![root];
        for _ in 0..MAX_DEPTH {
            let mut next = Vec::new();
            for parent in frontier.drain(..) {
                for e in &self.edges {
                    if e.hierarchy.id() == hierarchy && e.parent.id() == parent {
                        let child = e.child.id();
                        if seen.insert(child) {
                            next.push(child);
                        }
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            frontier = next;
        }
        seen
    }

    /// The concepts with no parent — the top of a hierarchy.
    pub fn top_of(&self, hierarchy: Id<Hierarchy>) -> BTreeSet<Id<Genre>> {
        let children: BTreeSet<_> = self
            .edges
            .iter()
            .filter(|e| e.hierarchy.id() == hierarchy)
            .map(|e| e.child.id())
            .collect();
        self.edges
            .iter()
            .filter(|e| e.hierarchy.id() == hierarchy)
            .map(|e| e.parent.id())
            .filter(|p| !children.contains(p))
            .collect()
    }

    /// Cycles, so the data gets **fixed** rather than merely survived.
    ///
    /// Edges are rows and rows union, so one device adding `A narrows B` while
    /// another adds `B narrows A` produces a cycle neither authored. The depth
    /// bound in [`Vocabulary::under`] stops that hanging a render, which is not
    /// the same as anyone knowing about it.
    pub fn cycles(&self) -> Vec<Cycle> {
        let mut found = Vec::new();
        for h in self.hierarchies.keys() {
            for g in self.genres.keys() {
                if self.under(*h, *g).len() > 1
                    && self
                        .under(*h, *g)
                        .iter()
                        .any(|d| *d != *g && self.under(*h, *d).contains(g))
                {
                    found.push(Cycle {
                        hierarchy: *h,
                        at: *g,
                    });
                }
            }
        }
        found
    }

    /// What a recording's genres actually are.
    ///
    /// Assignments are rows and rows only union, so "recording-level overrides"
    /// is a **read rule** rather than something the storage does — and it has
    /// to be written down or every caller invents its own.
    ///
    /// The middle rung is where the compilation case lives: a rock song on its
    /// album *and* on _80s Hits_ is reached through two release groups, and a
    /// naive union files it under the compilation's `Pop`. Preferring
    /// non-compilations resolves it with data the model already carries;
    /// falling back to all of them keeps a compilation-only recording from
    /// having no genre at all.
    pub fn effective_genres(
        &self,
        recording: Id<crate::model::entity::Recording>,
        groups: &[(Id<crate::model::entity::ReleaseGroup>, bool)],
    ) -> BTreeSet<Id<Genre>> {
        let own: BTreeSet<_> = self
            .assignments
            .iter()
            .filter(|a| a.subject == GenreOn::Recording(recording))
            .map(|a| a.genre.id())
            .collect();
        if !own.is_empty() {
            return own;
        }

        let from = |only_albums: bool| -> BTreeSet<Id<Genre>> {
            self.assignments
                .iter()
                .filter_map(|a| match a.subject {
                    GenreOn::ReleaseGroup(g)
                        if groups
                            .iter()
                            .any(|(id, comp)| *id == g && (!only_albums || !*comp)) =>
                    {
                        Some(a.genre.id())
                    }
                    _ => None,
                })
                .collect()
        };

        let albums = from(true);
        if albums.is_empty() {
            from(false)
        } else {
            albums
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::entity::{Recording, ReleaseGroup};
    use crate::model::ingest::Confidence;

    /// The vocabulary `tools/genre/propose.py` already holds, in miniature.
    fn library_vocabulary() -> (Vocabulary, Id<Hierarchy>, Id<Genre>, Id<Genre>) {
        let mut v = Vocabulary::default();
        let browse = v.hierarchy("browse");
        let electronic = v.define("Electronic");
        let techno = v.define("Techno");
        let minimal = v.define("Minimal Techno");
        v.narrows(browse, techno, electronic);
        v.narrows(browse, minimal, techno);
        (v, browse, electronic, techno)
    }

    /// **What the compound string was faking.** `genre="*electronic*"` finds
    /// `Electronic; Techno` by luck of spelling; this finds it by meaning, and
    /// finds `Minimal Techno` too, which the substring never did.
    #[test]
    fn filtering_under_a_concept_reaches_the_whole_subtree() {
        let (v, browse, electronic, techno) = library_vocabulary();
        let under = v.under(browse, electronic);
        assert_eq!(under.len(), 3, "itself, Techno, and Minimal Techno");
        assert!(under.contains(&techno));
        assert_eq!(v.under(browse, techno).len(), 2, "and it narrows downward");
    }

    /// One list, several hierarchies — the SDMX move. A browse tree and a
    /// filter tree can disagree without either concept changing.
    #[test]
    fn one_vocabulary_carries_more_than_one_hierarchy() {
        let (mut v, browse, electronic, techno) = library_vocabulary();
        let dancefloor = v.hierarchy("dancefloor");
        let house = v.define("House");
        v.narrows(dancefloor, techno, house);

        assert!(v.under(browse, electronic).contains(&techno));
        assert!(
            !v.under(dancefloor, electronic).contains(&techno),
            "the other hierarchy says something else, and both are true"
        );
        assert_eq!(v.top_of(dancefloor), BTreeSet::from([house]));
    }

    /// Edges are rows and rows union, so two devices can build a cycle neither
    /// authored. The depth bound stops it hanging a render; this is how anyone
    /// finds out.
    #[test]
    fn a_cycle_nobody_authored_is_detected() {
        let (mut v, browse, electronic, techno) = library_vocabulary();
        assert!(v.cycles().is_empty());

        v.narrows(browse, electronic, techno); // the other device's edge
        assert!(
            !v.cycles().is_empty(),
            "a cycle is a thing to be fixed, not merely survived"
        );
        assert!(
            v.under(browse, electronic).len() <= v.genres.len(),
            "and the walk still terminates"
        );
    }

    /// **The compilation case.** A rock song on its album and on _80s Hits_
    /// must not be filed under the compilation's genre.
    #[test]
    fn a_compilation_does_not_decide_a_track_s_genre() {
        let mut v = Vocabulary::default();
        let rock = v.define("Rock");
        let pop = v.define("Pop");
        let album = Id::<ReleaseGroup>::derived(b"the-album");
        let comp = Id::<ReleaseGroup>::derived(b"80s-hits");
        let song = Id::<Recording>::derived(b"the-song");

        for (subject, genre) in [(album, rock), (comp, pop)] {
            v.assignments.push(GenreAssignment {
                subject: GenreOn::ReleaseGroup(subject),
                genre: Ref::to(genre),
                basis: Source::Discogs,
                confidence: Confidence::Matched,
            });
        }

        let on_both = [(album, false), (comp, true)];
        assert_eq!(
            v.effective_genres(song, &on_both),
            BTreeSet::from([rock]),
            "the album decides, not the compilation"
        );
    }

    /// But a compilation-only track is not left with nothing.
    #[test]
    fn a_compilation_still_answers_when_it_is_all_there_is() {
        let mut v = Vocabulary::default();
        let pop = v.define("Pop");
        let comp = Id::<ReleaseGroup>::derived(b"80s-hits");
        let song = Id::<Recording>::derived(b"only-here");
        v.assignments.push(GenreAssignment {
            subject: GenreOn::ReleaseGroup(comp),
            genre: Ref::to(pop),
            basis: Source::Discogs,
            confidence: Confidence::Matched,
        });

        assert_eq!(
            v.effective_genres(song, &[(comp, true)]),
            BTreeSet::from([pop])
        );
    }

    /// The override, which is what makes the read rule a rule rather than a
    /// guess: where inheritance gets it wrong, an assignment on the recording
    /// settles it.
    #[test]
    fn a_recording_s_own_assignment_wins_outright() {
        let mut v = Vocabulary::default();
        let (rock, jazz) = (v.define("Rock"), v.define("Jazz"));
        let album = Id::<ReleaseGroup>::derived(b"album");
        let song = Id::<Recording>::derived(b"song");
        v.assignments.push(GenreAssignment {
            subject: GenreOn::ReleaseGroup(album),
            genre: Ref::to(rock),
            basis: Source::Discogs,
            confidence: Confidence::Matched,
        });
        v.assignments.push(GenreAssignment {
            subject: GenreOn::Recording(song),
            genre: Ref::to(jazz),
            basis: Source::Manual,
            confidence: Confidence::Confirmed,
        });

        assert_eq!(
            v.effective_genres(song, &[(album, false)]),
            BTreeSet::from([jazz]),
            "the album is not consulted at all once the track has its own"
        );
    }

    /// Multi-genre falls out of row existence rather than needing a list field.
    #[test]
    fn a_subject_can_carry_more_than_one_genre() {
        let mut v = Vocabulary::default();
        let (a, b) = (v.define("Electronic"), v.define("Ambient"));
        let group = Id::<ReleaseGroup>::derived(b"g");
        for g in [a, b] {
            v.assignments.push(GenreAssignment {
                subject: GenreOn::ReleaseGroup(group),
                genre: Ref::to(g),
                basis: Source::Discogs,
                confidence: Confidence::Matched,
            });
        }
        let song = Id::<Recording>::derived(b"s");
        assert_eq!(v.effective_genres(song, &[(group, false)]).len(), 2);
    }

    /// A concept's id comes from its name, so two devices defining `Techno`
    /// independently define the same concept.
    #[test]
    fn defining_a_concept_twice_defines_it_once() {
        let mut v = Vocabulary::default();
        assert_eq!(v.define("Techno"), v.define("techno"));
        assert_eq!(v.genres.len(), 1, "case is not a different concept");
    }

    #[test]
    fn the_vocabulary_round_trips_through_storage() {
        let (v, browse, electronic, _) = library_vocabulary();
        let bytes = postcard::to_allocvec(&v).unwrap();
        let back: Vocabulary = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back.under(browse, electronic), v.under(browse, electronic));
    }
}
