//! The seam — issue #77.
//!
//! **The library answers questions and accepts changes; the player asks and
//! plays.** This module is where that sentence becomes a type.
//!
//! Today `src/ui/` reaches into a `Scan` and handles `PathBuf`s directly, so
//! the player and the librarian are one program by accident rather than by
//! design. [`Librarian`] is the surface that ends that: everything the player
//! needs, expressed in the model's terms, and nothing that requires it to know
//! where a file sits or what a tag key is called.
//!
//! ## Two directions, and only one of them holds yet
//!
//! *The model must not know about the player* holds today and is asserted, so
//! it cannot quietly stop holding. Nothing under `src/model/` names `ui`,
//! `audio`, `cmd`, `ratatui` or `crossterm`; its only reaches outside are
//! `audio_id`, which is identity rather than playback, and `lofty`, which is
//! the tag boundary.
//!
//! *The player must not know about the store* does **not** hold yet, and
//! pretending otherwise would be worse than saying so. [`tests::the_leak`]
//! counts what is left and refuses to let it grow, which turns a 10,000-line
//! migration into something with a number on it that can only go down.

use crate::model::encoding::{Encoding, Encodings, Format, Verdict};
use crate::model::entity::{Collection, Device, Recording, ReleaseGroup, Track};
use crate::model::id::Id;
use crate::model::ingest::Confidence;
use std::collections::{BTreeMap, BTreeSet};

use crate::model::facet::Described;
use crate::model::log::{Catalogue, Collected};
use crate::model::ord::OrdKey;
use crate::model::provenance::Provenance;
use crate::model::reference::{CreditOn, Reference};
use crate::model::rows::Drawn;
use crate::model::spine::Spine;
use crate::model::vocabulary::Vocabulary;

/// What the player is allowed to ask, and what it is allowed to ask for.
///
/// Deliberately small. The four things `src/ui/` asks a `Scan` for today are
/// *get*, *iterate*, *upsert* and *look up by path* — and the last of those is
/// the one that must not survive the move, because a path is a fact about one
/// device's copy and the player has no business holding one.
pub struct Librarian {
    pub spine: Spine,
    pub reference: Reference,
    /// The genre codelist and its hierarchy.
    ///
    /// **This carried an `#[expect(dead_code)]` until #116**, and the
    /// attribute retired itself the moment `depth` read the edges — which is
    /// what `expect` is for and why #113 chose it over `allow`.
    pub vocabulary: Vocabulary,
    pub encodings: Encodings,
    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — held so the librarian owns the whole fold; nothing reads it back through the seam"
    )]
    pub provenance: Provenance,
    /// Folded from the log, never stored as numbers. #67's rule decides a
    /// rating; a play is counted.
    pub ratings: std::collections::BTreeMap<Id<Recording>, u8>,
    pub plays: std::collections::BTreeMap<Id<Recording>, u32>,
    /// Every genre assigned to each subject, by name.
    ///
    /// Folded down from `Catalogue::genres`, which keys by
    /// `(subject, concept, source)`: several sources may assert the same
    /// concept, and each carries its own confidence. Collapsed to the distinct
    /// labels here, most confident first, so a display with room for one shows
    /// the best-supported and a filter can ask about any of them.
    ///
    /// **Keyed by axis too** (#117), so `genre` and `version` share one map
    /// and a third axis needs no fourth field.
    pub concepts:
        std::collections::BTreeMap<(Described, Id<crate::model::entity::Facet>), Vec<String>>,
    /// Every row this library can draw, materialised once (#86).
    ///
    /// `Listed` borrows and the librarian computes — `file_under` from the
    /// album-level credit, `album` from the slot — so there is nothing to lend
    /// a view of unless it is kept. Built at the end of the fold and lent from
    /// there.
    rows: std::collections::BTreeMap<Id<Encoding>, Drawn>,
    /// Where this device can open each row, from the holdings it asserted.
    ///
    /// Empty until [`Librarian::here`] says which device this is. The
    /// librarian knows every holding; which of them is reachable *now* is a
    /// fact about the machine it is running on.
    locators: std::collections::BTreeMap<Id<Encoding>, String>,
    /// And the inverse, for reading a playlist back.
    ///
    /// **Many paths, one row.** Two files with identical bytes are one
    /// encoding, so both locators name it — which is the right answer for an
    /// `.m3u` that happens to list the copy you deleted.
    held_at: std::collections::BTreeMap<String, Id<Encoding>>,
    /// What each collection is called.
    pub collections: BTreeMap<Id<Collection>, Collected>,
    /// Every collection entry, in list order — see [`Catalogue::entries`].
    collected: BTreeSet<(Id<Collection>, OrdKey, Id<Recording>)>,
    /// Which slot each copy sits in — issue #78.
    ///
    /// The field that makes a row's album a fact rather than a choice. Without
    /// it, `album_of` asked a *recording* which album it was on, and a
    /// recording is on every release that holds it.
    pub placed: std::collections::BTreeMap<Id<Encoding>, Id<Track>>,
}

impl Default for Librarian {
    fn default() -> Self {
        Self::new()
    }
}

/// How the two questions drawing a row asks get answered — issue #123.
///
/// **The same answer, found two ways, because the workloads are opposite.**
/// `Reference::credits_on` and `Encodings::of` each walk their whole
/// collection. For one row that is right: a scan of 13,000 credits beats
/// building a map of them to ask once. `draw` asks about *every* row, and
/// there the scan was four passes over the credits and one over the encodings
/// per row — 333 ms of a 397 ms fold.
///
/// So the choice is made at the call site and named, rather than one strategy
/// being imposed on both. What is *not* duplicated is the answer: `line_of`
/// remains the only place that decides how a credit line reads.
enum Lookup<'a> {
    /// Ask the collections directly. For a caller drawing one row.
    Direct(&'a Librarian),
    /// Built once for a pass over the library.
    Built {
        l: &'a Librarian,
        credits: BTreeMap<CreditOn, Vec<&'a crate::model::reference::Credit>>,
        encodings: BTreeMap<Id<Recording>, Vec<&'a crate::model::encoding::Encoding>>,
    },
}

impl<'a> Lookup<'a> {
    /// The index `draw` uses, built once.
    fn built(l: &'a Librarian) -> Self {
        Lookup::Built {
            l,
            credits: l.reference.by_subject(),
            encodings: l.encodings.by_recording(),
        }
    }

    /// The credit line for a subject, as printed.
    ///
    /// Both arms end at `line_of`, which stays the only place that decides how
    /// a credit line reads. What differs is only how the credits were found.
    fn line(&self, subject: CreditOn) -> String {
        match self {
            Lookup::Direct(l) => l.reference.credited_as(subject),
            Lookup::Built { l, credits, .. } => credits
                .get(&subject)
                .map(|found| l.reference.line_of(found))
                .unwrap_or_default(),
        }
    }

    /// Whether the library holds any encoding of this recording.
    fn holds(&self, recording: Id<Recording>) -> bool {
        match self {
            Lookup::Direct(l) => !l.encodings.of(recording).is_empty(),
            Lookup::Built { encodings, .. } => encodings.contains_key(&recording),
        }
    }
}

impl Librarian {
    pub fn new() -> Self {
        Librarian {
            spine: Spine::default(),
            reference: Reference::default(),
            vocabulary: Vocabulary::default(),
            encodings: Encodings::default(),
            provenance: Provenance::default(),
            ratings: Default::default(),
            plays: Default::default(),
            concepts: Default::default(),
            rows: Default::default(),
            locators: Default::default(),
            held_at: Default::default(),
            collections: Default::default(),
            collected: Default::default(),
            placed: Default::default(),
        }
    }

    /// What a row on screen needs to draw itself.
    ///
    /// Assembled **by the library** out of five entities the player never has
    /// to know are separate — and carrying no path, because there is no path
    /// that is true of the library rather than of one machine.
    pub fn row(&self, recording: Id<Recording>) -> Option<Row> {
        let canonical = self.encodings.canonical(recording).map(|e| e.id);
        self.row_of(recording, canonical)
    }

    /// The row for **one copy** — issue #78, and the question it settles.
    ///
    /// Measured on the real library: asked of a recording, 163 files showed an
    /// album that was not the one their own tags name. Asked of the copy, 15
    /// do, and every one of those is a title the model has settled the case of
    /// — `Strange Time To Be Alive` against `Strange Time to Be Alive` — which
    /// is one album having one name rather than a placement being wrong.
    ///
    /// A recording is on every release that holds it; a file is on one. Where
    /// the row is about a file, say which, and the album stops being a choice.
    /// `row` is this with the canonical copy filled in, which makes the
    /// answer a *rule* rather than whichever placement sorted first.
    pub fn row_of(&self, recording: Id<Recording>, copy: Option<Id<Encoding>>) -> Option<Row> {
        self.row_of_via(recording, copy, &Lookup::Direct(self))
    }

    /// [`Self::row_of`], against a credit index the caller already built.
    fn row_of_via(
        &self,
        recording: Id<Recording>,
        copy: Option<Id<Encoding>>,
        by: &Lookup<'_>,
    ) -> Option<Row> {
        let rec = self.spine.recordings.get(&recording)?;
        let canonical = copy.and_then(|id| self.encodings.encodings.get(&id));
        let slot = copy.and_then(|id| self.placed.get(&id).copied());
        Some(Row {
            recording,
            title: rec.title.clone(),
            file_under: self.files_under(recording, slot, by),
            album: self.album_of(slot),
            // **The length of the copy this would open**, not of the
            // recording — a recording has no single length. An edit, a radio
            // cut and a truncated rip are one recording and three durations,
            // and "did I hear enough of it" is a question about the file that
            // played. Ranking them by length was a rule invented to fill a
            // field that should not have been filled: under it, a twelve-second
            // rip measured against a four-minute sibling could never earn a
            // play at all.
            length: canonical.map(|e| e.length).unwrap_or_default(),
            rating: self.ratings.get(&recording).copied(),
            plays: self.plays.get(&recording).copied().unwrap_or(0),
            format: canonical.map(|e| e.format),
            bitrate: canonical.map(|e| e.bitrate),
            verdict: canonical.map(|e| e.verdict()),
            available: by.holds(recording),
        })
    }

