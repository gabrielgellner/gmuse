//! **L1's store: the folded catalogue.**
//!
//! Split out of `model/log.rs` by #193, which had held the facts and their
//! fold in one 3,917-line file — `Record` and `Fact` at the top, `Catalogue`
//! and `fold` below them. #191 could not separate `facts` from `projection`
//! while that was true, and noted so at the time.
//!
//! **Keyed by id and assembled into nothing.** It mirrors a log whose facts
//! name ids, which is why it already had the shape §5a asks for: a name once
//! per artist, credits keyed by `(subject, artist, role, source)`. The rows
//! a person reads are assembled from this by `Librarian`, which is L2's and
//! is rebuilt rather than patched.

use std::collections::{BTreeMap, BTreeSet};

use crate::audio_id::AudioId;
use crate::model::clock::Stamp;
use crate::model::entity::{
    Artist, Collection, Concept, Device, Encoding, Facet, Hierarchy, Label, Medium, Recording,
    Release, ReleaseGroup, Role, Track, View, Work,
};
use crate::model::facet::{ConceptRecord, Described, FacetRecord};
use crate::model::id::Id;
use crate::model::ingest::Confidence;
use crate::model::ord::OrdKey;
use crate::model::reference::{AliasKind, Cataloguable, CreditOn, RoleClass, Source};

use crate::facts::{ArtistEdge, Fact, Period, Record, Spectrum};
use crate::model::spine::PartialDate;
fn assign<S, C>(
    live: &mut BTreeMap<(S, C, Source), Confidence>,
    retracted: &mut BTreeMap<(S, C), Source>,
    subject: S,
    concept: C,
    basis: Source,
    confidence: Confidence,
) where
    S: Ord + Copy,
    C: Ord + Copy,
{
    // **Recorded whether or not it is shadowed.** Dropping it would destroy
    // the provenance saying where it came from, which is the thing that makes
    // an override inspectable rather than magic (#96).
    live.insert((subject, concept, basis), confidence);
    // **Precedence by basis** (#96). An import does not undo a person's
    // correction: without this, retracting a wrong Discogs genre lasts exactly
    // until the next Discogs pass, and an edit that does not survive a resync
    // is not an edit.
    if retracted
        .get(&(subject, concept))
        .is_none_or(|by| basis >= *by)
    {
        retracted.remove(&(subject, concept));
    }
}

/// Fold one retraction, **keyed one field shorter than what it shadows**.
///
/// A retraction answers the question rather than arguing with one asserter, so
/// it silences every basis at once. Keyed by basis instead, a person would have
/// to retract once per source that ever said it and the next import would put
/// it back.
///
/// The row is kept rather than dropped, because *decided against* and *nobody
/// has said* are different answers and only a row can tell them apart.
fn retract<S, C>(
    live: &mut BTreeMap<(S, C, Source), Confidence>,
    retracted: &mut BTreeMap<(S, C), Source>,
    subject: S,
    concept: C,
    basis: Source,
) where
    S: Ord + Copy,
    C: Ord + Copy,
{
    live.retain(|(s, c, _), _| (*s, *c) != (subject, concept));
    // The strongest voice that ruled it out, so a weaker retraction arriving
    // later cannot weaken a person's.
    let at = retracted.entry((subject, concept)).or_insert(basis);
    *at = (*at).max(basis);
}

/// Fold one claim that has no attribute to carry.
///
/// The row-existence form of [`assign`]: where an assignment splits into an
/// identity and an attribute, an edge is identity all the way down — there is
/// nothing to say about `Techno narrows Electronic` beyond that somebody said
/// it. So the key is the whole claim, and the fold is a set union.
fn assert_edge<K: Ord + Clone>(live: &mut BTreeSet<K>, retracted: &mut BTreeSet<K>, edge: K) {
    retracted.remove(&edge);
    live.insert(edge);
}

/// Fold one retraction of such a claim — the same rule as [`retract`].
///
/// The live row goes, a row saying it was **decided against** stays, and a
/// later assertion revives. Keeping the row is what separates "somebody ruled
/// this out" from "nobody has said", which an absence cannot express and a
/// review queue needs.
///
/// It differs from [`retract`] in exactly one place, and only because the
/// claims differ: an assignment's live key carries the basis that made it, so
/// retracting one has to sweep every asserter. An edge has no asserter in its
/// key, so there is one row to remove.
fn retract_edge<K: Ord + Clone>(live: &mut BTreeSet<K>, retracted: &mut BTreeSet<K>, edge: K) {
    live.remove(&edge);
    retracted.insert(edge);
}

