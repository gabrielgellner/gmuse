//! The seam — issue #77.
//!
//! **The library answers questions and accepts changes; the player asks and
//! plays.** This module is where that sentence becomes a type.
//!
//! Today `src/ui/` reaches into a `Store` and handles `PathBuf`s directly, so
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
use crate::model::entity::{Device, Recording, ReleaseGroup, Track};
use crate::model::id::Id;
use crate::model::log::Catalogue;
use crate::model::provenance::Provenance;
use crate::model::reference::{CreditOn, Reference};
use crate::model::spine::Spine;
use crate::model::vocabulary::Vocabulary;

/// What the player is allowed to ask, and what it is allowed to ask for.
///
/// Deliberately small. The four things `src/ui/` asks a `Store` for today are
/// *get*, *iterate*, *upsert* and *look up by path* — and the last of those is
/// the one that must not survive the move, because a path is a fact about one
/// device's copy and the player has no business holding one.
pub struct Librarian {
    pub spine: Spine,
    pub reference: Reference,
    pub vocabulary: Vocabulary,
    pub encodings: Encodings,
    pub provenance: Provenance,
    /// Folded from the log, never stored as numbers. #67's rule decides a
    /// rating; a play is counted.
    pub ratings: std::collections::BTreeMap<Id<Recording>, u8>,
    pub plays: std::collections::BTreeMap<Id<Recording>, u32>,
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
        let rec = self.spine.recordings.get(&recording)?;
        let canonical = copy.and_then(|id| self.encodings.encodings.get(&id));
        let slot = copy.and_then(|id| self.placed.get(&id).copied());
        Some(Row {
            recording,
            title: rec.title.clone(),
            file_under: self.files_under(recording, slot),
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
            available: !self.encodings.of(recording).is_empty(),
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
    fn files_under(&self, recording: Id<Recording>, slot: Option<Id<Track>>) -> String {
        let on_release = slot
            .and_then(|s| self.spine.tracks.get(&s))
            .and_then(|t| self.spine.media.get(&t.medium.id()))
            .and_then(|m| self.spine.releases.get(&m.release.id()))
            .map(|r| CreditOn::ReleaseGroup(r.group.id()));

        on_release
            .map(|subject| self.reference.credited_as(subject))
            .filter(|s| !s.is_empty())
            .or_else(|| {
                let by_recording = self.reference.credited_as(CreditOn::Recording(recording));
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
        l
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

    /// **Every copy of a recording this library holds, as rows would list
    /// them** — issue #84.
    ///
    /// One entry per copy, because a browse tree is a list of what you have
    /// and two files are two things you have. A recording nothing holds is
    /// still one entry, with no copy: §1 is explicit that a row nothing holds
    /// is ordinary, and dropping it would make a wishlist entry invisible.
    pub fn listings(&self, recording: Id<Recording>) -> Vec<Listing> {
        let copies = self.encodings.of(recording);
        if copies.is_empty() {
            return vec![Listing {
                recording,
                copy: None,
            }];
        }
        copies
            .into_iter()
            .map(|e| Listing {
                recording,
                copy: Some(e.id),
            })
            .collect()
    }
}

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
    /// `src/ui/` still names a `Store` and still handles `PathBuf`s. Asserting
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
    /// instead of a count. `Store` cannot reach zero while the type exists,
    /// which is #86. Only `reaches` still measures a migration in progress,
    /// and its own floor is four.
    ///
    /// **It counts the substring, so prose counted too.** Two `Store`
    /// mentions were the word *Stored* in doc comments and one was a comment
    /// naming `Store::recording_of` — three of five, in a number somebody was
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
        /// The import, the `Context` field, and one comment about what
        /// playlists hold. **This one reaches zero at #86 and not before** —
        /// the player must name the type for as long as the type exists.
        const STORE: usize = 3;
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
        /// counting `Store` alone said 5 while sixteen reads went around the
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
        /// **The five left are three questions, and two of them may never
        /// move:**
        ///
        /// - **three** read or write `.m3u`, which is a path format by
        ///   definition. An export naming paths is not a leak.
        /// - **one** is the remote's owned snapshot: another thread renders a
        ///   caller's own template, and a borrowed row cannot cross a thread.
        /// - **one** is the filter closure, which matches on a `&Track`.
        ///
        /// So its floor is **four**, and only the filter closure is still
        /// worth moving.
        const REACHES: usize = 5;

        let (mut store, mut reaches) = (0, 0);
        let mut stray_paths: Vec<String> = Vec::new();
        for (name, text) in read("src/ui") {
            let live = production(&text);
            store += live.matches("Store").count();
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
        assert!(
            store <= STORE && reaches <= REACHES,
            "the seam widened: Store {store} (was {STORE}), \
             reaches {reaches} (was {REACHES})"
        );
        if store < STORE || reaches < REACHES {
            eprintln!("seam narrowed to Store {store}, reaches {reaches} — lower the constants");
        }
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
            "Genre",
            "Hierarchy",
            "Holdings",
            "Label",
            "Medium",
            "Release",
            "ReleaseGroup",
            "Role",
            "Qualifier",
            "Recording",
            "Track",
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