    /// Who a copy files under.
    ///
    /// The album-level credit first, then the recording's own — which is the
    /// modelled version of `Track::file_under`'s "album artist, else artist".
    /// The heuristic was guessing at a fact; this asks for it.
    ///
    /// **Asked of a slot, not of a recording.** It used to take the first of
    /// `tracks_of_recording`, and a recording is on every release that holds
    /// it — so a song on a compilation and its original album filed under
    /// whichever sorted first. On this library that split `Luna` across two
    /// artists with every tag on the album identical (#84).
    fn files_under(
        &self,
        recording: Id<Recording>,
        slot: Option<Id<Track>>,
        by: &Lookup<'_>,
    ) -> String {
        let on_release = slot
            .and_then(|s| self.spine.tracks.get(&s))
            .and_then(|t| self.spine.media.get(&t.medium.id()))
            .and_then(|m| self.spine.releases.get(&m.release.id()))
            .map(|r| CreditOn::ReleaseGroup(r.group.id()));

        let line = |subject: CreditOn| by.line(subject);
        on_release
            .map(line)
            .filter(|s| !s.is_empty())
            .or_else(|| {
                let by_recording = line(CreditOn::Recording(recording));
                (!by_recording.is_empty()).then_some(by_recording)
            })
            .unwrap_or_else(|| UNKNOWN_ARTIST.to_string())
    }

    /// The album a **copy** files under, by way of the release its slot is on.
    ///
    /// See [`Librarian::files_under`] for why this is asked of a slot.
    fn album_of(&self, slot: Option<Id<Track>>) -> Option<String> {
        let track = self.spine.tracks.get(&slot?)?;
        let medium = self.spine.media.get(&track.medium.id())?;
        let release = self.spine.releases.get(&medium.release.id())?;
        self.spine
            .groups
            .get(&release.group.id())
            .map(|g| g.title.clone())
    }

    /// Which copy this machine should open, and where it sits on it.
    ///
    /// **The one place a locator is produced**, and it is produced at the
    /// moment of playing rather than stored anywhere the player can keep. A
    /// player that cannot hold a path cannot key anything by one.
    pub fn open(
        &self,
        recording: Id<Recording>,
        here: &[Id<Device>],
        prefers: &[Format],
    ) -> Option<(&Encoding, &str)> {
        let pinned = self
            .spine
            .recordings
            .get(&recording)
            .and_then(|r| r.pinned)
            .map(|p| p.id());
        let chosen = self
            .encodings
            .for_playback(recording, pinned, here, prefers)?;
        let locator = self
            .encodings
            .holders(chosen.id)
            .into_iter()
            .find(|h| here.contains(&h.device.id()))
            .map(|h| h.locator.as_str())?;
        Some((chosen, locator))
    }