/// What the catalogue looks like after folding a set of records.
///
/// Every collection is ordered, because "both devices fold to the same result"
/// has to be checkable byte for byte — a hash map would make the assertion
/// pass or fail on iteration order.
#[derive(Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Catalogue {
    pub artist_names: BTreeMap<Id<Artist>, String>,
    pub recordings: BTreeMap<Id<Recording>, Recorded>,
    pub encodings: BTreeMap<Id<Encoding>, Encoded>,
    /// Which recording each encoding holds, **independent of whether anything
    /// has described it**.
    ///
    /// `Encoded::recording` carries the same answer for encodings a scan has
    /// seen, and is what the projection reads. This is the one that survives a
    /// log with no scan beside it — the case a relink lives in.
    pub bound: BTreeMap<Id<Encoding>, Id<Recording>>,
    /// Every assertion, on every axis, **one row per asserter**.
    ///
    /// The key is the claim's identity and it includes *who says so*, because
    /// Discogs saying Techno and a person saying Techno are two independent
    /// pieces of evidence rather than one claim written twice. Folding them to
    /// one row would make the second silently overwrite the first and lose the
    /// agreement, which is the most useful thing the pair says.
    ///
    /// The value is the claim's one **attribute**: last writer wins per
    /// identity, so re-scoring an import (Discogs moving provisional to matched)
    /// updates in place, while a human's disagreement lands beside it.
    ///
    /// The facet is not in the key because it is already in the concept's id
    /// (#117) — `ambient` on two axes is two concepts, so one map holds both
    /// without a third field to keep in step.
    pub assertions: BTreeMap<(Described, Id<Concept>, Source), Confidence>,
    /// Pairs somebody has ruled out.
    ///
    /// **Keyed without a basis, unlike the assertions it shadows.** A person
    /// saying "this is not live" is answering the question, not arguing with
    /// one asserter, so a retraction silences every basis at once — otherwise
    /// retracting would mean retracting once per source that ever said it, and
    /// the next import would put it back.
    ///
    /// Kept rather than dropped, because *decided against* and *nobody has
    /// said* are different answers and only a row can tell them apart. A later
    /// assertion clears the entry, or a retraction would be a delete wearing
    /// another name.
    pub assertions_retracted: BTreeMap<(Described, Id<Concept>), Source>,
    /// Every saved view, by the id its name mints.
    pub views: BTreeMap<Id<View>, SavedView>,
    /// Views somebody has dropped. Kept, because *deleted* and *never existed*
    /// are different answers and only a row can tell them apart.
    pub views_dropped: BTreeSet<Id<View>>,
    /// What each collection is called, and whether its order means anything.
    pub collections: BTreeMap<Id<Collection>, Collected>,
    /// Every collection entry, **keyed by position before recording**.
    ///
    /// So that walking the set gives a collection's entries in list order —
    /// a `BTreeSet` over `(collection, ord, recording)` is already sorted the
    /// way the playlist reads, and the fold needs no comparator.
    ///
    /// Two devices inserting *different* recordings at the same point produce
    /// equal `ord`s, which is fine: the recording breaks the tie, and it
    /// breaks it the same way on both machines.
    pub entries: BTreeSet<(Id<Collection>, OrdKey, Id<Recording>)>,
    /// Entries somebody has taken out. Kept rather than deleted, for the same
    /// reason every other retraction is: an absence cannot tell "removed" from
    /// "never added", and a merge has to be able to.
    pub entries_retracted: BTreeSet<(Id<Collection>, OrdKey, Id<Recording>)>,
    /// Every other name anything is known by. Row existence, so curation
    /// unions across devices the way `narrows` does.
    pub aliases: BTreeSet<(Cataloguable, String, AliasKind)>,
    /// Every name anything was authoritatively called and is not called now
    /// — `Fact::HeadingRetired`. A subset of `aliases`, held apart rather than
    /// flagged inside it, because MADS/RDF's `DeprecatedAuthority` is a
    /// disjoint class from `Variant` and the two axes multiply.
    #[serde(default)]
    pub retired_headings: BTreeSet<(Cataloguable, String)>,
    /// One artist to another — `Fact::ArtistRelated`. Row existence, so two
    /// devices stating one relation union.
    ///
    /// **A graph, not a redirect.** `Merges` resolves a chain and the far end
    /// replaces the near one; here both ends stay and only a *view* collapses
    /// them, so the edges are kept as edges.
    #[serde(default)]
    pub artist_relations: BTreeSet<ArtistEdge>,
    /// When a relation held, where anybody said — keyed by the edge.
    ///
    /// Apart from the edge itself because it is the one part that can be
    /// restated: a membership whose end date arrives later is the same edge
    /// with more known about it, and folding it into the key would make it a
    /// second edge instead.
    #[serde(default)]
    pub relation_periods: BTreeMap<ArtistEdge, Period>,
    /// Every axis of description there is.
    pub facets: BTreeMap<Id<Facet>, FacetRecord>,
    /// Every term, on whichever axis it sits.
    ///
    /// **One list for every facet** (#117), where genre and version kept two.
    /// Nothing can collide: a concept's id seeds from its facet's key, so
    /// `ambient` the style and `ambient` the function are different rows.
    pub concepts: BTreeMap<Id<Concept>, ConceptRecord>,
    /// What each imprint is called.
    pub label_names: BTreeMap<Id<Label>, String>,
    /// ReplayGain per encoding, as the tags claim it.
    pub levels: BTreeMap<Id<Encoding>, crate::model::encoding::Levels>,
    /// What each hierarchy is called, and which axis it groups.
    pub hierarchies: BTreeMap<Id<Hierarchy>, (Id<Facet>, String)>,
    /// Every narrower-of edge. Row existence, so curation unions.
    pub narrows: BTreeSet<(Id<Hierarchy>, Id<Concept>, Id<Concept>)>,
    /// Edges somebody has ruled out. Kept for the reason
    /// [`Catalogue::assertions_retracted`] is: a decision and a silence are
    /// different answers, and only a row can tell them apart.
    pub narrows_retracted: BTreeSet<(Id<Hierarchy>, Id<Concept>, Id<Concept>)>,
    /// Every bytes-came-from-bytes edge — #132. Row existence, like the rest.
    pub encoding_derived: BTreeSet<(Id<Encoding>, Id<Encoding>)>,
    pub encoding_derived_retracted: BTreeSet<(Id<Encoding>, Id<Encoding>)>,
    /// Every recording-derived-from edge. Row existence, so curation unions
    /// exactly as `narrows` does (#79).
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
    /// The other traversal, one level up: work-derived-from-work.
    pub work_derived: BTreeSet<(
        Id<crate::model::entity::Work>,
        Id<crate::model::entity::Work>,
        crate::model::spine::WorkDerivationKind,
    )>,
    pub work_derived_retracted: BTreeSet<(
        Id<crate::model::entity::Work>,
        Id<crate::model::entity::Work>,
        crate::model::spine::WorkDerivationKind,
    )>,
    pub plays: BTreeMap<Id<Recording>, u32>,
    /// What each recording's identity was derived from.
    pub identified: BTreeMap<Id<Recording>, crate::model::basis::Basis>,
    /// When each recording entered the library, as the earliest anyone says.
    pub acquired: BTreeMap<Id<Recording>, i64>,
    /// The most recent play, in **seconds** since the epoch — the same unit
    /// as `acquired` and as the filter language's clock, not the stamp's.
    pub last_played: BTreeMap<Id<Recording>, i64>,
    /// The latest rating per recording. A fold, never a stored number.
    pub ratings: BTreeMap<Id<Recording>, u8>,
    /// Cutoff and the analyser that found it, per encoding.
    pub measurements: BTreeMap<Id<Encoding>, Spectrum>,
    pub groups: BTreeMap<Id<ReleaseGroup>, Grouped>,
    /// Which recordings have been folded into which.
    ///
    /// Applied to **history** — ratings, plays, acquisition — and to anything
    /// else keyed by a recording, so a merge moves what was said about the
    /// loser rather than stranding it.
    pub merges: crate::model::merge::Merges<Recording>,
    /// The same, for albums — #156. Kept apart because they answer different
    /// questions and a merge at one level implies nothing at the other.
    pub group_merges: crate::model::merge::Merges<ReleaseGroup>,
    /// The redirects an artist merge left — `Fact::ArtistMerged`.
    pub artist_merges: crate::model::merge::Merges<Artist>,
    /// **Bytes the library holds and has not named** — #160.
    ///
    /// What the tags claimed, for an encoding bound to no recording. Empty for
    /// every file an authority names, because those are catalogued outright.
    pub claims: BTreeMap<Id<Encoding>, (String, String)>,
    /// Every composition anyone has named.
    pub works: BTreeMap<Id<Work>, String>,
    /// Which recordings perform which works. Row existence: a recording can
    /// perform two, which is what a medley is.
    pub performances: BTreeSet<(Id<Recording>, Id<Work>)>,
    /// The role codelist.
    pub roles: BTreeMap<Id<Role>, (String, RoleClass)>,
    /// Every credit. Keyed by the claim, valued by how it was printed.
    pub credits: BTreeMap<(CreditOn, Id<Artist>, Id<Role>, Source), Credited>,
    /// Credits somebody has ruled out, and who ruled them out — the shadow
    /// [`Fact::CreditRetracted`] casts. Keyed without the `Source`, because a
    /// retraction answers the question rather than one asserter.
    pub credits_retracted: BTreeMap<(CreditOn, Id<Artist>, Id<Role>), Source>,
    /// Every pressing, by id.
    pub releases: BTreeMap<Id<Release>, Released>,
    /// Every disc, by id.
    pub media: BTreeMap<Id<Medium>, Pressed>,
    /// Every slot: which medium, which recording, which number.
    pub tracks: BTreeMap<Id<Track>, Placed>,
    /// Which slot each copy sits in. Row existence — a file is in one place,
    /// and two devices holding the same file agree about where.
    pub placed: BTreeMap<Id<Encoding>, Id<Track>>,
    /// Which copies a mastering tool re-encoded, and who says so — #143.
    ///
    /// A set of bases rather than one, because claims accumulate: the path
    /// saying it and a tagger saying it are two pieces of evidence, and their
    /// agreeing is the most useful thing the pair says.
    pub processed: BTreeMap<Id<Encoding>, BTreeSet<crate::model::reference::Source>>,
    /// Copies the library has retired — #159. Their rows are all still here,
    /// which is what makes the retirement reversible; what changes is that
    /// nothing counts them as a copy the library holds.
    pub retired: BTreeSet<Id<Encoding>>,
    /// What each device last said it holds, and when it said so.
    ///
    /// The stamp is not decoration. You cannot stat another machine's disk, so
    /// a remote holding is the claim "device X had this, there, as of X's last
    /// sync" — and a row that reads "on asnas, as of Tuesday" is honest where
    /// one that reads "on asnas" is not.
    pub holdings: BTreeMap<Id<Device>, Asserted>,
    /// Structural disagreements. Never resolved silently — §4.
    pub conflicts: Vec<Conflict>,
}

/// One device's holdings, as of one moment.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Asserted {
    pub at: Stamp,
    pub held: BTreeMap<Id<Encoding>, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Credited {
    pub position: u8,
    pub as_credited: Option<String>,
    pub join_phrase: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Released {
    pub group: Id<ReleaseGroup>,
    pub title: String,
    pub released: Option<PartialDate>,
    pub label: Option<Id<Label>>,
    #[serde(default)]
    pub media: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Pressed {
    pub release: Id<Release>,
    pub position: u16,
    pub format: Option<String>,
    #[serde(default)]
    pub tracks: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Placed {
    pub medium: Id<Medium>,
    pub recording: Id<Recording>,
    pub position: u16,
    pub title_on_release: Option<String>,
}

/// A saved view, as the fold has it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SavedView {
    pub name: String,
    /// The stack as somebody typed it — `in="Jazz" &year>=2015`.
    pub source: String,
}

/// A collection, as the fold has it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Collected {
    pub name: String,
    /// Whether the order means anything. A playlist is an ordered collection;
    /// an unordered one is a set somebody curated by hand.
    pub ordered: bool,
    /// The query it was frozen from, if it was frozen rather than hand-built.
    ///
    /// `None` is "somebody made this by hand", which is a different answer
    /// from "frozen from a query that happens to match nothing" — and only a
    /// distinct value can tell them apart.
    #[serde(default)]
    pub from: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Grouped {
    pub title: String,
    pub artist: Id<Artist>,
    pub first_released: Option<PartialDate>,
    #[serde(default)]
    pub kind: crate::model::spine::ReleaseType,
    #[serde(default)]
    pub secondary: std::collections::BTreeSet<crate::model::spine::ReleaseSecondary>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Recorded {
    pub title: String,
    pub artist: Id<Artist>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Encoded {
    pub audio: AudioId,
    pub recording: Option<Id<Recording>>,
    pub format: Option<crate::model::encoding::Format>,
    pub bitrate: u32,
    /// Playing time in milliseconds. Zero means nobody has said.
    pub length_ms: u32,
    pub sample_rate: Option<u32>,
    pub channels: Option<u8>,
}

/// Two devices bound one encoding to different recordings, from the same base.
///
/// The winner is shown, because a library that refuses to answer is worse than
/// one that answers and says so. The losers are kept, because "stop and show
/// me" needs something to show.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Conflict {
    pub encoding: Id<Encoding>,
    pub base: Option<Id<Recording>>,
    /// Every value asserted, ordered by stamp. The last one is what shows.
    pub asserted: Vec<(Stamp, Id<Recording>)>,
}

impl Conflict {
    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — a conflict's winner and losers, which no command surfaces"
    )]
    pub fn winner(&self) -> Id<Recording> {
        self.asserted.last().expect("a conflict has candidates").1
    }

    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — conflicts, the genre backlog and compaction, none of which a command surfaces"
    )]
    /// The recordings this collision left behind.
    ///
    /// One encoding cannot be two performances, so every loser here is the
    /// same song as the winner under a different id — which is a **merge**
    /// waiting to be proposed, not rubbish waiting to be collected.
    pub fn losers(&self) -> impl Iterator<Item = Id<Recording>> + '_ {
        let winner = self.winner();
        self.asserted
            .iter()
            .map(|(_, r)| *r)
            .filter(move |r| *r != winner)
    }
}

/// Every recording asserted for one `(encoding, base)` pair, in stamp order.
/// Grouping by the **base** is what distinguishes a disagreement from a
/// correction: two writes from the same base disagree, where a write from a
/// base someone else established is a correction of it.
type Bindings = BTreeMap<(Id<Encoding>, Option<Id<Recording>>), Vec<(Stamp, Id<Recording>)>>;

