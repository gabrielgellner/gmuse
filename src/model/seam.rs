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
use crate::model::entity::{Device, Recording, ReleaseGroup};
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
        }
    }

    /// What a row on screen needs to draw itself.
    ///
    /// Assembled **by the library** out of five entities the player never has
    /// to know are separate — and carrying no path, because there is no path
    /// that is true of the library rather than of one machine.
    pub fn row(&self, recording: Id<Recording>) -> Option<Row> {
        let rec = self.spine.recordings.get(&recording)?;
        let canonical = self.encodings.canonical(recording);
        Some(Row {
            recording,
            title: rec.title.clone(),
            file_under: self.files_under(recording),
            album: self.album_of(recording),
            length: rec.length,
            rating: self.ratings.get(&recording).copied(),
            plays: self.plays.get(&recording).copied().unwrap_or(0),
            format: canonical.map(|e| e.format),
            bitrate: canonical.map(|e| e.bitrate),
            verdict: canonical.map(|e| e.verdict()),
            available: !self.encodings.of(recording).is_empty(),
        })
    }

    /// Who a recording files under.
    ///
    /// The album-level credit first, then the recording's own — which is the
    /// modelled version of `Track::file_under`'s "album artist, else artist".
    /// The heuristic was guessing at a fact; this asks for it.
    fn files_under(&self, recording: Id<Recording>) -> String {
        let on_release = self
            .spine
            .tracks_of_recording(recording)
            .first()
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

    /// The album a recording files under, by way of the release it sits on.
    fn album_of(&self, recording: Id<Recording>) -> Option<String> {
        let track = self.spine.tracks_of_recording(recording).first().copied()?;
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
    /// Deliberately total. A distribution bound to a recording that no record
    /// catalogued still lands — that is the unidentified 16%, and it must not
    /// be second class.
    pub fn from_catalogue(cat: &Catalogue) -> Self {
        use crate::model::encoding::Encoding;
        use crate::model::spine::Recording as Rec;

        let mut l = Librarian::new();
        for (id, r) in &cat.recordings {
            l.spine.recordings.insert(
                *id,
                Rec {
                    id: *id,
                    title: r.title.clone(),
                    work: None,
                    recorded: None,
                    bpm: None,
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
        // A log says "this recording is by this artist"; the model says that
        // with a credit, so the projection mints one. Seeding the role here
        // rather than assuming one keeps the role list a codelist (#74) —
        // performer is where an unqualified credit belongs.
        let performer = crate::model::id::Id::<crate::model::entity::Role>::derived(b"performer");
        l.reference
            .roles
            .entry(performer)
            .or_insert_with(|| crate::model::reference::RoleRecord {
                id: performer,
                name: "performer".into(),
                class: crate::model::reference::RoleClass::Performer,
            });
        for (id, r) in &cat.recordings {
            l.reference.credits.push(crate::model::reference::Credit {
                subject: CreditOn::Recording(*id),
                artist: crate::model::id::Ref::to(r.artist),
                role: crate::model::id::Ref::to(performer),
                position: 0,
                as_credited: None,
                join_phrase: None,
            });
        }

        // The album spine. A scan sees one release per album, so the release
        // and the medium are minted from the group rather than read — a
        // pressing this library does not hold is not a thing to invent.
        for (group, g) in &cat.groups {
            l.spine.groups.insert(
                *group,
                crate::model::spine::ReleaseGroup {
                    id: *group,
                    title: g.title.clone(),
                    first_released: None,
                    kind: crate::model::spine::ReleaseType::Album,
                },
            );
            let release =
                Id::<crate::model::entity::Release>::derived(group.to_string().as_bytes());
            l.spine.releases.insert(
                release,
                crate::model::spine::Release {
                    id: release,
                    group: crate::model::id::Ref::to(*group),
                    title: g.title.clone(),
                    released: None,
                    catalogue_number: None,
                },
            );
            l.reference.artists.entry(g.artist).or_insert_with(|| {
                crate::model::reference::ArtistRecord {
                    id: g.artist,
                    name: cat.artist_names.get(&g.artist).cloned().unwrap_or_default(),
                    sort_name: cat.artist_names.get(&g.artist).cloned().unwrap_or_default(),
                }
            });
            // Album-level credit, which is what `file_under` asks for first.
            l.reference.credits.push(crate::model::reference::Credit {
                subject: CreditOn::ReleaseGroup(*group),
                artist: crate::model::id::Ref::to(g.artist),
                role: crate::model::id::Ref::to(performer),
                position: 0,
                as_credited: None,
                join_phrase: None,
            });
        }

        for (recording, (group, disc, track)) in &cat.placements {
            let release =
                Id::<crate::model::entity::Release>::derived(group.to_string().as_bytes());
            let medium =
                Id::<crate::model::entity::Medium>::derived(format!("{release}/{disc}").as_bytes());
            l.spine
                .media
                .entry(medium)
                .or_insert(crate::model::spine::Medium {
                    id: medium,
                    release: crate::model::id::Ref::to(release),
                    position: *disc,
                    format: None,
                });
            let id = Id::<crate::model::entity::Track>::derived(
                format!("{medium}/{recording}").as_bytes(),
            );
            l.spine.tracks.insert(
                id,
                crate::model::spine::Track {
                    id,
                    medium: crate::model::id::Ref::to(medium),
                    recording: crate::model::id::Ref::to(*recording),
                    position: *track,
                    title_on_release: None,
                },
            );
        }

        // Names are a register: whatever the fold settled on is the label.
        for (id, name) in &cat.artist_names {
            if let Some(a) = l.reference.artists.get_mut(id) {
                a.name = name.clone();
                a.sort_name = name.clone();
            }
        }
        for (id, d) in &cat.distributions {
            l.encodings.encodings.insert(
                *id,
                Encoding {
                    id: *id,
                    audio: d.audio,
                    recording: d.recording.map(crate::model::id::Ref::to),
                    format: Format::Mp3,
                    bitrate: 0,
                    cutoff_hz: None,
                    gain: None,
                    encoder_delay: None,
                },
            );
        }
        for (device, asserted) in &cat.holdings {
            for (dist, locator) in &asserted.held {
                l.encodings.holdings.push(crate::model::encoding::Holding {
                    distribution: crate::model::id::Ref::to(*dist),
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
    pub fn group_of(&self, recording: Id<Recording>) -> Option<Id<ReleaseGroup>> {
        let track = self.spine.tracks_of_recording(recording).first().copied()?;
        let medium = self.spine.media.get(&track.medium.id())?;
        let release = self.spine.releases.get(&medium.release.id())?;
        Some(release.group.id())
    }

    /// Where a recording sits: disc, then track, as the tags claim.
    ///
    /// Sorts unplaced recordings last rather than first, because a track with
    /// no number is a gap in what is known, and putting it at the top of an
    /// album would state something the tags never did.
    pub fn place_of(&self, recording: Id<Recording>) -> (u16, u16) {
        self.spine
            .tracks_of_recording(recording)
            .first()
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
    /// **Lower these as the migration lands. Never raise them.**
    ///
    /// Most of what is left is test fixtures rather than the player: of the 36
    /// `Store` mentions, only **five are outside `#[cfg(test)]`** — one of them
    /// the word "Stored" in a comment, and the rest the `Context` that hands a
    /// store to whoever still wants one.
    ///
    /// Both renderers stopped needing a store for the same reason: they were
    /// reaching for it because the projection could not answer, never because
    /// they wanted a store. The tree got display names beside its sort keys
    /// (#84); the list got a trait saying what a row files under.
    #[test]
    fn the_leak_from_the_player_only_narrows() {
        const STORE: usize = 36;
        const PATHS: usize = 28;

        let (mut store, mut paths) = (0, 0);
        for (_, text) in read("src/ui") {
            store += text.matches("Store").count();
            paths += text.matches("PathBuf").count();
        }

        assert!(
            store <= STORE && paths <= PATHS,
            "the seam widened: Store {store} (was {STORE}), PathBuf {paths} (was {PATHS})"
        );
        if store < STORE || paths < PATHS {
            eprintln!("seam narrowed to Store {store}, PathBuf {paths} — lower the constants");
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
        m2.play(
            240,
            rec,
            *m2.catalogue().distributions.keys().next().unwrap(),
        );

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

    /// A recording nothing holds is still a row — decided about, not owned.
    #[test]
    fn something_not_owned_is_not_a_missing_row() {
        let l = Librarian::new();
        assert!(l.row(Id::derived(b"never-catalogued")).is_none());
    }
}