    /// Build the library a log describes.
    ///
    /// The link that makes the whole layer end-to-end: files become a scan
    /// (#70), a scan becomes records (#69), records fold to a catalogue, and a
    /// catalogue becomes this. Nothing in the chain is a stored projection, so
    /// there is nothing that can drift out of step with the record of how it
    /// got there.
    ///
    /// Deliberately total. A encoding bound to a recording that no record
    /// catalogued still lands — that is the unidentified 7.3%, and it must not
    /// be second class.
    pub fn from_catalogue(cat: &Catalogue) -> Self {
        use crate::model::encoding::Encoding;
        use crate::model::spine::Recording as Rec;

        let mut l = Librarian::new();

        // How long each recording plays, taken from an encoding of it.
        //
        for (id, r) in &cat.recordings {
            l.spine.recordings.insert(
                *id,
                Rec {
                    id: *id,
                    title: r.title.clone(),
                    // A fold, not a field. A medley performs several works and
                    // the spine holds one, so an ambiguous answer is no answer
                    // — the second row is still in the catalogue for anything
                    // that can read a set.
                    work: match cat
                        .performances
                        .iter()
                        .filter(|(rec, _)| rec == id)
                        .collect::<Vec<_>>()
                        .as_slice()
                    {
                        [(_, w)] => Some(crate::model::id::Ref::to(*w)),
                        _ => None,
                    },
                    recorded: None,
                    bpm: None,
                    // Not filled: a recording has no length of its own.
                    // Its encodings do, and `Row` takes the one it would play.
                    length: std::time::Duration::ZERO,
                    pinned: None,
                },
            );
            l.reference.artists.entry(r.artist).or_insert_with(|| {
                crate::model::reference::ArtistRecord {
                    id: r.artist,
                    name: cat.artist_names.get(&r.artist).cloned().unwrap_or_default(),
                    sort_name: cat.artist_names.get(&r.artist).cloned().unwrap_or_default(),
                }
            });
        }
        // The role codelist and the credits, read.
        //
        // Both used to be made up here: one role minted from `b"performer"`,
        // and a credit pushed for every recording naming it. Every artist in
        // the library was a performer because one line said so, and a row that
        // could only say one thing said nothing.
        for (id, (name, class)) in &cat.roles {
            l.reference.roles.insert(
                *id,
                crate::model::reference::RoleRecord {
                    id: *id,
                    name: name.clone(),
                    class: *class,
                },
            );
        }
        for ((subject, artist, role, _), c) in &cat.credits {
            l.reference.credits.push(crate::model::reference::Credit {
                subject: *subject,
                artist: crate::model::id::Ref::to(*artist),
                role: crate::model::id::Ref::to(*role),
                position: c.position,
                as_credited: c.as_credited.clone(),
                join_phrase: c.join_phrase.clone(),
            });
        }
        for (id, name) in &cat.label_names {
            l.reference.labels.insert(
                *id,
                crate::model::reference::LabelRecord {
                    id: *id,
                    name: name.clone(),
                },
            );
        }
        for (id, title) in &cat.works {
            l.spine.works.insert(
                *id,
                crate::model::spine::Work {
                    id: *id,
                    title: title.clone(),
                },
            );
        }

        // The album spine.
        for (group, g) in &cat.groups {
            l.spine.groups.insert(
                *group,
                crate::model::spine::ReleaseGroup {
                    id: *group,
                    title: g.title.clone(),
                    first_released: g.first_released,
                    kind: crate::model::spine::ReleaseType::Album,
                },
            );
            l.reference.artists.entry(g.artist).or_insert_with(|| {
                crate::model::reference::ArtistRecord {
                    id: g.artist,
                    name: cat.artist_names.get(&g.artist).cloned().unwrap_or_default(),
                    sort_name: cat.artist_names.get(&g.artist).cloned().unwrap_or_default(),
                }
            });
        }

        // Read, not invented. Each of these three used to be constructed here
        // from whatever was to hand — with no stamp, no author and no way to
        // correct one — and looked exactly like a folded fact downstream.
        for (id, r) in &cat.releases {
            l.spine.releases.insert(
                *id,
                crate::model::spine::Release {
                    id: *id,
                    group: crate::model::id::Ref::to(r.group),
                    title: r.title.clone(),
                    released: r.released,
                    label: r.label.map(crate::model::id::Ref::to),
                    catalogue_number: None,
                },
            );
        }
        for (id, m) in &cat.media {
            l.spine.media.insert(
                *id,
                crate::model::spine::Medium {
                    id: *id,
                    release: crate::model::id::Ref::to(m.release),
                    position: m.position,
                    format: m.format.clone(),
                },
            );
        }
        for (id, p) in &cat.tracks {
            l.spine.tracks.insert(
                *id,
                crate::model::spine::Track {
                    id: *id,
                    medium: crate::model::id::Ref::to(p.medium),
                    recording: crate::model::id::Ref::to(p.recording),
                    position: p.position,
                    title_on_release: p.title_on_release.clone(),
                },
            );
        }

        l.placed = cat.placed.clone();

        // Names are a register: whatever the fold settled on is the label.
        for (id, name) in &cat.artist_names {
            if let Some(a) = l.reference.artists.get_mut(id) {
                a.name = name.clone();
                a.sort_name = name.clone();
            }
        }
        for (id, d) in &cat.encodings {
            l.encodings.encodings.insert(
                *id,
                Encoding {
                    id: *id,
                    audio: d.audio,
                    recording: d.recording.map(crate::model::id::Ref::to),
                    format: d.format.unwrap_or(Format::Mp3),
                    bitrate: d.bitrate,
                    sample_rate: d.sample_rate,
                    channels: d.channels,
                    length: std::time::Duration::from_millis(u64::from(d.length_ms)),
                    cutoff_hz: cat.measurements.get(id).map(|(hz, analyser)| {
                        crate::model::encoding::Measured {
                            value: *hz,
                            analyser: *analyser,
                        }
                    }),
                    gain: None,
                    levels: cat.levels.get(id).copied().unwrap_or_default(),
                    encoder_delay: None,
                },
            );
        }
        for (device, asserted) in &cat.holdings {
            for (dist, locator) in &asserted.held {
                l.encodings.holdings.push(crate::model::encoding::Holding {
                    encoding: crate::model::id::Ref::to(*dist),
                    device: crate::model::id::Ref::to(*device),
                    locator: locator.clone(),
                });
            }
        }
        l.ratings = cat.ratings.clone();
        l.plays = cat.plays.clone();
        l.collections = cat.collections.clone();
        l.collected = cat.entries.clone();

        // **The hierarchy, which nothing carried across before** (#116). The
        // fold has held these edges since #74 and the librarian left them
        // behind, so `depth` had nothing to walk and the genres on a row could
        // only be ordered by name.
        for (hierarchy, (_, name)) in &cat.hierarchies {
            l.vocabulary.hierarchies.insert(
                *hierarchy,
                crate::model::vocabulary::HierarchyRecord {
                    id: *hierarchy,
                    name: name.clone(),
                },
            );
        }
        // **And the terms, which it left behind the same way** (#79). Edges
        // without labels let `depth` walk but gave `at_level` an ancestor it
        // could only name by falling back to the concept it started from — so
        // `levels genre@1` silently drew the specific term as the broad one.
        for (id, record) in &cat.concepts {
            l.vocabulary.genres.insert(
                *id,
                crate::model::vocabulary::Term {
                    id: *id,
                    name: record.label.clone(),
                },
            );
        }
        for (hierarchy, parent, child) in &cat.narrows {
            l.vocabulary
                .edges
                .insert(crate::model::vocabulary::HierarchyEdge {
                    hierarchy: crate::model::id::Ref::to(*hierarchy),
                    parent: crate::model::id::Ref::to(*parent),
                    child: crate::model::id::Ref::to(*child),
                });
        }

        // Every genre a subject is assigned, most confident first. Several
        // sources may assign the same one — a tag and a curated pass agreeing
        // is two rows in the fold and one genre on the shelf — so the best
        // confidence for each name wins and the name is listed once.
        // **Keyed by axis, not one map per axis** (#117), so an axis added to
        // `facet::ALL` crosses the seam with nothing written here. A node
        // label is left out: ISO 25964's rule is that a term put into an array
        // to organise it does not take part in retrieval.
        let mut depths: BTreeMap<Id<crate::model::entity::Concept>, usize> = BTreeMap::new();
        let mut best: BTreeMap<(Described, Id<crate::model::entity::Facet>), Ranked<'_>> =
            BTreeMap::new();
        for ((subject, concept, _), confidence) in &cat.assertions {
            if cat.assertions_retracted.contains_key(&(*subject, *concept)) {
                continue;
            }
            let Some(record) = cat.concepts.get(concept).filter(|c| !c.node_label) else {
                continue;
            };
            let name = &record.label;
            // **Memoised, because a vocabulary is small and the assertions
            // are not** (#123). `depth` walks the edge set once per hop, and
            // this library asks it 26,490 times about roughly 250 distinct
            // concepts.
            let depth = *depths
                .entry(*concept)
                .or_insert_with(|| l.vocabulary.depth(*concept));
            let seen = best.entry((*subject, record.facet)).or_default();
            match seen.iter_mut().find(|(_, _, held)| *held == name.as_str()) {
                Some(entry) if entry.0 < *confidence => entry.0 = *confidence,
                Some(_) => {}
                None => seen.push((*confidence, depth, name.as_str())),
            }
        }
        l.concepts = best
            .into_iter()
            .map(|(subject, mut names)| {
                // Most confident, then **broadest**, then by name.
                //
                // **Tag order does not survive the fold and is not meant to.**
                // `Rock; Blues Rock; Alternative Rock` arrives broadest-first
                // because that is how taggers write it, but the catalogue keys
                // assignments by `(subject, genre, source)` — a set, with no
                // position, because a position is a fact about a *string* and
                // not about the genres.
                //
                // The hierarchy is what orders them: `Rock` is a top concept
                // and `Alternative Rock` is narrower-of it, so depth answers
                // "which is broadest" from the data rather than from the
                // spelling. Where no edge places a genre it is depth 0 and the
                // name breaks the tie, which is the alphabetical order this
                // had before any hierarchy existed (#116).
                names.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(b.2)));
                (
                    subject,
                    names.into_iter().map(|(_, _, n)| n.to_string()).collect(),
                )
            })
            .collect();

        l.draw();
        l
    }

    /// Say which device this is, so rows know where they can be opened.
    pub fn here(&mut self, device: Id<Device>) {
        let mine = || {
            self.encodings
                .holdings
                .iter()
                .filter(|h| h.device.id() == device)
        };
        self.locators = mine()
            .map(|h| (h.encoding.id(), h.locator.clone()))
            .collect();
        self.held_at = mine()
            .map(|h| (h.locator.clone(), h.encoding.id()))
            .collect();
    }

    /// Materialise every row.
    ///
    /// One pass at the end of the fold rather than one assembly per draw. The
    /// UI redraws five times a second and a screenful is fifty rows; building
    /// them on demand would recompute the same credits and album titles
    /// hundreds of times a second for a library that changed between none of
    /// them.
    fn draw(&mut self) {
        // **Built once for the whole library** (#123). `credits_on` is a
        // linear scan, and asking it per row was 6,500 full passes over 13,000
        // credits — 333ms of a 397ms fold.
        let by = Lookup::built(self);
        let ids: Vec<Id<Encoding>> = self.encodings.encodings.keys().copied().collect();
        let drawn: Vec<(Id<Encoding>, Drawn)> = ids
            .into_iter()
            .filter_map(|id| Some((id, self.drawn_of(id, &by)?)))
            .collect();
        self.rows.extend(drawn);
    }

    /// Assemble one row out of the entities behind it.
    fn drawn_of(&self, copy: Id<Encoding>, by: &Lookup<'_>) -> Option<Drawn> {
        let encoding = self.encodings.encodings.get(&copy)?;
        let recording = encoding.recording?.id();
        let row = self.row_of_via(recording, Some(copy), by)?;
        let slot = self.placed.get(&copy).copied();
        let (disc, track) = self.place_of(slot);
        let group = self.group_of(slot);

        // **Every concept on the recording**, per axis, and the album's only
        // where the recording carries none of its own — a compilation's `Pop`
        // must not join what the track itself was asserted (#74). The rule is
        // per axis rather than per row, so an album-level genre and a
        // track-level version do not shadow one another.
        let mut concepts: std::collections::BTreeMap<_, Vec<String>> =
            std::collections::BTreeMap::new();
        for facet in crate::model::facet::ALL
            .iter()
            .map(|(key, _)| Id::<crate::model::entity::Facet>::of_key(key))
        {
            let mut held = self
                .concepts
                .get(&(Described::Recording(recording), facet))
                .cloned()
                .unwrap_or_default();
            if held.is_empty() {
                held = group
                    .and_then(|g| self.concepts.get(&(Described::ReleaseGroup(g), facet)))
                    .cloned()
                    .unwrap_or_default();
            }
            if !held.is_empty() {
                concepts.insert(facet, held);
            }
        }

        let on_release = slot
            .and_then(|s| self.spine.tracks.get(&s))
            .and_then(|t| self.spine.media.get(&t.medium.id()))
            .and_then(|m| self.spine.releases.get(&m.release.id()));

        // **Through the same lookup as everything else here** (#123). These
        // two were the rest of the per-row scan: `files_under` above and these
        // below are four passes over every credit in the library, per row.
        let line = |subject: CreditOn| by.line(subject);

        Some(Drawn {
            file_under: row.file_under,
            album: row
                .album
                .clone()
                .unwrap_or_else(|| crate::model::tree::NO_ALBUM.to_string()),
            // `u16::MAX` is `place_of`'s "not numbered" — it sorts unplaced
            // copies last rather than first, which is why it is not zero. Read
            // as a number it shows a track 65535.
            disc: (disc != u16::MAX).then_some(u32::from(disc)),
            track: (track != u16::MAX).then_some(u32::from(track)),
            title: row.title.clone(),
            duration: row.length,
            artist: {
                let credited = line(CreditOn::Recording(recording));
                (!credited.is_empty()).then_some(credited)
            },
            album_artist: group.map(|g| line(CreditOn::ReleaseGroup(g))),
            // The model's title *is* the tagged one, normalised. There is no
            // file name to fall back to here, which is the point: a row holds
            // no location, so it cannot borrow one for a label.
            title_if_named: (!row.title.is_empty()).then(|| row.title.clone()),
            album_if_named: row.album,
            concepts,
            label: on_release
                .and_then(|r| r.label)
                .and_then(|l| self.reference.labels.get(&l.id()))
                .map(|l| l.name.clone()),
            // The album's first release where the tags say so, else this
            // pressing's. `OriginalReleaseDate` is the #59-correct answer — a
            // 2015 pressing of a 1982 album is 1982 — and 98% of this library
            // does not carry it, so falling back is the difference between a
            // year and a blank.
            year: group
                .and_then(|g| self.spine.groups.get(&g))
                .and_then(|g| g.first_released)
                .or_else(|| on_release.and_then(|r| r.released))
                .map(|d| i32::from(d.year)),
            bitrate: Some(encoding.bitrate),
            sample_rate: encoding.sample_rate,
            channels: encoding.channels,
        })
    }

    /// The album a recording sits on, if any.
    /// Asked of a slot, for the reason [`Librarian::files_under`] is: a
    /// recording is on every release that holds it.
    pub fn group_of(&self, slot: Option<Id<Track>>) -> Option<Id<ReleaseGroup>> {
        let track = self.spine.tracks.get(&slot?)?;
        let medium = self.spine.media.get(&track.medium.id())?;
        let release = self.spine.releases.get(&medium.release.id())?;
        Some(release.group.id())
    }

    /// Where a recording sits: disc, then track, as the tags claim.
    ///
    /// Sorts unplaced recordings last rather than first, because a track with
    /// no number is a gap in what is known, and putting it at the top of an
    /// album would state something the tags never did.
    pub fn place_of(&self, slot: Option<Id<Track>>) -> (u16, u16) {
        slot.and_then(|s| self.spine.tracks.get(&s))
            .and_then(|t| {
                self.spine
                    .media
                    .get(&t.medium.id())
                    .map(|m| (m.position, t.position))
            })
            .map(|(d, tr)| (d, if tr == 0 { u16::MAX } else { tr }))
            .unwrap_or((u16::MAX, u16::MAX))
    }

    /// Everything playable, in a stable order.
    pub fn recordings(&self) -> impl Iterator<Item = Id<Recording>> + '_ {
        self.spine.recordings.keys().copied()
    }

    /// Which slot a copy sits in.
    pub fn slot_of(&self, copy: Option<Id<Encoding>>) -> Option<Id<Track>> {
        self.placed.get(&copy?).copied()
    }

    // **`listings` lived here and is gone** (#121).
    //
    // It answered "every copy of this recording, as rows would list them" and
    // had one caller: the tree, which asked it per recording and allocated a
    // `Vec` for each — the whole library's worth, before any of them were
    // tested for membership. That was a ~100ms floor no amount of narrowing
    // could get under. The tree walks the members now, and a member *is* a
    // copy, so the listing is built rather than looked up.
    //
    // What went with it is the entry for a recording nothing holds — a
    // wishlist row, which §1 is explicit is ordinary. Nothing lost it here:
    // `Shelf::grouped` already dropped any listing with no copy, so the tree
    // has never drawn one. When the shelf learns to, it needs a set that
    // names recordings rather than copies, not this back.
}