/// Fold a set of records into the catalogue they describe.
///
/// Sorting first is what makes the result independent of the order records
/// arrived in, which is the property the whole no-coordinator design rests on:
/// merge A into B or B into A and the answer is the same.
pub fn fold(records: &BTreeSet<Record>) -> Catalogue {
    let mut cat = Catalogue::default();
    let mut bindings: Bindings = BTreeMap::new();
    // Every placement each copy has ever been given, in stamp order, so the
    // pass below can prefer the best-evidenced rather than the last.
    let mut claimed: BTreeMap<Id<Encoding>, Vec<(Id<Track>, Source)>> = BTreeMap::new();

    // **Merges first, in their own pass.** A merge recorded on Tuesday has to
    // move a rating given on Monday, so it cannot be applied as it is reached
    // — every recording named below is resolved through the finished set.
    for Record { fact, .. } in records {
        match fact {
            Fact::RecordingMerged { loser, winner } => {
                let _ = cat.merges.merge(*loser, *winner);
            }
            Fact::ReleaseGroupMerged { loser, winner } => {
                let _ = cat.group_merges.merge(*loser, *winner);
            }
            Fact::ArtistMerged { loser, winner } => {
                let _ = cat.artist_merges.merge(*loser, *winner);
            }
            _ => {}
        }
    }
    let of = |r: &Id<Recording>| cat.merges.resolve(*r);
    let group_of = |g: &Id<ReleaseGroup>| cat.group_merges.resolve(*g);
    // **Every artist named below**, because a merge has to move a credit given
    // before it just as it moves a rating. The name goes through too: the
    // loser's label lands on the winner and the last writer wins, which is the
    // same rule `RecordingCatalogued`'s title already follows through `of`.
    let who = |a: &Id<Artist>| cat.artist_merges.resolve(*a);
    let described = |s: &Described| match s {
        Described::Recording(r) => Described::Recording(cat.merges.resolve(*r)),
        // A genre asserted on the loser is a genre on the album — #156.
        Described::ReleaseGroup(g) => Described::ReleaseGroup(cat.group_merges.resolve(*g)),
    };
    let credit_on = |s: &CreditOn| match s {
        CreditOn::Recording(r) => CreditOn::Recording(cat.merges.resolve(*r)),
        // And so is the album credit, which is what `file_under` asks for
        // first: a merge that left it behind would file the two halves of one
        // record under different artists.
        CreditOn::ReleaseGroup(g) => CreditOn::ReleaseGroup(cat.group_merges.resolve(*g)),
        other => *other,
    };

    for Record { stamp, fact } in records {
        match fact {
            Fact::ArtistNamed { artist, name } => {
                // Last writer wins, and the set is already in stamp order.
                cat.artist_names.insert(who(artist), name.clone());
            }
            Fact::RecordingCatalogued {
                recording,
                title,
                artist,
            } => {
                cat.recordings.insert(
                    of(recording),
                    Recorded {
                        title: title.clone(),
                        artist: who(artist),
                    },
                );
            }
            Fact::EncodingDescribed {
                encoding,
                audio,
                format,
                bitrate,
                length_ms,
                sample_rate,
                channels,
            } => {
                cat.encodings.entry(*encoding).or_insert(Encoded {
                    audio: *audio,
                    recording: None,
                    format: *format,
                    bitrate: *bitrate,
                    length_ms: *length_ms,
                    sample_rate: *sample_rate,
                    channels: *channels,
                });
            }
            Fact::EncodingBound { encoding, from, to } => {
                bindings
                    .entry((*encoding, from.map(|f| of(&f))))
                    .or_default()
                    .push((*stamp, of(to)));
            }
            Fact::ViewSaved { view, name, source } => {
                cat.views.insert(
                    *view,
                    SavedView {
                        name: name.clone(),
                        source: source.clone(),
                    },
                );
                cat.views_dropped.remove(view);
            }
            Fact::ViewDropped { view } => {
                cat.views.remove(view);
                cat.views_dropped.insert(*view);
            }
            Fact::CollectionNamed {
                collection,
                name,
                ordered,
            } => {
                // The source survives a rename: it is a claim about where the
                // membership came from, not about what the list is called.
                let from = cat.collections.get(collection).and_then(|c| c.from.clone());
                cat.collections.insert(
                    *collection,
                    Collected {
                        name: name.clone(),
                        ordered: *ordered,
                        from,
                    },
                );
            }
            Fact::CollectionFrozen { collection, source } => {
                if let Some(c) = cat.collections.get_mut(collection) {
                    c.from = Some(source.clone());
                }
            }
            Fact::CollectionEntryAdded {
                collection,
                recording,
                ord,
            } => assert_edge(
                &mut cat.entries,
                &mut cat.entries_retracted,
                (*collection, ord.clone(), of(recording)),
            ),
            Fact::CollectionEntryRemoved {
                collection,
                recording,
                ord,
            } => retract_edge(
                &mut cat.entries,
                &mut cat.entries_retracted,
                (*collection, ord.clone(), of(recording)),
            ),
            Fact::FacetDefined { facet, key, label } => {
                cat.facets.insert(
                    *facet,
                    FacetRecord {
                        key: key.clone(),
                        label: label.clone(),
                    },
                );
            }
            Fact::ConceptCatalogued {
                facet,
                concept,
                label,
                node_label,
            } => {
                cat.concepts.insert(
                    *concept,
                    ConceptRecord {
                        facet: *facet,
                        label: label.clone(),
                        node_label: *node_label,
                    },
                );
            }
            Fact::HierarchyDefined {
                hierarchy,
                facet,
                name,
            } => {
                cat.hierarchies.insert(*hierarchy, (*facet, name.clone()));
            }
            Fact::EncodingDerived { from, to } => assert_edge(
                &mut cat.encoding_derived,
                &mut cat.encoding_derived_retracted,
                (*from, *to),
            ),
            Fact::EncodingDerivationRetracted { from, to } => retract_edge(
                &mut cat.encoding_derived,
                &mut cat.encoding_derived_retracted,
                (*from, *to),
            ),
            Fact::RecordingDerived { from, to, kind } => assert_edge(
                &mut cat.derived,
                &mut cat.derived_retracted,
                (*from, *to, *kind),
            ),
            Fact::RecordingDerivationRetracted { from, to, kind } => retract_edge(
                &mut cat.derived,
                &mut cat.derived_retracted,
                (*from, *to, *kind),
            ),
            Fact::WorkDerived { from, to, kind } => assert_edge(
                &mut cat.work_derived,
                &mut cat.work_derived_retracted,
                (*from, *to, *kind),
            ),
            Fact::WorkDerivationRetracted { from, to, kind } => retract_edge(
                &mut cat.work_derived,
                &mut cat.work_derived_retracted,
                (*from, *to, *kind),
            ),
            Fact::HierarchyNarrowed {
                hierarchy,
                parent,
                child,
            } => assert_edge(
                &mut cat.narrows,
                &mut cat.narrows_retracted,
                (*hierarchy, *parent, *child),
            ),
            Fact::HierarchyNarrowRetracted {
                hierarchy,
                parent,
                child,
            } => retract_edge(
                &mut cat.narrows,
                &mut cat.narrows_retracted,
                (*hierarchy, *parent, *child),
            ),
            Fact::ConceptAsserted {
                subject,
                concept,
                basis,
                confidence,
            } => assign(
                &mut cat.assertions,
                &mut cat.assertions_retracted,
                described(subject),
                *concept,
                *basis,
                *confidence,
            ),
            Fact::AliasAsserted {
                subject,
                value,
                kind,
            } => {
                cat.aliases.insert((*subject, value.clone(), *kind));
            }
            // Both, and deliberately: a heading the library retired is still a
            // name it answers to, so it lands in `aliases` too — as an
            // `ArtistName`, because it was the authorised one and a printed
            // name is what it stays.
            Fact::HeadingRetired { subject, value } => {
                cat.aliases
                    .insert((*subject, value.clone(), AliasKind::ArtistName));
                cat.retired_headings.insert((*subject, value.clone()));
            }
            Fact::ArtistRelated {
                from,
                to,
                kind,
                period,
            } => {
                // **Both ends resolved through the merges**, because a merge
                // that ran after the relation was stated would otherwise
                // leave an edge pointing at a row nothing else uses.
                let edge = (
                    cat.artist_merges.resolve(*from),
                    cat.artist_merges.resolve(*to),
                    *kind,
                );
                cat.artist_relations.insert(edge);
                if let Some(span) = period {
                    cat.relation_periods.insert(edge, *span);
                }
            }
            Fact::ConceptRetracted {
                subject,
                concept,
                basis,
            } => retract(
                &mut cat.assertions,
                &mut cat.assertions_retracted,
                described(subject),
                *concept,
                *basis,
            ),
            Fact::HoldingsAsserted { device, held } => {
                // Records arrive in stamp order, so the last one simply wins.
                cat.holdings.insert(
                    *device,
                    Asserted {
                        at: *stamp,
                        held: held.clone(),
                    },
                );
            }
            Fact::ReleaseGroupCatalogued {
                group,
                title,
                artist,
                first_released,
                kind,
                secondary,
            } => {
                cat.groups.insert(
                    // **Through the merge**, so the loser keeps no album row
                    // of its own — the same shape `RecordingCatalogued` takes,
                    // and what stops a merged album still drawing a node.
                    group_of(group),
                    Grouped {
                        title: title.clone(),
                        // **Through the merge too**, and this is the one that
                        // reaches a folder: `file_under` asks the group for its
                        // artist first, so a split left here files the two
                        // halves of one artist's record apart.
                        artist: who(artist),
                        first_released: *first_released,
                        kind: *kind,
                        secondary: secondary.clone(),
                    },
                );
            }
            Fact::WorkCatalogued { work, title } => {
                cat.works.insert(*work, title.clone());
            }
            Fact::WorkPerformed { recording, work } => {
                cat.performances.insert((of(recording), *work));
            }
            Fact::RoleDefined { role, name, class } => {
                cat.roles.insert(*role, (name.clone(), *class));
            }
            Fact::CreditAsserted {
                subject,
                artist,
                role,
                basis,
                position,
                as_credited,
                join_phrase,
            } => {
                // **A later assertion lifts the shadow**, because the two
                // are one question answered twice and the later answer stands.
                cat.credits_retracted
                    .remove(&(credit_on(subject), who(artist), *role));
                cat.credits.insert(
                    // **The artist through the merge**, which is what collapses
                    // the two halves of a split credit into one row rather than
                    // leaving `line_of` to render the same person twice.
                    (credit_on(subject), who(artist), *role, *basis),
                    Credited {
                        position: *position,
                        as_credited: as_credited.clone(),
                        join_phrase: join_phrase.clone(),
                    },
                );
            }
            // Applied in the pass below, because a retitle has to outlast a
            // scan rather than merely follow it.
            Fact::RecordingRetitled { .. } | Fact::ReleaseGroupRetitled { .. } => {}
            Fact::EncodingClaimed {
                encoding,
                artist,
                title,
            } => {
                // Last writer wins, as every descriptive fact does: a retag
                // that changes what the file says changes what it claims.
                cat.claims
                    .insert(*encoding, (artist.clone(), title.clone()));
            }
            Fact::CreditRetracted {
                subject,
                artist,
                role,
                basis,
            } => {
                let key = (credit_on(subject), who(artist), *role);
                // Every basis at once — see the fact's own doc for why it is
                // keyed one field shorter than the assertion it shadows.
                cat.credits.retain(|(s, a, r, _), _| (*s, *a, *r) != key);
                // The strongest voice that ruled it out, so a weaker
                // retraction arriving later cannot weaken a person's.
                let at = cat.credits_retracted.entry(key).or_insert(*basis);
                *at = (*at).max(*basis);
            }
            Fact::EncodingPlaced {
                encoding,
                track,
                basis,
            } => {
                // Kept in full and resolved below, because the last word is
                // not always the best one — see the pass after this loop.
                cat.placed.insert(*encoding, *track);
                claimed.entry(*encoding).or_default().push((*track, *basis));
            }
            // **Last word wins**, because these come in a pair: a copy retired
            // and brought back and retired again is retired, and the set is
            // already in stamp order.
            Fact::EncodingProcessed { encoding, basis } => {
                cat.processed.entry(*encoding).or_default().insert(*basis);
            }
            Fact::EncodingRetired { encoding } => {
                cat.retired.insert(*encoding);
            }
            Fact::EncodingRestored { encoding } => {
                cat.retired.remove(encoding);
            }
            Fact::EncodingLevelled { encoding, levels } => {
                // Last writer wins: re-tagging a file is a correction, and
                // there is nothing to reconcile between two claims about one
                // encoding's loudness.
                cat.levels.insert(*encoding, *levels);
            }
            Fact::LabelNamed { label, name } => {
                cat.label_names.insert(*label, name.clone());
            }
            Fact::ReleaseCatalogued {
                release,
                group,
                title,
                released,
                label,
                media,
            } => {
                cat.releases.insert(
                    *release,
                    Released {
                        // **The pressing stays; only which album it is on
                        // moves.** That is the whole point of merging at this
                        // level: *Barton Hollow* is one album holding a
                        // standard edition and a deluxe, and folding the
                        // releases together would lose the difference.
                        group: group_of(group),
                        title: title.clone(),
                        released: *released,
                        label: *label,
                        media: *media,
                    },
                );
            }
            Fact::MediumCatalogued {
                medium,
                release,
                position,
                format,
                tracks,
            } => {
                cat.media.insert(
                    *medium,
                    Pressed {
                        release: *release,
                        position: *position,
                        format: format.clone(),
                        tracks: *tracks,
                    },
                );
            }
            Fact::TrackPlaced {
                track,
                medium,
                recording,
                position,
                title_on_release,
            } => {
                cat.tracks.insert(
                    *track,
                    Placed {
                        medium: *medium,
                        recording: of(recording),
                        position: *position,
                        title_on_release: title_on_release.clone(),
                    },
                );
            }
            Fact::EncodingMeasured {
                encoding,
                cutoff_hz,
                cliff_db,
                analyser,
            } => {
                // Last measurement wins, which is what a re-measure means.
                cat.measurements.insert(
                    *encoding,
                    Spectrum {
                        cutoff_hz: *cutoff_hz,
                        cliff_db: *cliff_db,
                        analyser: *analyser,
                    },
                );
            }
            Fact::RecordingRated { recording, stars } => {
                // Records arrive in stamp order, so the last simply wins.
                match stars {
                    Some(n) => {
                        cat.ratings.insert(of(recording), *n);
                    }
                    None => {
                        cat.ratings.remove(&of(recording));
                    }
                }
            }
            Fact::RecordingPlayed { recording, .. } => {
                *cat.plays.entry(of(recording)).or_insert(0) += 1;
                // The latest play, by the wall time the play happened, which
                // is what "days since played" is asking about. The stamp
                // orders the log; this answers the question.
                //
                // **In seconds**, like every other fact and like the filter
                // language's clock. A stamp is in milliseconds, and handing
                // that straight out made "played a minute ago" read as fifty
                // years from now.
                let seconds = stamp.wall / 1_000;
                let latest = cat.last_played.entry(of(recording)).or_insert(seconds);
                *latest = (*latest).max(seconds);
            }
            Fact::RecordingIdentified { recording, basis } => {
                // **No conflict rule, because there cannot be a conflict.**
                //
                // The id is `of_basis(basis)`, so two different bases reaching
                // one recording would be a UUIDv5 collision. Every fact naming
                // this id carries the same basis, and the only thing two
                // devices can disagree about is whether they have said it yet.
                //
                // A "best rung wins" comparison was written here first and is
                // gone: a mutation that reversed it failed no test, which is
                // the signature of a branch that cannot run. A file that later
                // gains a MusicBrainz id does not update this row — it mints a
                // *different* recording, and the two are joined by a merge.
                cat.identified
                    .entry(*recording)
                    .or_insert_with(|| basis.clone());
            }
            // Applied in the pass above, because a merge has to move what was
            // said before it.
            Fact::RecordingMerged { .. }
            | Fact::ReleaseGroupMerged { .. }
            | Fact::ArtistMerged { .. } => {}
            Fact::RecordingAcquired { recording, at } => {
                let first = cat.acquired.entry(of(recording)).or_insert(*at);
                *first = (*first).min(*at);
            }
        }
    }

    // **A placement onto a MusicBrainz release outranks one onto a name.**
    //
    // Last writer wins was the rule and it loses real information. A release
    // is minted from its MusicBrainz id where the tags carry one and from its
    // release group otherwise (`of_release`), and **identical audio is one
    // encoding however many files hold it** — so scanning a badly tagged copy
    // re-places the well tagged one.
    //
    // Measured: `Luna` is held under `~/MusicLibrary` with a MusicBrainz album
    // id and was held under `~/DanceMusic-staging` with seven tags and none.
    // Same audio, so the same 32 encodings — and importing the staging folder
    // moved all 32 onto a medium minted from the album *name*, off the
    // MusicBrainz-derived medium where their catalogued tracklist sits. The
    // tracklist then appeared to have attached to nothing. 39 files across the
    // library were sitting on the weaker medium when this was written.
    //
    // The rung is computable rather than recorded: a release equal to
    // `of_release(None, group)` was minted from the group, and anything else
    // came from an id. So no fact needs a new field to say which it was.
    //
    // Resolved after the loop because a placement can be folded before the
    // release it points at, and the rung is a property of that release.
    // **And who said so outranks both** — #170. Where two placements name
    // MusicBrainz-derived releases the computed rung cannot separate them, so
    // recency decides and a scan is always the most recent thing that ran.
    // `Source` is what breaks that tie: a pass that read a whole tracklist and
    // matched it against every file held says more than a tag does.
    for (encoding, tracks) in &claimed {
        let rung = |t: &Id<Track>| {
            cat.tracks
                .get(t)
                .and_then(|slot| cat.media.get(&slot.medium))
                .and_then(|m| cat.releases.get(&m.release).map(|r| (m.release, r.group)))
                .is_some_and(|(release, group)| release != Id::<Release>::of_release(None, group))
        };
        let strongest = tracks.iter().map(|(_, b)| *b).max();
        if let Some(best) = tracks
            .iter()
            .rev()
            .filter(|(_, b)| Some(*b) == strongest)
            .find(|(t, _)| rung(t))
            .or_else(|| tracks.iter().rev().find(|(_, b)| Some(*b) == strongest))
        {
            cat.placed.insert(*encoding, best.0);
        }
    }

    // **Retitles last, and that is a rung rather than an ordering trick.**
    //
    // Last writer wins is the rule for a descriptive fact, and it does not
    // work here: `query` and `apply` bring their view up to date by scanning
    // the disk, and a scan re-asserts `RecordingCatalogued` from the tags with
    // a fresh stamp. Worse, it asserts it against the *current* recording id —
    // so after a relink the fact is new rather than a repeat, `drop_repeats`
    // keeps it, and it lands on top of anything written earlier.
    //
    // Measured the hard way: 23 retitles applied cleanly and not one of them
    // was visible afterwards, because the next read re-scanned over them.
    //
    // So a retitle is curation and a scan is a reading, and curation outranks
    // a reading whenever it was written. That is the same relationship
    // `Source` encodes for a claim; `RecordingCatalogued` carries no rung, and
    // giving it one is a larger change than this needs.
    for Record { fact, .. } in records {
        match fact {
            Fact::RecordingRetitled { recording, title } => {
                if let Some(recorded) = cat.recordings.get_mut(&of(recording)) {
                    recorded.title = title.clone();
                }
            }
            Fact::ReleaseGroupRetitled { group, title } => {
                if let Some(album) = cat.groups.get_mut(&group_of(group)) {
                    album.title = title.clone();
                }
            }
            _ => {}
        }
    }

    for ((encoding, base), mut asserted) in bindings {
        asserted.sort();
        let distinct: BTreeSet<_> = asserted.iter().map(|(_, r)| *r).collect();
        let winner = asserted.last().expect("non-empty by construction").1;

        // **Recorded whether or not anyone has described these bytes.**
        //
        // This used to write the binding onto the `Encoded` row and drop it
        // when there was none, which made a relink invisible to anything that
        // had not also scanned. `ingest` checks exactly this before minting —
        // "a binding someone made wins over the one we would mint" — so
        // dropping it defeated the one guard stopping a scan from silently
        // undoing an identification. Applying 377 of them left the work list
        // where it was, and that is how it was found.
        //
        // A binding is a fact about an encoding *id*. Whether its bytes have
        // been described is a different fact and a different question.
        cat.bound.insert(encoding, winner);
        if let Some(d) = cat.encodings.get_mut(&encoding) {
            d.recording = Some(winner);
        }
        if distinct.len() > 1 {
            cat.conflicts.push(Conflict {
                encoding,
                base,
                asserted,
            });
        }
    }
    cat.conflicts.sort_by_key(|c| c.encoding);
    cat
}