/// **Reading a collection back** — #92.
///
/// Its own block because all of it is ahead of its callers: a playlist is
/// still a file on disk until #92's UI half, and these are what that will
/// read. `#[allow]` rather than `#[expect]` because tests exercise them, so
/// the expectation would report itself unfulfilled under `cargo test`.
#[allow(
    dead_code,
    reason = "the model is tested ahead of its callers; a playlist is still a file on disk until #92's UI half"
)]
impl Librarian {
    /// What a collection holds, in order.
    ///
    /// **A sequence, so duplicates come back as duplicates** (#92). The same
    /// recording twice in one playlist is two entries at two positions, and a
    /// `Vec` is the only shape that can say so — a set could not, which is
    /// exactly why a view is not only a predicate.
    ///
    /// Already in order: the fold keys entries by position before recording,
    /// so walking them is walking the list.
    pub fn entries_of(&self, collection: Id<Collection>) -> Vec<Id<Recording>> {
        <Self as crate::model::rows::Listing>::entries_of(self, collection)
    }

    fn collection_entries(&self, collection: Id<Collection>) -> Vec<Id<Recording>> {
        self.collected
            .iter()
            .filter(|(c, _, _)| *c == collection)
            .map(|(_, _, recording)| *recording)
            .collect()
    }

    /// The position of each entry, for a caller inserting between two of them.
    ///
    /// Separate from [`Librarian::entries_of`] because they are asked at
    /// different moments: the recordings are what a list shows, and the keys
    /// are what an edit needs — and an edit needs the two it is landing
    /// between rather than all of them.
    pub fn positions_of(&self, collection: Id<Collection>) -> Vec<OrdKey> {
        self.collected
            .iter()
            .filter(|(c, _, _)| *c == collection)
            .map(|(_, ord, _)| ord.clone())
            .collect()
    }

    /// Every collection there is, by id.
    pub fn collections(&self) -> &BTreeMap<Id<Collection>, Collected> {
        &self.collections
    }
}

/// **The librarian answers the row contract** — #86, and the point of it.
///
/// `Scan` implemented this and the player read rows through the store. Now the
/// library draws them, which is what lets the store go: the trait is the seam,
/// and swapping which side of it answers is the whole migration.
impl crate::model::rows::Listing for Librarian {
    fn entries_of(&self, collection: Id<Collection>) -> Vec<Id<Recording>> {
        self.collection_entries(collection)
    }

    /// Whether any narrower-of edge places a concept on this axis.
    ///
    /// Asked of the child, because an edge is only useful for climbing if the
    /// thing being climbed from is on the axis in question — a version
    /// hierarchy must not make `levels genre@1` claim it has somewhere to go
    /// (#117 keeps the axes apart, and this is where that matters).
    fn places(&self, facet: Id<crate::model::entity::Facet>) -> bool {
        // **The id says which axis it is on**, because it was minted from the
        // facet key and the label (#117). So re-deriving from the term's name
        // and comparing is the membership test, and no second map is needed to
        // answer it.
        let on_axis = |id: Id<crate::model::entity::Concept>| {
            self.vocabulary.genres.get(&id).is_some_and(|term| {
                crate::model::facet::ALL.iter().any(|(key, _)| {
                    Id::<crate::model::entity::Facet>::of_key(key) == facet
                        && Id::<crate::model::entity::Concept>::in_facet(key, &term.name) == id
                })
            })
        };
        self.vocabulary.edges.iter().any(|e| on_axis(e.child.id()))
    }

    fn listed(&self, id: Id<Encoding>) -> Option<crate::model::rows::Listed<'_>> {
        Some(self.rows.get(&id)?.listed())
    }

    fn rows(&self) -> Vec<Id<Encoding>> {
        self.rows.keys().copied().collect()
    }

    /// **The binding the library holds**, which is what a relink changes.
    fn recording_of(&self, id: Id<Encoding>) -> Option<Id<Recording>> {
        Some(self.encodings.encodings.get(&id)?.recording?.id())
    }

    fn playable(&self, id: Id<Encoding>) -> Option<crate::model::rows::Playable<'_>> {
        let levels = &self.encodings.encodings.get(&id)?.levels;
        Some(crate::model::rows::Playable {
            // Only a holding on *this* device. A row the library knows about
            // and this machine does not hold is an ordinary row and an
            // unplayable one, which is a different answer from "no such row".
            locator: self.locators.get(&id)?,
            gain: (!levels.is_empty()).then(|| crate::model::rows::Loudness {
                track_db: levels.track_db(),
                track_peak: levels.track_peak(),
                album_db: levels.album_db(),
                album_peak: levels.album_peak(),
            }),
        })
    }
}

/// The claims about one subject on one axis, before they are collapsed to
/// labels: how confident, how deep in the hierarchy, and what it is called.
type Ranked<'a> = Vec<(Confidence, usize, &'a str)>;

/// A recording, and which copy of it a row is about.
///
/// The pair `row_of` takes, named so a browse tree can carry it. `copy` is
/// `None` for a recording nothing here holds — an ordinary row, not a missing
/// one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Listing {
    pub recording: Id<Recording>,
    pub copy: Option<Id<Encoding>>,
}

/// One displayed row.
///
/// Holds identities and values, **never a location**. That omission is the
/// point: a `PathBuf` on a row is what let `history.toml` be keyed by path,
/// which cost three stranded entries and a four-star rating on a file that
/// still existed.
///
/// Every field here is something `src/ui/` reads off a `Track` today. Two of
/// them dissolve rather than move:
///
/// - **`title` never needs a file name.** `Track::display_title` falls back to
///   the path's stem, but ingest already did that (#70), so by the time a
///   recording exists its title is a title.
/// - **`file_under` stops being a heuristic.** "Album artist, else artist" is
///   guessing at a fact the model states: ask the release group's credit, then
///   the recording's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub recording: Id<Recording>,
    pub title: String,
    /// Who this files under — the album-level credit where there is one.
    pub file_under: String,
    pub album: Option<String>,
    pub length: std::time::Duration,
    pub rating: Option<u8>,
    /// Counted from the log, never a stored number.
    pub plays: u32,
    pub format: Option<Format>,
    pub bitrate: Option<u32>,
    /// What the canonical encoding measures as, so a re-buy list is a filter
    /// rather than a separate pass.
    pub verdict: Option<Verdict>,
    pub available: bool,
}