#[allow(
    dead_code,
    reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — the genre curation backlog, which no command asks for"
)]
impl Catalogue {
    /// Live assertions about one subject, on one axis.
    ///
    /// **The reader's half of one map holding every facet** (#117). Callers
    /// used to reach for `genres` or `qualifiers` and get the axis for free;
    /// now they name it, which is the point — asking for "the genres" without
    /// saying so was what let a single-value row shape look reasonable.
    ///
    /// Retracted pairs are gone and node labels never appear: ISO 25964's rule
    /// is that a term put into an array to organise it does not take part in
    /// retrieval.
    pub fn asserted(
        &self,
        subject: Described,
        facet: Id<Facet>,
    ) -> Vec<(Id<Concept>, Source, Confidence)> {
        self.assertions
            .iter()
            .filter(|((s, c, _), _)| {
                *s == subject
                    && !self.assertions_retracted.contains_key(&(*s, *c))
                    && self
                        .concepts
                        .get(c)
                        .is_some_and(|r| r.facet == facet && !r.node_label)
            })
            .map(|((_, c, basis), confidence)| (*c, *basis, *confidence))
            .collect()
    }

    /// The same, as the labels a person reads, deduplicated and sorted.
    pub fn labels_on(&self, subject: Described, facet_key: &str) -> Vec<&str> {
        let facet = Id::<Facet>::of_key(facet_key);
        let mut out: Vec<&str> = self
            .asserted(subject, facet)
            .into_iter()
            .filter_map(|(c, _, _)| self.concepts.get(&c).map(|r| r.label.as_str()))
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Concepts this hierarchy does not place — a standing review query.
    ///
    /// **An orphan concept is legitimate, not an error.** A genre defined at
    /// listen time, on the track that prompted it, has no reason to wait for
    /// somebody to decide where it sits: assignment does not wait for
    /// placement, and refusing the assignment until it does would mean the
    /// moment you know the answer is the moment you cannot record it.
    ///
    /// What it costs is reachability. `genre+ = Electronic` walks edges, so an
    /// unplaced concept is invisible to every closure query until an edge
    /// exists — found only by naming it exactly. That is why this is a query
    /// rather than a rule: the concepts are fine, and the list of them is the
    /// curation backlog.
    /// **The axis is an argument, not read off the hierarchy.** A hierarchy
    /// need not be declared before an edge names it — `narrows` mints the
    /// edge, not the array — so deriving the facet from
    /// [`Catalogue::hierarchies`] would silently return nothing for exactly
    /// the vocabularies still being built, which is when a backlog matters.
    pub fn unplaced_concepts(
        &self,
        hierarchy: Id<Hierarchy>,
        facet: Id<Facet>,
    ) -> BTreeSet<Id<Concept>> {
        let placed: BTreeSet<Id<Concept>> = self
            .narrows
            .iter()
            .filter(|(h, _, _)| *h == hierarchy)
            .flat_map(|(_, parent, child)| [*parent, *child])
            .collect();
        // **Only this hierarchy's own axis**, or every version concept would
        // read as an unplaced genre the moment one list held both (#117).
        self.concepts
            .iter()
            .filter(|(id, c)| c.facet == facet && !placed.contains(*id))
            .map(|(id, _)| *id)
            .collect()
    }

    /// The merges the collisions are asking for.
    ///
    /// **Why an orphan is left rather than swept.** A recording that nothing
    /// holds is an ordinary row — §1 is explicit that a wishlist entry is
    /// exactly one of those, so "delete what nothing points at" would delete
    /// everything you have decided about and do not own. Structurally the two
    /// are identical.
    ///
    /// The model can still tell them apart, because a collision *names* its
    /// loser. So the answer is not to collect the orphan but to propose
    /// retiring it: one encoding cannot be two performances, so the pair is
    /// the same song twice and a redirect (§3) settles it without deleting
    /// anything. This is what feeds the staged queue in §3 — the machine finds
    /// them, a person decides.
    pub fn proposed_merges(&self) -> Vec<(Id<Recording>, Id<Recording>)> {
        let mut out: Vec<_> = self
            .conflicts
            .iter()
            .flat_map(|c| c.losers().map(move |l| (l, c.winner())))
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Every device that claims to hold this encoding, with where and when.
    ///
    /// Deliberately not "is it available": that needs to know which device is
    /// *this* one and which are reachable right now, neither of which is a fact
    /// about the library. #38 layers Local / Remote / Absent over this.
    pub fn holders(&self, encoding: Id<Encoding>) -> Vec<(Id<Device>, &str, Stamp)> {
        self.holdings
            .iter()
            .filter_map(|(device, a)| {
                a.held
                    .get(&encoding)
                    .map(|locator| (*device, locator.as_str(), a.at))
            })
            .collect()
    }
}

/// Drop the holdings assertions nothing reads any more.
///
/// **The one event type that may be compacted.** A superseded assertion
/// carries nothing — the later one says everything it said and more — where a
/// superseded play still carries a listen and a superseded rating still carries
/// what someone once thought.
///
/// Without this the log grows by a full file listing every time a device scans.
/// Measured on the live library, scan observations were 6227 of 6572 events
/// after three days of use: 95% of the log, and none of it worth keeping.
///
/// Folding a compacted set gives exactly what folding the original gives, which
/// is the property that makes this safe to run whenever, and the one the tests
/// assert directly.
/// Breadth-first, so `depth` is the shortest way back to each ancestor, and
/// `seen` is what stops a cycle rather than the bound. **The bound is the
/// backstop**: edges are rows and rows union, so two devices can author a
/// cycle neither of them meant, and a render must answer rather than hang.
///
/// Returns `(kind, depth)` per edge taken, which is what makes the bracket
/// correlated: the kind and the depth come from the same hop.
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

impl crate::query::filter::Listened for Catalogue {
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

#[cfg(test)]
mod closure_tests {
    use super::*;
    use crate::model::entity::{Device, Hierarchy};
    use crate::model::instance::Instance;
    use crate::query::filter::Listened;

    fn built(edges: &[(&str, &str)]) -> Catalogue {
        let mut i = Instance::new(Id::<Device>::derived(b"test"));
        let h = Id::<Hierarchy>::derived(b"browse");
        for (parent, child) in edges {
            for label in [parent, child] {
                i.assert_concept(
                    100,
                    crate::model::facet::Described::Recording(Id::derived(b"anything")),
                    crate::model::facet::GENRE,
                    label,
                    crate::model::reference::Source::Manual,
                    crate::model::ingest::Confidence::Confirmed,
                );
            }
            i.narrows(100, h, crate::model::facet::GENRE, parent, child);
        }
        i.catalogue()
    }

    /// **The derivation walk is bounded and cycle-safe** — #79, and the one
    /// thing `domain-model.md` §5 says is not optional.
    ///
    /// Edges are rows and rows union, so one device saying `A derives B` while
    /// another says `B derives A` produces a cycle neither authored. An
    /// unbounded walk hangs the render; this answers.
    #[test]
    fn a_derivation_cycle_answers_rather_than_hanging() {
        use crate::model::spine::RecordingDerivationKind::Remix;
        use crate::query::filter::Listened;

        let song = |n: u8| Id::<Recording>::derived(&[n]);
        let mut i = crate::model::instance::Instance::new(Id::derived(b"laptop"));
        // A chain that closes on itself: 1 → 2 → 3 → 1.
        i.derives(100, song(1), song(2), Remix);
        i.derives(100, song(2), song(3), Remix);
        i.derives(100, song(3), song(1), Remix);

        let cat = i.catalogue();
        let up = cat.derived_from(song(3));
        assert_eq!(
            up.len(),
            2,
            "each ancestor is reached once, and the loop did not repeat them: {up:?}"
        );
        assert!(up.iter().all(|(_, depth)| *depth <= 16), "{up:?}");
    }

    /// **The depth is the shortest way back**, and a retraction takes an edge
    /// out of the walk without deleting the fact.
    #[test]
    fn a_derivation_walk_reports_hops_and_respects_a_retraction() {
        use crate::model::spine::RecordingDerivationKind::{Edit, Remix};
        use crate::query::filter::Listened;

        let song = |n: u8| Id::<Recording>::derived(&[n]);
        let mut i = crate::model::instance::Instance::new(Id::derived(b"laptop"));
        // 1 is the original, 2 is a remix of it, 3 is an edit of the remix.
        i.derives(100, song(1), song(2), Remix);
        i.derives(100, song(2), song(3), Edit);

        let cat = i.catalogue();
        let mut up = cat.derived_from(song(3));
        up.sort();
        assert_eq!(up, [("edit".to_string(), 1), ("remix".to_string(), 2)]);

        i.retract_derivation(200, song(1), song(2), Remix);
        assert_eq!(
            i.catalogue().derived_from(song(3)),
            [("edit".to_string(), 1)],
            "a retracted edge was still walked"
        );
    }

    /// **Closure walks up, at any depth** — #93.
    #[test]
    fn broader_reaches_every_ancestor() {
        let cat = built(&[("Music", "Electronic"), ("Electronic", "Techno")]);
        let up = cat.broader(crate::model::facet::GENRE, "Techno");
        assert!(up.contains(&"Electronic".to_string()), "{up:?}");
        assert!(
            up.contains(&"Music".to_string()),
            "not at any depth: {up:?}"
        );
        assert!(
            !up.contains(&"Techno".to_string()),
            "a concept is not broader than itself"
        );
    }

    /// **A cycle must not hang a render** — #79's requirement, and the reason
    /// the bound is not optional.
    ///
    /// A hierarchy is curated by hand, so a loop is a question of when rather
    /// than whether: `Techno` narrower-of `Rock` narrower-of `Techno` is two
    /// edits nobody would make together and somebody eventually will.
    #[test]
    fn a_cycle_in_the_hierarchy_terminates() {
        let cat = built(&[("Rock", "Techno"), ("Techno", "Rock")]);
        let up = cat.broader(crate::model::facet::GENRE, "Techno");
        assert!(up.contains(&"Rock".to_string()), "{up:?}");
        // It returns, which is the whole assertion — and each concept once.
        assert_eq!(up.len(), 2, "walked a loop more than once: {up:?}");
    }

    /// A withdrawn edge stops being walked, or repairing a wrong placement
    /// would mean the hierarchy could only ever grow.
    #[test]
    fn a_retracted_edge_is_not_climbed() {
        let mut i = Instance::new(Id::<Device>::derived(b"test"));
        let h = Id::<Hierarchy>::derived(b"browse");
        for label in ["Rock", "Techno"] {
            i.assert_concept(
                100,
                crate::model::facet::Described::Recording(Id::derived(b"anything")),
                crate::model::facet::GENRE,
                label,
                crate::model::reference::Source::Manual,
                crate::model::ingest::Confidence::Confirmed,
            );
        }
        i.narrows(100, h, crate::model::facet::GENRE, "Rock", "Techno");
        assert_eq!(
            i.catalogue().broader(crate::model::facet::GENRE, "Techno"),
            ["Rock"]
        );

        i.retract_narrowing(200, h, crate::model::facet::GENRE, "Rock", "Techno");
        assert!(
            i.catalogue()
                .broader(crate::model::facet::GENRE, "Techno")
                .is_empty(),
            "a withdrawn edge was still climbed"
        );
    }
}

#[cfg(test)]
mod history_tests {
    use super::*;
    use crate::model::clock::HybridClock;
    use crate::model::entity::Device;

    fn device(n: u128) -> Id<Device> {
        Id::from_uuid(uuid::Uuid::from_u128(n))
    }

    fn recording(n: u128) -> Id<Recording> {
        Id::from_uuid(uuid::Uuid::from_u128(n))
    }

    /// **The earliest wins, whatever order it is heard in.** Min is
    /// commutative, associative and idempotent, so two devices converge on the
    /// acquisition date with no rule to apply and nothing to surface.
    #[test]
    fn acquired_folds_to_the_earliest_anyone_says() {
        let rec = recording(1);
        let mut clock = HybridClock::new(device(1));
        // Deliberately out of order: the later claim is stamped first.
        let records: BTreeSet<Record> = [
            Record {
                stamp: clock.stamp(1_000),
                fact: Fact::RecordingAcquired {
                    recording: rec,
                    at: 9_999,
                },
            },
            Record {
                stamp: clock.stamp(2_000),
                fact: Fact::RecordingAcquired {
                    recording: rec,
                    at: 500,
                },
            },
            Record {
                stamp: clock.stamp(3_000),
                fact: Fact::RecordingAcquired {
                    recording: rec,
                    at: 7_000,
                },
            },
        ]
        .into();
        assert_eq!(
            fold(&records).acquired.get(&rec).copied(),
            Some(500),
            "the earliest, not the latest and not the last heard"
        );
    }

    /// `days_since_played` asks when the play happened, so the fold keeps the
    /// most recent one rather than the count alone.
    #[test]
    fn last_played_is_the_most_recent_play() {
        let rec = recording(1);
        let mut clock = HybridClock::new(device(1));
        // Stamped in milliseconds, which is the clock's unit.
        let records: BTreeSet<Record> = [5_000_000i64, 9_000_000, 7_000_000]
            .into_iter()
            .map(|wall| Record {
                stamp: clock.stamp(wall),
                fact: Fact::RecordingPlayed {
                    recording: rec,
                    encoding: None,
                },
            })
            .collect();
        let cat = fold(&records);
        assert_eq!(cat.plays.get(&rec).copied(), Some(3), "all three counted");
        // Read back in **seconds**, like every other date the player compares.
        // Handing out the stamp's milliseconds made "played a minute ago" read
        // as fifty years from now, and only running the thing showed it.
        assert_eq!(
            cat.last_played.get(&rec).copied(),
            Some(9_000),
            "the newest play, and in seconds"
        );
    }

    /// A recording nobody has played or acquired has neither, which is what
    /// makes `!play_count?` and `!days_since_added?` findable.
    #[test]
    fn a_silent_recording_has_no_history() {
        let cat = fold(&BTreeSet::new());
        assert!(cat.acquired.is_empty());
        assert!(cat.last_played.is_empty());
    }
}

#[cfg(test)]
mod merge_tests {
    //! **A merge moves what was said, or it is a delete with extra steps.**

    use super::*;
    use crate::model::basis::Basis;
    use crate::model::clock::HybridClock;

    fn rec(n: u128) -> Id<Recording> {
        Id::from_uuid(uuid::Uuid::from_u128(n))
    }

    fn log(facts: impl IntoIterator<Item = Fact>) -> BTreeSet<Record> {
        let mut clock = HybridClock::new(Id::derived(b"laptop"));
        facts
            .into_iter()
            .map(|fact| Record {
                stamp: clock.stamp(1_000),
                fact,
            })
            .collect()
    }

    /// **The case that actually happened**, in miniature: a rating and two
    /// plays given before anyone knew what the recording was.
    #[test]
    fn history_given_to_the_loser_lands_on_the_winner() {
        let (old, new) = (rec(1), rec(2));
        let cat = fold(&log([
            Fact::RecordingRated {
                recording: old,
                stars: Some(5),
            },
            Fact::RecordingPlayed {
                recording: old,
                encoding: Some(Id::derived(b"e")),
            },
            Fact::RecordingPlayed {
                recording: new,
                encoding: Some(Id::derived(b"e")),
            },
            Fact::RecordingMerged {
                loser: old,
                winner: new,
            },
        ]));

        assert_eq!(cat.ratings.get(&new), Some(&5), "the rating was stranded");
        assert_eq!(cat.plays.get(&new), Some(&2), "the plays did not add up");
        assert!(!cat.ratings.contains_key(&old) && !cat.plays.contains_key(&old));
    }

    /// **A merge recorded on Tuesday moves a rating given on Monday**, which
    /// is why merges are folded in a pass of their own. Applied as they are
    /// reached, everything earlier stays where it was.
    #[test]
    fn a_merge_reaches_backwards() {
        let (old, new) = (rec(1), rec(2));
        let cat = fold(&log([
            Fact::RecordingRated {
                recording: old,
                stars: Some(4),
            },
            Fact::RecordingMerged {
                loser: old,
                winner: new,
            },
        ]));
        assert_eq!(cat.ratings.get(&new), Some(&4));
    }

    /// The row goes too, or the library shows the song twice.
    #[test]
    fn a_merged_recording_keeps_no_row_of_its_own() {
        let (old, new) = (rec(1), rec(2));
        let cat = fold(&log([
            Fact::RecordingCatalogued {
                recording: old,
                title: "0011".into(),
                artist: Id::derived(b"brinkmann"),
            },
            Fact::RecordingIdentified {
                recording: new,
                basis: Basis::MusicBrainz(uuid::Uuid::from_u128(9)),
            },
            Fact::RecordingMerged {
                loser: old,
                winner: new,
            },
        ]));
        assert!(
            !cat.recordings.contains_key(&old),
            "the song is listed twice"
        );
        assert_eq!(cat.recordings[&new].title, "0011");
    }

    /// **A badly tagged copy does not re-place a well tagged one.**
    ///
    /// Identical audio is one encoding however many files hold it, and
    /// `EncodingPlaced` was last-writer-wins — so scanning the worse copy moved
    /// the better one. Measured: `Luna` was held under `~/MusicLibrary` with a
    /// MusicBrainz album id and under `~/DanceMusic-staging` with seven tags
    /// and none. Importing the staging folder moved all 32 encodings onto a
    /// medium minted from the album *name*, and the tracklist catalogued
    /// against the MusicBrainz medium then appeared to have attached to
    /// nothing. 39 files were sitting on the weaker medium.
    ///
    /// The rung is computable: a release equal to `of_release(None, group)`
    /// came from the group, anything else came from an id.
    #[test]
    fn a_placement_from_a_musicbrainz_release_outranks_one_from_a_name() {
        use crate::model::entity::{Artist, Medium, Release, ReleaseGroup, Track};
        let group = Id::<ReleaseGroup>::derived(b"luna");
        let named = Id::<Release>::of_release(None, group);
        let minted = Id::<Release>::of_musicbrainz(uuid::Uuid::from_u128(0xdc70));
        let good = Id::<Medium>::of_medium(minted, 1);
        let weak = Id::<Medium>::of_medium(named, 1);
        let song = Id::<Recording>::derived(b"triton");
        let copy = Id::<Encoding>::derived(b"one rip");
        let here = Id::<Track>::of_track(good, song);
        let there = Id::<Track>::of_track(weak, song);
        let disc = |release, medium| Fact::MediumCatalogued {
            medium,
            release,
            position: 1,
            format: None,
            tracks: Some(34),
        };
        let pressing = |release| Fact::ReleaseCatalogued {
            release,
            group,
            title: "Luna".into(),
            released: None,
            label: None,
            media: Some(1),
        };
        let slot = |track, medium| Fact::TrackPlaced {
            track,
            medium,
            recording: song,
            position: 1,
            title_on_release: None,
        };
        let cat = fold(&log([
            pressing(minted),
            pressing(named),
            disc(minted, good),
            disc(named, weak),
            slot(here, good),
            slot(there, weak),
            Fact::RecordingCatalogued {
                recording: song,
                title: "Triton".into(),
                artist: Id::<Artist>::derived(b"bodzin"),
            },
            // The well tagged copy first, then the badly tagged one — which is
            // the order that lost the good placement.
            Fact::EncodingPlaced {
                encoding: copy,
                track: here,
                basis: crate::model::reference::Source::TagVariant,
            },
            Fact::EncodingPlaced {
                encoding: copy,
                track: there,
                basis: crate::model::reference::Source::TagVariant,
            },
        ]));
        assert_eq!(
            cat.placed.get(&copy),
            Some(&here),
            "the later, worse-evidenced placement won"
        );
    }

    /// **And where nothing is better evidenced, the last word still wins.**
    /// The rung picks the best available; it does not freeze a placement.
    #[test]
    fn among_placements_of_one_rung_the_latest_stands() {
        use crate::model::entity::{Medium, Release, ReleaseGroup, Track};
        let group = Id::<ReleaseGroup>::derived(b"a record");
        let named = Id::<Release>::of_release(None, group);
        let one = Id::<Medium>::of_medium(named, 1);
        let two = Id::<Medium>::of_medium(named, 2);
        let song = Id::<Recording>::derived(b"a song");
        let copy = Id::<Encoding>::derived(b"a copy");
        let first = Id::<Track>::of_track(one, song);
        let last = Id::<Track>::of_track(two, song);
        let cat = fold(&log([
            Fact::ReleaseCatalogued {
                release: named,
                group,
                title: "A Record".into(),
                released: None,
                label: None,
                media: Some(2),
            },
            Fact::EncodingPlaced {
                encoding: copy,
                track: first,
                basis: crate::model::reference::Source::TagVariant,
            },
            Fact::EncodingPlaced {
                encoding: copy,
                track: last,
                basis: crate::model::reference::Source::TagVariant,
            },
        ]));
        assert_eq!(cat.placed.get(&copy), Some(&last));
    }

    /// **A retraction shadows a credit, and a later assertion lifts it.**
    ///
    /// The log is append-only and merges by union, so a credit is live or not
    /// according to whichever of assert and retract carries the later stamp —
    /// the same rule `ConceptRetracted` follows, and for the same reason: a
    /// stale replica re-delivering the assertion must not resurrect it.
    #[test]
    fn a_withdrawn_credit_stays_withdrawn_until_somebody_asserts_it_again() {
        use crate::model::entity::{Artist, ReleaseGroup, Role};
        let album = Id::<ReleaseGroup>::derived(b"luna");
        let joined = Id::<Artist>::derived(b"stephan bodzin & marc romboy");
        let performer = Id::<Role>::derived(b"performer");
        let on = CreditOn::ReleaseGroup(album);
        let assert = || Fact::CreditAsserted {
            subject: on,
            artist: joined,
            role: performer,
            basis: Source::TagVariant,
            position: 0,
            as_credited: None,
            join_phrase: None,
        };
        let withdraw = || Fact::CreditRetracted {
            subject: on,
            artist: joined,
            role: performer,
            basis: Source::Manual,
        };

        let live = fold(&log([assert()]));
        assert_eq!(live.credits.len(), 1);

        let gone = fold(&log([assert(), withdraw()]));
        assert!(
            gone.credits.is_empty(),
            "the credit survived its retraction"
        );
        assert_eq!(
            gone.credits_retracted.get(&(on, joined, performer)),
            Some(&Source::Manual),
            "who ruled it out was not recorded"
        );

        let again = fold(&log([assert(), withdraw(), assert()]));
        assert_eq!(again.credits.len(), 1, "a later assertion did not lift it");
        assert!(again.credits_retracted.is_empty());
    }

    /// **It shadows every basis at once**, being keyed one field shorter than
    /// the assertion. Somebody saying this artist is not on this record is
    /// answering the question, not arguing with one asserter.
    #[test]
    fn a_withdrawal_does_not_leave_the_same_credit_standing_on_another_rung() {
        use crate::model::entity::{Artist, ReleaseGroup, Role};
        let album = Id::<ReleaseGroup>::derived(b"luna");
        let joined = Id::<Artist>::derived(b"a joined credit");
        let performer = Id::<Role>::derived(b"performer");
        let on = CreditOn::ReleaseGroup(album);
        let assert = |basis| Fact::CreditAsserted {
            subject: on,
            artist: joined,
            role: performer,
            basis,
            position: 0,
            as_credited: None,
            join_phrase: None,
        };
        let cat = fold(&log([
            assert(Source::TagVariant),
            assert(Source::MusicBrainz),
            Fact::CreditRetracted {
                subject: on,
                artist: joined,
                role: performer,
                basis: Source::Manual,
            },
        ]));
        assert!(
            cat.credits.is_empty(),
            "a credit asserted twice needed retracting twice"
        );
    }

    /// **A retitle changes the label and nothing else** — the fact an
    /// identification could not write.
    ///
    /// The case that wanted it: two James Blake tracks in one folder with
    /// their tags transposed. Identifying them bound each encoding to the
    /// right performance and merged the byte-minted rows away — and left both
    /// titles wrong, because a merge carries the loser's description onto the
    /// winner and nothing wrote a new one.
    #[test]
    fn a_retitle_moves_the_label_and_leaves_the_credit() {
        use crate::model::entity::Artist;
        let song = Id::<Recording>::derived(b"give a man a rod");
        let blake = Id::<Artist>::derived(b"james blake");
        let cat = fold(&log([
            Fact::RecordingCatalogued {
                recording: song,
                title: "The Bells Sketch".into(),
                artist: blake,
            },
            Fact::RecordingRetitled {
                recording: song,
                title: "Give a Man a Rod".into(),
            },
        ]));
        assert_eq!(cat.recordings[&song].title, "Give a Man a Rod");
        assert_eq!(
            cat.recordings[&song].artist, blake,
            "a retitle is not a recredit"
        );
    }

    /// **An album can be told what it is called, and a merge is why.**
    ///
    /// A merge unites what both sides said and a title is descriptive, so the
    /// last writer takes it. Three editions of *21* are one release group
    /// holding three releases — the shape `Barton Hollow` settled (#134) — and
    /// merging them named the album `21 (Target Bonus Disc)`.
    #[test]
    fn an_album_can_be_retitled_and_the_retitle_follows_a_merge() {
        use crate::model::entity::{Artist, ReleaseGroup};
        let standard = Id::<ReleaseGroup>::derived(b"21");
        let target = Id::<ReleaseGroup>::derived(b"21 target bonus disc");
        let adele = Id::<Artist>::derived(b"adele");
        let catalogued = |group, title: &str| Fact::ReleaseGroupCatalogued {
            group,
            title: title.into(),
            artist: adele,
            first_released: None,
            kind: Default::default(),
            secondary: Default::default(),
        };
        let cat = fold(&log([
            catalogued(standard, "21"),
            catalogued(target, "21 (Target Bonus Disc)"),
            // The merge is right and hands the name to whichever was written
            // last, which is the whole problem.
            Fact::ReleaseGroupMerged {
                loser: target,
                winner: standard,
            },
            // Named against the loser, because that is the id a person reading
            // the merge has in front of them. Redirects resolve on read.
            Fact::ReleaseGroupRetitled {
                group: target,
                title: "21".into(),
            },
        ]));
        assert!(
            !cat.groups.contains_key(&target),
            "the album is listed twice"
        );
        assert_eq!(cat.groups[&standard].title, "21");
        assert_eq!(
            cat.groups[&standard].artist, adele,
            "a retitle is not a recredit"
        );
    }

    /// **And it outlasts a scan**, for the reason a recording's does: every
    /// read walks the disk and re-asserts `ReleaseGroupCatalogued` from the
    /// tags, so last-writer-wins would hand the name back to the tag forever.
    #[test]
    fn an_album_retitle_is_not_undone_by_the_next_scan() {
        use crate::model::entity::{Artist, ReleaseGroup};
        let album = Id::<ReleaseGroup>::derived(b"21");
        let catalogued = |title: &str| Fact::ReleaseGroupCatalogued {
            group: album,
            title: title.into(),
            artist: Id::<Artist>::derived(b"adele"),
            first_released: None,
            kind: Default::default(),
            secondary: Default::default(),
        };
        let mut records = log([
            catalogued("21 (Target Bonus Disc)"),
            Fact::ReleaseGroupRetitled {
                group: album,
                title: "21".into(),
            },
        ]);
        // The next read, saying what the tags still say — and newer.
        records.extend(log([catalogued("21 (Target Bonus Disc)")]));
        assert_eq!(
            fold(&records).groups[&album].title,
            "21",
            "a scan wrote over a title somebody curated"
        );
    }

    /// **A retitle outlasts a scan, whenever it was written.**
    ///
    /// The bug this exists for: 23 retitles applied cleanly and not one was
    /// visible afterwards. `query` and `apply` bring their view up to date by
    /// scanning the disk, and a scan re-asserts `RecordingCatalogued` from the
    /// tags with a *fresh* stamp — so last-writer-wins hands the tag the win
    /// on every read, forever.
    ///
    /// Note the stamps here: the scan's fact is newer, and the retitle still
    /// wins. That is the point.
    #[test]
    fn a_retitle_is_not_undone_by_the_next_scan() {
        use crate::model::entity::Artist;
        let song = Id::<Recording>::derived(b"give a man a rod");
        let blake = Id::<Artist>::derived(b"james blake");
        let mut records = log([
            Fact::RecordingCatalogued {
                recording: song,
                title: "The Bells Sketch".into(),
                artist: blake,
            },
            Fact::RecordingRetitled {
                recording: song,
                title: "Give a Man a Rod".into(),
            },
        ]);
        // The next read walks the disk and says what the tags still say.
        records.extend(log([Fact::RecordingCatalogued {
            recording: song,
            title: "The Bells Sketch".into(),
            artist: blake,
        }]));

        assert_eq!(
            fold(&records).recordings[&song].title,
            "Give a Man a Rod",
            "a scan wrote over a title somebody curated"
        );
    }

    /// **And it follows a merge**, which is the shape it actually arrives in:
    /// an identification merges the byte-minted row into the MusicBrainz one,
    /// and the retitle names whichever of the two the tool was looking at.
    #[test]
    fn a_retitle_reaches_the_row_a_merge_moved_it_to() {
        use crate::model::entity::Artist;
        let old = Id::<Recording>::derived(b"minted from bytes");
        let new = Id::<Recording>::derived(b"named by musicbrainz");
        let cat = fold(&log([
            Fact::RecordingCatalogued {
                recording: old,
                title: "The Bells Sketch".into(),
                artist: Id::<Artist>::derived(b"james blake"),
            },
            Fact::RecordingMerged {
                loser: old,
                winner: new,
            },
            Fact::RecordingRetitled {
                recording: old,
                title: "Give a Man a Rod".into(),
            },
        ]));
        assert!(!cat.recordings.contains_key(&old));
        assert_eq!(
            cat.recordings[&new].title, "Give a Man a Rod",
            "the retitle did not follow the merge"
        );
    }

    /// **A retitle of a row nothing catalogued does nothing.** It describes
    /// something held; `parse` refuses one whose subject is unknown, and this
    /// is the fold half of that — a stray fact must not mint a row with a
    /// title and no artist.
    #[test]
    fn a_retitle_does_not_invent_a_recording() {
        let nobody = Id::<Recording>::derived(b"never catalogued");
        let cat = fold(&log([Fact::RecordingRetitled {
            recording: nobody,
            title: "Out of nowhere".into(),
        }]));
        assert!(cat.recordings.is_empty(), "a title minted a row");
    }

    /// **An artist merge collapses a split credit into one row.**
    ///
    /// The measured case: of 1015 named artists this library holds, 82 names
    /// are held by more than one id, because `of_artist` mints from a
    /// MusicBrainz id where the tags carry one and from the name otherwise.
    /// `line_of` dedupes by rendered name, so both halves drew as one artist
    /// and the division was invisible — but the credits really were two rows,
    /// and a browse by artist really did split the work.
    #[test]
    fn a_merged_artist_takes_the_credits_given_to_both() {
        use crate::model::entity::Artist;
        let named = Id::<Artist>::derived(b"name\x1fumfang");
        let minted = Id::<Artist>::derived(b"musicbrainz\x1fumfang");
        let one = Id::<Recording>::derived(b"shant");
        let two = Id::<Recording>::derived(b"six");
        let performer = Id::<crate::model::entity::Role>::derived(b"performer");
        let credit = |recording, artist| Fact::CreditAsserted {
            subject: CreditOn::Recording(recording),
            artist,
            role: performer,
            basis: Source::TagVariant,
            position: 0,
            as_credited: None,
            join_phrase: None,
        };
        let cat = fold(&log([
            Fact::ArtistNamed {
                artist: named,
                name: "UMFANG".into(),
            },
            Fact::ArtistNamed {
                artist: minted,
                name: "Umfang".into(),
            },
            credit(one, named),
            credit(two, minted),
            Fact::ArtistMerged {
                loser: named,
                winner: minted,
            },
        ]));

        assert!(
            !cat.artist_names.contains_key(&named),
            "the artist is still listed twice"
        );
        assert_eq!(
            cat.credits.len(),
            2,
            "both credits survive — a merge joins the artist, not the work"
        );
        assert!(
            cat.credits.keys().all(|(_, artist, ..)| *artist == minted),
            "a credit did not follow the artist it was given to"
        );
        // Last writer wins, as it does for every descriptive fact: the merge
        // moves the loser's label onto the winner and the later one stands.
        assert_eq!(cat.artist_names[&minted], "Umfang");
    }

    /// **And it moves the album credit**, which is the one that reaches a
    /// folder: `file_under` asks the release group for its artist first, so a
    /// split left here files the two halves of one record apart. Three of the
    /// 82 splits differ in case as well — `Umfang` against `UMFANG` — and
    /// those are the ones that showed, as a folder named `Umfang, UMFANG`.
    #[test]
    fn a_merged_artist_takes_the_album_with_them() {
        use crate::model::entity::{Artist, ReleaseGroup};
        let named = Id::<Artist>::derived(b"name\x1fantigone");
        let minted = Id::<Artist>::derived(b"musicbrainz\x1fantigone");
        let group = Id::<ReleaseGroup>::derived(b"as i walk to you");
        let cat = fold(&log([
            Fact::ReleaseGroupCatalogued {
                group,
                title: "As I Walk to You".into(),
                artist: named,
                first_released: None,
                kind: Default::default(),
                secondary: Default::default(),
            },
            Fact::ArtistMerged {
                loser: named,
                winner: minted,
            },
        ]));
        assert_eq!(cat.groups[&group].artist, minted);
    }

    /// **An album merge moves the pressings and keeps them apart** — #156.
    ///
    /// The case that wanted this: *Barton Hollow* is a standard edition of 14
    /// tracks and a deluxe with 4 more, and a retag made them two albums. The
    /// answer is one album holding two releases — so the merge has to move the
    /// pressing's `group` and must not touch the pressing itself.
    #[test]
    fn a_merged_album_keeps_its_pressings_and_loses_its_row() {
        use crate::model::entity::{Release, ReleaseGroup};
        let old = Id::<ReleaseGroup>::derived(b"barton hollow, deluxe");
        let new = Id::<ReleaseGroup>::derived(b"barton hollow");
        let standard = Id::<Release>::derived(b"standard");
        let deluxe = Id::<Release>::derived(b"deluxe");
        let cat = fold(&log([
            Fact::ReleaseGroupCatalogued {
                group: new,
                title: "Barton Hollow".into(),
                artist: Id::derived(b"the civil wars"),
                first_released: None,
                kind: Default::default(),
                secondary: Default::default(),
            },
            Fact::ReleaseGroupCatalogued {
                group: old,
                title: "Barton Hollow".into(),
                artist: Id::derived(b"the civil wars"),
                first_released: None,
                kind: Default::default(),
                secondary: Default::default(),
            },
            Fact::ReleaseCatalogued {
                release: standard,
                group: new,
                title: "Barton Hollow".into(),
                released: None,
                label: None,
                media: Some(1),
            },
            Fact::ReleaseCatalogued {
                release: deluxe,
                group: old,
                title: "Barton Hollow".into(),
                released: None,
                label: None,
                media: Some(1),
            },
            Fact::ReleaseGroupMerged {
                loser: old,
                winner: new,
            },
        ]));

        assert!(!cat.groups.contains_key(&old), "the album is listed twice");
        assert_eq!(cat.releases.len(), 2, "**both pressings survive**");
        assert!(
            cat.releases.values().all(|r| r.group == new),
            "a pressing did not follow its album"
        );
    }

    /// A credit and a genre asserted on the loser are about the album, so they
    /// come too. Otherwise a merge files the two halves of one record under
    /// different artists.
    #[test]
    fn what_was_said_about_the_loser_is_said_about_the_winner() {
        use crate::model::entity::ReleaseGroup;
        use crate::model::facet::Described;
        use crate::model::reference::{CreditOn, RoleClass, Source};
        let old = Id::<ReleaseGroup>::derived(b"loser");
        let new = Id::<ReleaseGroup>::derived(b"winner");
        let who = Id::derived(b"the civil wars");
        let role = Id::<crate::model::entity::Role>::of_name("performer");
        let cat = fold(&log([
            Fact::RoleDefined {
                role,
                name: "performer".into(),
                class: RoleClass::Performer,
            },
            Fact::CreditAsserted {
                subject: CreditOn::ReleaseGroup(old),
                artist: who,
                role,
                basis: Source::TagVariant,
                position: 0,
                as_credited: None,
                join_phrase: None,
            },
            Fact::ReleaseGroupMerged {
                loser: old,
                winner: new,
            },
        ]));
        assert!(
            cat.credits
                .keys()
                .all(|(subject, ..)| *subject == CreditOn::ReleaseGroup(new)),
            "the album credit stayed on the row nothing points at"
        );
        let _ = Described::ReleaseGroup(new);
    }

    /// **A retired copy stops being one, and nothing else changes** — #159.
    ///
    /// The tombstone `storage-layout.md` §6 specified and nothing wrote. What
    /// it has to do is narrow: the library stops offering the *copy*, and the
    /// recording, its slot and everything said about it stay exactly where
    /// they are — because a row the library holds no copy of is ordinary, and
    /// since #155 it is a row on purpose.
    #[test]
    fn a_retired_copy_is_shadowed_and_its_row_is_not() {
        use crate::model::encoding::Format;
        let enc = Id::<crate::model::entity::Encoding>::derived(b"a copy");
        let audio = crate::audio_id::AudioId { scheme: 1, hash: 7 };
        let described = Fact::EncodingDescribed {
            encoding: enc,
            audio,
            format: Some(Format::Mp3),
            bitrate: 320,
            length_ms: 200_000,
            sample_rate: Some(44_100),
            channels: Some(2),
        };
        let cat = fold(&log([
            described.clone(),
            Fact::EncodingRetired { encoding: enc },
        ]));
        assert!(cat.retired.contains(&enc));
        assert!(
            cat.encodings.contains_key(&enc),
            "**nothing is destroyed** — the row is what makes this reversible"
        );

        // And back, because a step you cannot take back is one nobody takes.
        let cat = fold(&log([
            described,
            Fact::EncodingRetired { encoding: enc },
            Fact::EncodingRestored { encoding: enc },
        ]));
        assert!(
            !cat.retired.contains(&enc),
            "a restored copy is still shadowed"
        );
    }

    /// Nothing is destroyed. The loser's facts are all still in the log, which
    /// is what makes a merge undoable and a false one repairable.
    #[test]
    fn a_merge_destroys_nothing() {
        let records = log([
            Fact::RecordingRated {
                recording: rec(1),
                stars: Some(5),
            },
            Fact::RecordingMerged {
                loser: rec(1),
                winner: rec(2),
            },
        ]);
        assert!(records.iter().any(|r| matches!(
            &r.fact,
            Fact::RecordingRated { recording, .. } if *recording == rec(1)
        )));
    }
}