/// What is shown when nothing names the artist.
pub const UNKNOWN_ARTIST: &str = "Unknown Artist";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::facet;
    use std::path::Path;

    fn read(dir: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut stack = vec![Path::new(dir).to_path_buf()];
        while let Some(p) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&p) else {
                continue;
            };
            for e in entries.flatten() {
                let path = e.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|x| x == "rs")
                    && let Ok(text) = std::fs::read_to_string(&path)
                {
                    out.push((path.display().to_string(), text));
                }
            }
        }
        out
    }

    /// **The direction that holds, asserted so it keeps holding.**
    ///
    /// The librarian must not know there is a player. Nothing here reaches for
    /// a terminal, an audio device, or a key binding — and the day something
    /// does, this fails rather than the seam quietly closing up.
    #[test]
    fn the_model_does_not_know_the_player_exists() {
        let forbidden = [
            "crate::ui",
            "crate::cmd",
            "crate::audio::",
            "crate::store",
            "crate::library",
            "crate::session",
            "crate::next",
            "crate::playlist",
            "ratatui",
            "crossterm",
            "cpal",
        ];
        // Only `use` lines: "depends on" means importing, and a test that
        // scanned raw text would trip over this very list.
        let mut leaks = Vec::new();
        for (file, text) in read("src/model") {
            for line in text.lines().map(str::trim) {
                if !line.starts_with("use ") {
                    continue;
                }
                for f in forbidden {
                    if line.contains(f) {
                        leaks.push(format!("{file}: {line}"));
                    }
                }
            }
        }
        assert!(
            leaks.is_empty(),
            "the model reached into the player: {leaks:#?}"
        );
    }

    /// **The direction that does not hold yet**, with a number on it.
    ///
    /// `src/ui/` still names a `Scan` and still handles `PathBuf`s. Asserting
    /// zero today would just fail; asserting *no worse than today* turns a
    /// 10,000-line migration into a ratchet, and makes any new leak a test
    /// failure rather than something to be found later.
    ///
    /// **Lower these as the migration lands. Never raise them** — and where
    /// one stops falling, say why rather than leaving it at a number.
    ///
    /// Two of the three have now been revised rather than driven to zero,
    /// because zero was never the right target for them. `PathBuf` counted
    /// four legitimate uses and could not fall further; it is a question now
    /// instead of a count. `Scan` cannot reach zero while the type exists,
    /// which is #86. Only `reaches` still measures a migration in progress,
    /// and its own floor is four.
    ///
    /// **It counts the substring, so prose counted too.** Two `Scan`
    /// mentions were the word *Stored* in doc comments and one was a comment
    /// naming `Scan::recording_of` — three of five, in a number somebody was
    /// going to chase to zero. Reworded rather than subtracted, so the count
    /// means what it says. What is left is the import, the `Context` field,
    /// and one comment about what playlists hold.
    ///
    /// **Test code is not counted**, and that is a correction rather than a
    /// loophole. Counting it measured the wrong thing twice over: most of what
    /// it found was fixtures building a library to test against, and the first
    /// test written to *prove* the seam had moved — that a rename can no
    /// longer strand a rating — failed the ratchet for naming the store it
    /// renamed a file in. A fixture is not a leak. What leaks is the player.
    #[test]
    fn the_leak_from_the_player_only_narrows() {
        /// **Zero** (#86). The player does not name the store at all.
        ///
        /// The last two were the import and the `Context` field, and they went
        /// together because they were the same thing: `Context.store` was what
        /// the import was for. Every read that went through it now goes to the
        /// librarian, which answers `rows::Listing` — so swapping which side
        /// of the seam answered was the whole of it.
        ///
        /// Two helpers dissolved on the way. `recording_of` *is*
        /// `Listing::recording_of`, and `holdings_here` is `rows()` folded by
        /// it. `audio_of` left the trait entirely: it existed so a caller
        /// could take one hop through each arena, and there is one arena.
        ///
        /// Like `REACHES`, this is a floor and not a ratchet. What is left to
        /// catch is a mention appearing.
        const STORE: usize = 0;
        /// **What a `PathBuf` in the player is allowed to be.**
        ///
        /// Counting them was the wrong measure and it had reached its floor
        /// without anyone noticing. All four that remain are legitimate, and
        /// two are not track paths at all:
        ///
        /// - `session_path` and `data_dir` are the **player's own files**.
        ///   Nothing about "the library must not be keyed by a path" says the
        ///   program may not know where it keeps its own state.
        /// - `PathBuf::from(locator)` is the sanctioned production point —
        ///   §"the locator is produced here and used immediately", and the
        ///   whole discipline is that it is not *kept*.
        /// - the `use` itself.
        ///
        /// So the count could only ever fall to four, and a number with a
        /// floor nobody had written down is a number somebody will chase.
        /// Replaced by the question it was standing in for: **is the player
        /// holding a path that is neither its own file nor one it is opening
        /// right now?** That fails on a new one and stays silent on these,
        /// which is what the ratchet was for.
        const SANCTIONED_PATHS: &[&str] = &[
            "use std::path",
            "session_path",
            "data_dir",
            "PathBuf::from(locator)",
        ];
        /// **The number that actually measures the seam.** The player reaches
        /// through `ctx.store` far more often than it names the type, so
        /// counting `Scan` alone said 5 while sixteen reads went around the
        /// librarian. Replacing a read lowers this and leaves the others flat,
        /// which is exactly the progress #77 is made of.
        ///
        /// One went when playback started opening through the librarian
        /// (#85): the locator and the recording behind it are asked of the
        /// library now, and what remains of the store on that path is reached
        /// through `rows::Listing` rather than by name.
        ///
        /// Three more went with #77's first pass — a rating's recording, the
        /// underrun label, and seeking by percentage. **What the last eleven
        /// are waiting on is not effort but three other decisions**, and they
        /// are listed here so the next person does not re-derive them:
        ///
        /// The five that rendered through `format::Template` are gone: it
        /// takes a `rows::Listed` now, which answers fourteen of the sixteen
        /// fields it used to read off a `Track`. The two it does not are
        /// `path` and `file_name`, and a template asking for either is refused
        /// when it is parsed — a row carries no location (#78).
        /// The session's two went with #112 — it names a recording and the
        /// copy that was open, so resuming reopens the file that was open
        /// rather than whichever the canonical rule would pick now.
        ///
        /// **Zero** (#86), and this is no longer a ratchet but a floor.
        ///
        /// The last two went together. The remote's snapshot is a
        /// `rows::Drawn` — an owned row, which is the one thing a borrow could
        /// not do and the whole reason it reached for the record itself. And
        /// reading an `.m3u` asks `Listing::at`, the inverse of `playable`:
        /// the one question that has to start from a path, because an `.m3u`
        /// is a path format. What the playlist holds afterwards is row ids.
        ///
        /// Before them, #86's second pass took the filter and the sort, and
        /// its first pass took the two `.m3u` writes.
        ///
        /// It cannot fall further, so a rise is the only thing left to catch —
        /// which is what this now does.
        const REACHES: usize = 0;

        let (mut store, mut reaches) = (0, 0);
        let mut stray_paths: Vec<String> = Vec::new();
        for (name, text) in read("src/ui") {
            let live = code_only(production(&text));
            store += identifiers(&live, "Scan");
            reaches += live.matches("ctx.store.").count();
            for line in live.lines().filter(|l| l.contains("PathBuf")) {
                if !SANCTIONED_PATHS.iter().any(|ok| line.contains(ok)) {
                    stray_paths.push(format!("{name}: {}", line.trim()));
                }
            }
        }

        assert!(
            stray_paths.is_empty(),
            "the player is holding a path that is not one of its own files, \
             and not one produced at the moment of opening: {stray_paths:#?}"
        );
        // **An assertion, not a ratchet.** It reached zero, and a number that
        // cannot fall further has nothing left to ratchet — what is worth
        // catching is a new one appearing.
        assert_eq!(
            reaches, REACHES,
            "the player reached past the seam again: {reaches} time(s)"
        );

        assert_eq!(
            store, STORE,
            "the player named the store again: {store} time(s)"
        );
    }

    /// **An assignment is identified by what it says, never by an id.**
    ///
    /// `entity.rs` lists `GenreAssignment` as a kind, and it stays one so
    /// anything derived from an assignment can be seeded by `KIND`. But
    /// minting `Id<GenreAssignment>` would put a *third* identity in the model
    /// — beside the subject's and the concept's — and it could only be one of
    /// two things. Random breaks convergent minting outright: two devices
    /// importing the same Discogs page have to emit the same row, and random
    /// ids guarantee they cannot. Content-derived buys nothing, because the
    /// content is already the key.
    ///
    /// The cost is not theoretical. An assignment with its own id is two rows
    /// that must cross the airgap together, and a fold that has to join across
    /// fact families before it can decide whether a claim is live.
    ///
    /// Asserted rather than remembered, because the reflex when a relationship
    /// grows a field is to reify it into a node, and here the field belongs on
    /// the row.
    #[test]
    fn no_assignment_carries_an_id() {
        let minted: Vec<_> = read("src")
            .into_iter()
            .filter(|(_, text)| {
                let live = production(text);
                live.contains("Id<GenreAssignment>") || live.contains("Id<QualifierAssignment>")
            })
            .map(|(path, _)| path)
            .collect();
        assert!(
            minted.is_empty(),
            "an assignment grew an identity of its own: {minted:#?}"
        );
    }

    /// **A group with no pressing fact yields no pressing.**
    ///
    /// `from_catalogue` used to mint a `Release` for every group and a
    /// `Medium` for every distinct disc number it saw, so those rows existed
    /// whether or not anyone had said so — with no stamp, no author and no way
    /// to correct one, and indistinguishable downstream from a folded fact.
    ///
    /// Asserted from the empty side because that is the side that cannot lie:
    /// a projection that invents will produce a row here, and one that reads
    /// cannot.
    #[test]
    fn the_projection_reads_the_spine_and_does_not_invent_it() {
        use crate::model::log::{Catalogue, Grouped};

        let mut cat = Catalogue::default();
        let group = Id::<crate::model::entity::ReleaseGroup>::derived(b"luna");
        cat.groups.insert(
            group,
            Grouped {
                title: "Luna".into(),
                artist: Id::derived(b"bodzin"),
                first_released: None,
            },
        );

        // And a recording, so the credit side has something to invent *for*.
        let rec = Id::<Recording>::derived(b"callisto");
        cat.recordings.insert(
            rec,
            crate::model::log::Recorded {
                title: "Callisto".into(),
                artist: Id::derived(b"bodzin"),
            },
        );

        let l = Librarian::from_catalogue(&cat);
        assert_eq!(l.spine.groups.len(), 1, "the album was read");
        assert_eq!(l.spine.recordings.len(), 1, "the recording was read");
        assert!(l.spine.releases.is_empty(), "a pressing nobody asserted");
        assert!(l.spine.media.is_empty(), "a disc nobody asserted");
        assert!(l.spine.tracks.is_empty(), "a slot nobody asserted");
        assert!(l.spine.works.is_empty(), "a composition nobody asserted");
        assert!(
            l.reference.credits.is_empty(),
            "every artist was a performer because one line said so"
        );
        assert!(
            l.reference.roles.is_empty(),
            "a codelist entry nobody defined"
        );
    }

    /// **No tool authors a fact that looks reviewed.**
    ///
    /// Two routes write to the library and they are not equals. A person
    /// acting on one thing in the UI appends directly, with `Source::Manual`
    /// and `Confidence::Confirmed`, because the person *is* the review. A
    /// tool — a matcher, an import, any pass in `tools/` — writes a proposal
    /// file, and `apply` turns it into records only after somebody has read
    /// it.
    ///
    /// Volume is the whole reason. A human mis-tags one recording; a matching
    /// pass mis-tags six hundred in a minute, and the three of 607 that this
    /// library's identification pass got wrong were all found by reading the
    /// file. A tool that could write `Manual` would be laundering its guesses
    /// into the one basis nothing downstream second-guesses.
    ///
    /// Checked at the boundary rather than in the type system: `Source` has to
    /// stay one enum so that provenance is one mechanism, so what stops the
    /// wrong variant is this test.
    #[test]
    fn no_tool_facing_surface_claims_a_person_said_so() {
        // `apply.rs` is the crossing point and is allowed to name it: a
        // proposal file a person edited by hand may say `manual`, which is
        // that person asserting, not the tool.
        let culprits: Vec<_> = read("src")
            .into_iter()
            .filter(|(path, _)| {
                path.contains("ingest.rs") || path.contains("scan.rs") || path.contains("tools")
            })
            .filter(|(_, text)| production(text).contains("Source::Manual"))
            .map(|(path, _)| path)
            .collect();
        assert!(
            culprits.is_empty(),
            "a tool wrote the basis reserved for a person: {culprits:#?}"
        );
    }

    /// **Every fact names its subject.**
    ///
    /// `[Subject][PastTenseVerb]` is the event-sourcing convention, and four
    /// variants used to break it — `Measured`, `Rated`, `Played`, `Acquired`.
    /// Each dropped the one word that mattered: `Measured` measured *what*? An
    /// encoding, not a recording, which is the distinction this whole model
    /// exists to draw.
    ///
    /// Asserted rather than remembered, because the next variant will be
    /// written by someone reaching for the shortest name that compiles.
    #[test]
    fn every_fact_names_its_subject() {
        const SUBJECTS: &[&str] = &[
            // Not one of the eight kinds an alias can hang off, and that is
            // the point: like `Holdings`, the subject is the thing being
            // recorded rather than the entity it attaches to. `ArtistAliased`
            // would need seven siblings saying nothing the `Cataloguable`
            // does not already say.
            "Alias",
            "Artist",
            "Collection",
            "Credit",
            "Device",
            "Encoding",
            "Concept",
            "Facet",
            "Hierarchy",
            "Holdings",
            "Label",
            "Medium",
            "Release",
            "ReleaseGroup",
            "Role",
            "Recording",
            "Track",
            "View",
            "Work",
        ];

        let (_, text) = read("src/model")
            .into_iter()
            .find(|(f, _)| f.ends_with("log.rs"))
            .expect("log.rs");
        let start = text.find("pub enum Fact {").expect("the Fact enum");
        let body = &text[start..text[start..].find("\n}\n").unwrap() + start];

        let mut nameless = Vec::new();
        for line in body.lines() {
            let Some(name) = line.strip_prefix("    ").and_then(|l| l.split(' ').next()) else {
                continue;
            };
            if !name.starts_with(char::is_uppercase) || !line.ends_with('{') {
                continue;
            }
            if !SUBJECTS.iter().any(|s| name.starts_with(s)) {
                nameless.push(name.to_string());
            }
        }
        assert!(
            nameless.is_empty(),
            "these facts do not say what they are about: {nameless:?}"
        );
    }

    /// Everything before the file's test module.
    ///
    /// Crude on purpose: this codebase puts `#[cfg(test)]` last and once per
    /// file, so a split on the first occurrence is exact here and obvious when
    /// it stops being. A parser would be more correct and less readable, and
    /// what is being measured is a trend rather than a total.
    fn production(text: &str) -> &str {
        match text.find("#[cfg(test)]") {
            Some(at) => &text[..at],
            None => text,
        }
    }

    /// Production code with its comments removed.
    ///
    /// **The ratchet used to count prose (#113).** It matched substrings over
    /// the whole file, so a doc comment explaining what the player must stop
    /// doing counted as the player doing it — and the closer the explanation
    /// got to naming the thing, the worse the number looked. Two of the
    /// `Store` mentions and one `PathBuf` were sentences.
    fn code_only(text: &str) -> String {
        text.lines()
            .map(|line| match line.find("//") {
                // Not inside a string literal: `"//"` is a path separator, not
                // a comment, and cutting there would silently drop real code.
                Some(at) if line[..at].matches('"').count() % 2 == 0 => &line[..at],
                _ => line,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// How many times `name` appears as an identifier rather than as a
    /// fragment of a longer one.
    ///
    /// `Scan` is inside `Scanned` and `Store` inside `Stored`, and counting
    /// the substring meant the number could not reach zero however much moved
    /// (#113).
    fn identifiers(text: &str, name: &str) -> usize {
        let bytes = text.as_bytes();
        let part = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
        text.match_indices(name)
            .filter(|(at, _)| {
                let before = *at == 0 || !part(bytes[at - 1]);
                let after = at + name.len() >= bytes.len() || !part(bytes[at + name.len()]);
                before && after
            })
            .count()
    }

    /// **The ratchet's own arithmetic, which the library does not exercise.**
    ///
    /// Both refinements survive mutation against `src/ui` — degrade
    /// `identifiers` to a substring match, or let `code_only` cut inside a
    /// string literal, and the count does not move, because no file happens
    /// to contain the case. A guard the corpus does not reach is a guard
    /// nobody is testing, so it is tested here instead.
    #[test]
    fn the_ratchet_counts_identifiers_and_not_prose() {
        // The reason the count could not reach zero: `Scan` is inside
        // `Scanned`, and `Store` was inside `Stored`.
        assert_eq!(
            identifiers("Scanned rescan SCAN", "Scan"),
            0,
            "a longer name that starts with it"
        );
        assert_eq!(
            identifiers("PreScan pre_Scan", "Scan"),
            0,
            "and one that ends with it"
        );
        assert_eq!(identifiers("let s: Scan = Scan::new();", "Scan"), 2);
        assert_eq!(
            identifiers("model::Scan,", "Scan"),
            1,
            "punctuation is a boundary"
        );
        assert_eq!(
            identifiers("Scan", "Scan"),
            1,
            "and so is the end of the text"
        );

        // A doc comment explaining what the player must stop doing is not the
        // player doing it.
        assert_eq!(
            identifiers(&code_only("// a comment about Scan"), "Scan"),
            0
        );
        assert_eq!(identifiers(&code_only("/// Scan, in prose"), "Scan"), 0);
        assert_eq!(
            identifiers(&code_only("let x: Scan = y; // and Scan again"), "Scan"),
            1
        );

        // And `//` inside a string is a path separator. Cutting there would
        // drop real code and quietly lower the number.
        assert_eq!(
            identifiers(
                &code_only(r#"let u = "http://x"; let s: Scan = y;"#),
                "Scan"
            ),
            1
        );
    }

    /// **The hierarchy orders a row's genres, broadest first** (#116).
    ///
    /// A row carries every genre assigned to it and a line has one slot, so
    /// which comes first is the whole question. Tag order does not survive the
    /// fold — the catalogue keys assignments by `(subject, genre, source)`, a
    /// set with no position — and alphabetical was the stand-in, which put
    /// `Abstract` where the tag had put `Rock`.
    ///
    /// This is what makes running `gmuse-tools genre apply` worth something:
    /// the live log contains no hierarchy at all, so every genre is depth 0
    /// today and the order falls back to the name.
    #[test]
    fn a_rows_genres_are_ordered_broadest_first() {
        use crate::model::entity::Hierarchy;
        use crate::model::instance::{Instance, Scanned};
        use crate::model::rows::Listing;

        let mut i = Instance::new(Id::<Device>::derived(b"laptop"));
        i.ingest(
            100,
            &[Scanned {
                audio: crate::audio_id::AudioId { scheme: 1, hash: 9 },
                title: "Racing to a Red Light".into(),
                artist: "Early James".into(),
                album: Some("Strange Time".into()),
                // Deliberately in an order that is neither the tags' usual one
                // nor alphabetical, so neither can pass by accident.
                genres: vec![
                    "Blues Rock".into(),
                    "Alternative Rock".into(),
                    "Rock".into(),
                ],
                ..Default::default()
            }],
        );

        // `Rock` is the top concept, `Alternative Rock` is narrower-of it, and
        // `Blues Rock` narrower again — so depth does more here than "is it a
        // top concept", and alphabetical would give the opposite order.
        let hierarchy = Id::<Hierarchy>::derived(b"discogs");
        i.narrows(200, hierarchy, facet::GENRE, "Rock", "Alternative Rock");
        i.narrows(
            200,
            hierarchy,
            facet::GENRE,
            "Alternative Rock",
            "Blues Rock",
        );

        let l = Librarian::from_catalogue(&i.catalogue());
        let copy = *l.encodings.encodings.keys().next().expect("a copy");
        let shown = l.listed(copy).expect("a row").genre().to_vec();

        assert_eq!(
            shown,
            ["Rock", "Alternative Rock", "Blues Rock"],
            "the genres were not ordered by how broad they are"
        );
    }

    /// **A file that names no album has no position and no date.**
    ///
    /// A track number is a position *on a medium of a release* and a date is a
    /// fact about a pressing, so a file whose tags name neither gets neither.
    /// A flat `Track` read both straight off the tag; the containment says
    /// otherwise, and "track 3 of nothing" is not a fact (#116).
    ///
    /// Pinned rather than left implicit because it is invisible when it bites:
    /// a loose single would show a blank where a year used to be, and nobody
    /// would know to look. All 6,500 files in the real library carry an album
    /// tag, so nothing is affected today — which is exactly why a test has to
    /// be the thing that notices if that stops being true.
    #[test]
    fn a_file_with_no_album_has_no_track_number_or_year() {
        use crate::model::instance::{Instance, Scanned};
        use crate::model::rows::Listing;

        let loose = Scanned {
            audio: crate::audio_id::AudioId { scheme: 1, hash: 5 },
            title: "A Loose Single".into(),
            artist: "Somebody".into(),
            // The tags say both, and there is nowhere for either to live.
            track: Some(3),
            released: Some(crate::model::spine::PartialDate::year(2024)),
            album: None,
            ..Default::default()
        };
        let mut i = Instance::new(Id::<Device>::derived(b"laptop"));
        i.ingest(100, std::slice::from_ref(&loose));
        let l = Librarian::from_catalogue(&i.catalogue());

        let copy = *l.encodings.encodings.keys().next().expect("one copy");
        let row = l.listed(copy).expect("a row");
        assert_eq!(row.title, "A Loose Single", "it is still a row");
        assert_eq!(row.track, None, "a position needs something to be on");
        assert_eq!(row.disc, None);
        assert_eq!(row.year, None, "a date needs an edition to be about");
    }

    /// **A tag's genres become facts, and a drawn row carries all of them.**
    ///
    /// The librarian showed no genre at all for 6,437 of 6,485 real rows,
    /// because a scan observed none: genres in this model are concepts with a
    /// basis and a confidence, and a tag had never been read as a claim. It is
    /// one now, at `TagVariant` — the weakest rung, which a curated pass
    /// outranks.
    #[test]
    fn the_genres_a_tag_names_reach_the_row() {
        use crate::model::entity::Device;
        use crate::model::instance::Instance;
        use crate::model::rows::Listing;

        let dir = std::env::temp_dir().join("gmuse-seam-genres");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("01 Racing.wav");
        crate::model::round_trip::write_wav(&file, 4410);
        crate::model::round_trip::write_genre(&file, "Rock; Blues Rock; Alternative Rock");

        let scan = vec![crate::model::ingest::read(&file).expect("reads")];
        assert_eq!(scan[0].genres.len(), 3, "the scan saw three terms");

        let device = Id::<Device>::derived(b"laptop");
        let mut m = Instance::new(device);
        m.ingest(100, &scan);
        let l = Librarian::from_catalogue(&m.catalogue());

        let copy = *l.encodings.encodings.keys().next().unwrap();
        let row = l.listed(copy).expect("a row");
        let mut shown: Vec<&str> = row.genre().iter().map(String::as_str).collect();
        shown.sort_unstable();
        assert_eq!(
            shown,
            ["Alternative Rock", "Blues Rock", "Rock"],
            "every term, not the first and not the joined string"
        );
    }

    /// A row is assembled by the library and carries no location, so the
    /// player cannot key anything by one.
    #[test]
    fn a_row_is_made_of_identities_and_values() {
        use crate::model::spine::Recording as Rec;
        use std::time::Duration;

        let mut l = Librarian::new();
        let id = Id::<Recording>::derived(b"callisto");
        l.spine.recordings.insert(
            id,
            Rec {
                id,
                title: "Callisto".into(),
                work: None,
                recorded: None,
                bpm: None,
                length: Duration::from_secs(400),
                pinned: None,
            },
        );

        let row = l.row(id).expect("a catalogued recording has a row");
        assert_eq!(row.title, "Callisto");
        assert!(!row.available, "no encoding, and still an ordinary row");
        assert_eq!(row.format, None);
    }

    /// **End to end.** Real files on disk, through ingest, through a log, to
    /// rows on a screen — with two machines and a sync in the middle, because
    /// that is the shape this whole layer exists to support.
    #[test]
    fn files_become_rows_by_way_of_a_log() {
        use crate::model::entity::Device;
        use crate::model::instance::Instance;

        let dir = std::env::temp_dir().join("gmuse-seam-end-to-end");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let file = dir.join("03 Callisto.wav");
        crate::model::round_trip::write_wav(&file, 4410);
        crate::model::round_trip::write_tags(
            &file,
            "Callisto",
            "Stephan Bodzin & Marc Romboy",
            Some("183105b5-0000-4000-8000-000000000000"),
        );

        let scan = vec![crate::model::ingest::read(&file).expect("reads")];
        let (mut m1, mut m2) = (
            Instance::new(Id::<Device>::derived(b"laptop")),
            Instance::new(Id::<Device>::derived(b"nas")),
        );
        m1.ingest(100, &scan);
        m2.ingest(140, &scan);

        let rec = *m1.catalogue().recordings.keys().next().unwrap();
        m1.rate(200, rec, Some(4));
        m2.play(240, rec, *m2.catalogue().encodings.keys().next().unwrap());

        let from_m2 = m2.records().clone();
        m1.merge(300, &from_m2);

        let l = Librarian::from_catalogue(&m1.catalogue());
        let row = l.row(rec).expect("a row");

        assert_eq!(row.title, "Callisto");
        assert_eq!(row.file_under, "Stephan Bodzin & Marc Romboy");
        assert_eq!(row.rating, Some(4), "set on the laptop");
        assert_eq!(row.plays, 1, "played on the NAS");
        assert!(row.available);
    }

    /// A file with no title tag still shows something useful, and the row
    /// never has to reach for a path to find it — ingest already did.
    #[test]
    fn a_title_never_falls_back_to_a_path_at_display_time() {
        use crate::model::entity::Device;
        use crate::model::instance::{Instance, Scanned};

        let mut i = Instance::new(Id::<Device>::derived(b"d"));
        i.ingest(
            100,
            &[Scanned {
                audio: crate::audio_id::AudioId { scheme: 1, hash: 7 },
                title: "04 Unknown Thing".into(), // what ingest put there
                artist: String::new(),
                ..Default::default()
            }],
        );
        let cat = i.catalogue();
        let rec = *cat.recordings.keys().next().unwrap();
        let l = Librarian::from_catalogue(&cat);

        let row = l.row(rec).unwrap();
        assert_eq!(row.title, "04 Unknown Thing");
        assert_eq!(
            row.file_under, UNKNOWN_ARTIST,
            "nothing named the artist, and that is an ordinary row"
        );
    }

    /// **A bad copy is a row with a verdict, not a folder** — issue #87.
    ///
    /// 726 files sit in `~/LowQualityMusic` because the library had one grade
    /// of membership: "this needs replacing" could only be said by taking it
    /// out. A folder cannot express *I have this, it is not good enough, and I
    /// want a better one* — only *I have this* or *I do not*.
    ///
    /// With the measurement in the model it is a filter, and the track is
    /// still there to play in the meantime.
    #[test]
    fn a_transcode_is_a_row_with_a_verdict() {
        use crate::model::encoding::{Format, Verdict};
        use crate::model::entity::Device;
        use crate::model::instance::{Instance, Scanned};

        let mut i = Instance::new(Id::<Device>::derived(b"laptop"));
        let lying = crate::audio_id::AudioId {
            scheme: 1,
            hash: 0xbad,
        };
        let honest = crate::audio_id::AudioId {
            scheme: 1,
            hash: 0x600d,
        };
        i.ingest(
            100,
            &[
                Scanned {
                    audio: lying,
                    title: "Isn't Love".into(),
                    artist: "Someone".into(),
                    format: Some(Format::Mp3),
                    bitrate: 320,
                    ..Default::default()
                },
                Scanned {
                    audio: honest,
                    title: "Fine".into(),
                    artist: "Someone".into(),
                    format: Some(Format::Mp3),
                    bitrate: 320,
                    ..Default::default()
                },
            ],
        );

        // Before anything is measured, nothing can be said.
        let before = Librarian::from_catalogue(&i.catalogue());
        assert!(
            before
                .encodings
                .encodings
                .values()
                .all(|e| e.verdict() == Verdict::Unmeasured),
            "a verdict without a measurement would be a guess"
        );

        // A quality pass runs — separately, because an FFT over a library is
        // minutes where hashing is seconds.
        let (bad, good) = (Id::of_audio(&lying), Id::of_audio(&honest));
        i.measured(200, bad, 15_700, crate::model::encoding::ANALYSER);
        i.measured(200, good, 19_800, crate::model::encoding::ANALYSER);

        let l = Librarian::from_catalogue(&i.catalogue());
        assert_eq!(
            l.encodings.encodings[&bad].verdict(),
            Verdict::Suspect,
            "320k claimed, 15.7 kHz measured — a lossy source re-encoded upward"
        );
        assert_eq!(l.encodings.encodings[&good].verdict(), Verdict::Good);

        // The re-buy list is a filter over rows that are still there, not a
        // directory listing of things that are not.
        let rebuy: Vec<_> = l
            .recordings()
            .filter_map(|r| l.row(r))
            .filter(|row| row.verdict == Some(Verdict::Suspect))
            .collect();
        assert_eq!(rebuy.len(), 1);
        assert_eq!(rebuy[0].title, "Isn't Love");
        assert!(
            rebuy[0].available,
            "and it still plays while you decide what to do about it"
        );
    }

    /// A re-measure targets what the old analyser touched, so a changed FFT
    /// does not mean redoing the library.
    #[test]
    fn an_older_measurement_is_findable() {
        use crate::model::encoding::{ANALYSER, Format};
        use crate::model::entity::Device;
        use crate::model::instance::{Instance, Scanned};

        let mut i = Instance::new(Id::<Device>::derived(b"laptop"));
        let a = crate::audio_id::AudioId { scheme: 1, hash: 1 };
        i.ingest(
            100,
            &[Scanned {
                audio: a,
                title: "x".into(),
                artist: "y".into(),
                format: Some(Format::Mp3),
                bitrate: 320,
                ..Default::default()
            }],
        );
        i.measured(200, Id::of_audio(&a), 19_000, ANALYSER - 1);

        let l = Librarian::from_catalogue(&i.catalogue());
        assert_eq!(l.encodings.stale_measurements(), vec![Id::of_audio(&a)]);
    }

    /// A recording nothing holds is still a row — decided about, not owned.
    #[test]
    fn something_not_owned_is_not_a_missing_row() {
        let l = Librarian::new();
        assert!(l.row(Id::derived(b"never-catalogued")).is_none());
    }
}

#[cfg(test)]
mod collections {
    //! **A playlist is a sequence of facts** — #92, and `domain-model.md`
    //! §"Collections".
    //!
    //! A collection and a playlist are one entity; a playlist is a collection
    //! whose order means something. What makes it a *sequence* rather than a
    //! set is that it admits duplicates and carries an order, and both of
    //! those have to survive a merge — which is what these check.

    use super::*;
    use crate::model::entity::Collection;
    use crate::model::instance::Instance;
    use crate::model::ord::OrdKey;

    fn instance(who: &[u8]) -> Instance {
        Instance::new(Id::<Device>::derived(who))
    }

    fn list() -> Id<Collection> {
        Id::<Collection>::derived(b"road trip")
    }

    fn song(n: u8) -> Id<Recording> {
        Id::<Recording>::derived(&[n])
    }

    /// Append `n` songs, and give back the keys so a test can insert between.
    fn appended(i: &mut Instance, songs: &[Id<Recording>]) -> Vec<OrdKey> {
        let mut keys: Vec<OrdKey> = Vec::new();
        for r in songs {
            let k = OrdKey::between(keys.last(), None);
            i.add_to_collection(100, list(), *r, k.clone());
            keys.push(k);
        }
        keys
    }

    fn shelved(i: &Instance) -> Vec<Id<Recording>> {
        Librarian::from_catalogue(&i.catalogue()).entries_of(list())
    }

    /// The ordinary case, and the one everything else is measured against.
    #[test]
    fn a_collection_comes_back_in_the_order_it_was_built() {
        let mut i = instance(b"laptop");
        i.name_collection(100, list(), "Road Trip", true);
        appended(&mut i, &[song(1), song(2), song(3)]);

        assert_eq!(shelved(&i), [song(1), song(2), song(3)]);
        assert_eq!(
            Librarian::from_catalogue(&i.catalogue()).collections()[&list()].name,
            "Road Trip"
        );
    }

    /// **The same recording twice is two entries.**
    ///
    /// The property that makes a view a sequence rather than a predicate. A
    /// set keyed by `(collection, recording)` could not say this, and playing
    /// one song twice in a queue is ordinary rather than exotic — which is why
    /// #92 says "the queue is just a view" is false unless a view can be one.
    #[test]
    fn one_recording_twice_is_two_entries() {
        let mut i = instance(b"laptop");
        appended(&mut i, &[song(1), song(2), song(1)]);
        assert_eq!(
            shelved(&i),
            [song(1), song(2), song(1)],
            "the duplicate was folded away"
        );
    }

    /// Inserting between two entries touches no other row — the whole reason
    /// a position is a fractional index and not an integer.
    #[test]
    fn inserting_between_two_entries_moves_nothing_else() {
        let mut i = instance(b"laptop");
        appended(&mut i, &[song(1), song(3)]);
        let before = i.catalogue().entries.clone();

        // Asked of the library rather than remembered by the caller, which is
        // how an edit will do it: the two keys it is landing between are what
        // it needs, and nothing else.
        let at = Librarian::from_catalogue(&i.catalogue()).positions_of(list());
        let mid = OrdKey::between(Some(&at[0]), Some(&at[1]));
        i.add_to_collection(200, list(), song(2), mid);

        assert_eq!(shelved(&i), [song(1), song(2), song(3)]);
        let after = i.catalogue().entries.clone();
        assert!(
            before.is_subset(&after) && after.len() == before.len() + 1,
            "inserting rewrote an existing entry"
        );
    }

    /// Removing one of two identical entries leaves the other, which is why
    /// the position is part of what a removal names.
    #[test]
    fn removing_one_of_two_identical_entries_keeps_the_other() {
        let mut i = instance(b"laptop");
        let keys = appended(&mut i, &[song(1), song(1)]);
        i.remove_from_collection(200, list(), song(1), keys[0].clone());

        assert_eq!(shelved(&i), [song(1)], "both copies went, or neither did");
    }

    /// **A removal is a row that says so**, not an absence.
    ///
    /// The same rule every other retraction follows: an absence cannot tell
    /// "somebody took this out" from "nobody put it in", and a merge with a
    /// device that still has the entry has to be able to.
    #[test]
    fn a_removal_survives_a_merge_with_a_device_that_still_holds_it() {
        let (mut mine, mut theirs) = (instance(b"laptop"), instance(b"nas"));
        let keys = appended(&mut mine, &[song(1), song(2)]);
        theirs.merge(150, mine.records());

        // I take one out; they never hear about it until we sync.
        mine.remove_from_collection(200, list(), song(2), keys[1].clone());
        theirs.merge(300, mine.records());

        assert_eq!(
            Librarian::from_catalogue(&theirs.catalogue()).entries_of(list()),
            [song(1)],
            "the removal did not cross"
        );
    }

    /// **Two devices building the same list agree on it**, and neither's
    /// entries are lost.
    ///
    /// Membership is row existence, so concurrent adds union — and the
    /// positions are what decides the order they union into.
    #[test]
    fn two_devices_adding_at_once_agree_on_the_result() {
        let (mut mine, mut theirs) = (instance(b"laptop"), instance(b"nas"));
        let keys = appended(&mut mine, &[song(1), song(4)]);
        theirs.merge(150, mine.records());

        // Both insert between the two, without seeing each other.
        let a = OrdKey::between(Some(&keys[0]), Some(&keys[1]));
        mine.add_to_collection(200, list(), song(2), a.clone());
        theirs.add_to_collection(200, list(), song(3), a);

        mine.merge(400, theirs.records());
        theirs.merge(400, mine.records());

        let (m, t) = (
            Librarian::from_catalogue(&mine.catalogue()).entries_of(list()),
            Librarian::from_catalogue(&theirs.catalogue()).entries_of(list()),
        );
        assert_eq!(m, t, "the two devices disagree about the list");
        assert_eq!(m.len(), 4, "an entry was lost in the merge");
        assert_eq!(m[0], song(1), "the list did not keep its ends");
        assert_eq!(m[3], song(4));
    }

    /// Moving an entry is a retraction and an assertion, because the position
    /// is part of which entry is meant.
    #[test]
    fn moving_an_entry_is_a_removal_and_an_add() {
        let mut i = instance(b"laptop");
        let keys = appended(&mut i, &[song(1), song(2), song(3)]);

        // Take the last one and put it first.
        i.remove_from_collection(200, list(), song(3), keys[2].clone());
        let front = OrdKey::between(None, Some(&keys[0]));
        i.add_to_collection(200, list(), song(3), front);

        assert_eq!(shelved(&i), [song(3), song(1), song(2)]);
    }

    /// **Two collections do not leak into each other.**
    ///
    /// The entries all live in one set, keyed by collection first — so the
    /// only thing keeping two playlists apart is that key, and every other
    /// test here uses a single list and could not tell.
    #[test]
    fn one_collection_does_not_see_anothers_entries() {
        let other = Id::<Collection>::derived(b"dinner");
        let mut i = instance(b"laptop");
        i.name_collection(100, list(), "Road Trip", true);
        i.name_collection(100, other, "Dinner", true);

        appended(&mut i, &[song(1), song(2)]);
        i.add_to_collection(100, other, song(9), OrdKey::first());

        let l = Librarian::from_catalogue(&i.catalogue());
        assert_eq!(l.entries_of(list()), [song(1), song(2)]);
        assert_eq!(l.entries_of(other), [song(9)]);
        assert_eq!(l.positions_of(other).len(), 1, "positions leaked too");
    }

    /// An unordered collection is the same entity with the flag off — the UI
    /// is free to keep calling the ordered one a playlist.
    #[test]
    fn a_collection_says_whether_its_order_means_anything() {
        let mut i = instance(b"laptop");
        i.name_collection(100, list(), "Odds and ends", false);
        let l = Librarian::from_catalogue(&i.catalogue());
        assert!(!l.collections()[&list()].ordered);
    }
}

#[cfg(test)]
mod perf {
    //! What it costs to rebuild the librarian — issue #123.
    //!
    //! `just bench-fold`. `Stop::Refold` runs this after `:collect` and
    //! `:add-view`, and on a 6,500-row library it was **467 ms** — four fifths
    //! of it inside `draw`, which asked four linear scans of the whole credit
    //! list per row.
    //!
    //! The phases are printed separately because they fail differently, and a
    //! single number would have hidden which one went wrong. Same reason as
    //! `shelf::perf`, and the same lesson: the fold had no measurement, so
    //! nothing said it had gone bad.

    use std::time::Instant;

    use super::*;
    use crate::model::instance::{Instance, Scanned};

    /// A library of `albums` albums, ten tracks each, over `artists` artists —
    /// with credits and genres, because those are what the scans were over.
    fn scan_of(artists: usize, albums: usize) -> Vec<Scanned> {
        const GENRES: [&str; 6] = ["Techno", "House", "Folk", "Rock", "Jazz", "Ambient"];
        let mut scan = Vec::new();
        for a in 0..albums {
            for track in 1..=10u16 {
                let artist = format!("Artist {:04}", a % artists);
                scan.push(Scanned {
                    audio: crate::audio_id::AudioId {
                        scheme: 1,
                        hash: (a as u128) * 1000 + u128::from(track),
                    },
                    title: format!("Track {track:02} of {a:04}"),
                    artist: artist.clone(),
                    album: Some(format!("Album {a:04}")),
                    album_artist: Some(artist),
                    genres: vec![GENRES[a % GENRES.len()].to_string()],
                    disc: Some(1),
                    track: Some(track),
                    length_ms: 200_000,
                    ..Default::default()
                });
            }
        }
        scan
    }

    /// The table, and the bound that makes it a test rather than a report.
    ///
    /// **Generous on purpose**, like `shelf::perf`. What it has to catch is a
    /// per-row scan reappearing, which costs a factor of ten or more, not a
    /// 20% drift on a busy machine.
    #[test]
    fn rebuilding_the_librarian_does_not_scan_per_row() {
        let device = crate::fixture::device();
        let scan = scan_of(200, 650);

        let t = Instant::now();
        let mut log = Instance::new(device);
        log.ingest(100, &scan);
        let ingest = t.elapsed();

        let t = Instant::now();
        let cat = log.catalogue();
        let folded = t.elapsed();

        let t = Instant::now();
        let mut l = Librarian::from_catalogue(&cat);
        let built = t.elapsed();
        l.here(device);

        let rows = crate::model::rows::Listing::rows(&l).len();
        eprintln!(
            "\n{rows} rows, {} records\n  ingest          {ingest:>9.1?}\n  \
             catalogue()     {folded:>9.1?}\n  from_catalogue  {built:>9.1?}",
            log.records().len()
        );

        assert!(rows > 6000, "the bench needs a library to measure");
        assert!(
            built < std::time::Duration::from_millis(400),
            "from_catalogue took {built:?} — something is scanning once per row again"
        );
    }
}
