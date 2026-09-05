//! What the disk said, this run — DESIGN §4 (INVARIANT).
//!
//! It was called `Store`, which claimed too much: this is not where the
//! library lives, it is a scan result — discardable, rebuilt, and upstream of
//! everything durable. The librarian is what knows things; this is what the
//! walker found.
//!
//! **The file is still `store.rs` and should join `scan.rs`.** The walker and
//! what it produces belong in one module, and separating that from the rename
//! keeps a 500-line move out of a diff about names (#86).
//!
//! One store; every view is a projection over it. Track identity is an integer
//! index into an arena, never a pointer or an `Rc`. That keeps the whole model
//! `Send`, makes views cheap to rebuild, and lets the same track appear in the
//! library, several playlists and the queue as the same `u32`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::model::entity::Encoding;
use crate::model::id::{Handle, Id};
#[cfg(test)]
use crate::model::spine::Recording;

/// A handle to a track. Stable for the life of the store.
///
/// Never derived from a path at read time — look it up once and keep the id.
/// A track's place in the arena.
///
/// **The same type the model uses**, not a second name for it. There were two
/// arena-index types with the same meaning and the same shape; keeping both
/// meant every site had to be read to know which world it belonged to, and
/// `history.rs`'s warning — "a history keyed by id would silently reattach to
/// different tracks" — had to be remembered rather than enforced.
///
/// It is a `Handle`, so it has no `Serialize` and cannot be written down. The
/// durable thing is `Id<Recording>`.
///
/// ## A hazard that exists only during the migration
///
/// **A handle to a copy, and that is what it always indexed.**
///
/// It was `Handle<Recording>`, which said the wrong thing: one is minted per
/// *file*, so two rips of one performance are two of these and one recording.
/// After #78 a displayed row names a copy, and this is that row's key.
///
/// The rename also closes a hazard this doc used to describe rather than
/// prevent. Two arenas exist — this `Scan` and `model::arena::Arena` — and
/// their handles were the same type; `Arena` refuses a handle from an older
/// generation, but a `Scan` index carries generation zero, which is what a
/// *fresh* `Arena` also hands out. So a `Scan` index passed to a new `Arena`
/// would have resolved rather than been refused, silently naming a different
/// entity.
///
/// It does not compile now, and `a_store_handle_cannot_be_read_as_a_recording_handle`
/// keeps it that way.
pub type TrackId = Id<Encoding>;

/// Where a track sits in the arena.
///
/// Private, and it stays private. It was `TrackId` until #86 — the player held
/// arena indices, so nothing it remembered could outlive the arena, which is
/// why a session and a playlist had to be written down as paths. A row is
/// named by its bytes now and this is only how `Scan` finds one.
type Slot = Handle<Encoding>;

/// ReplayGain, as found in tags. Values are dB (gain) and linear (peak).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct ReplayGain {
    pub track_gain: Option<f32>,
    pub track_peak: Option<f32>,
    pub album_gain: Option<f32>,
    pub album_peak: Option<f32>,
}

impl ReplayGain {
    pub fn is_empty(&self) -> bool {
        self.track_gain.is_none()
            && self.track_peak.is_none()
            && self.album_gain.is_none()
            && self.album_peak.is_none()
    }
}

/// One track's metadata. Display strings are kept verbatim; normalised sort
/// keys belong to the `Shelf` projection, not here.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub path: PathBuf,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub albumartist: Option<String>,
    pub title: Option<String>,
    pub tracknumber: Option<u32>,
    pub discnumber: Option<u32>,
    pub date: Option<i32>,
    /// The genres this file's tags name, one per term.
    ///
    /// **A list, because a genre tag is one.** `Rock; Blues Rock; Alternative
    /// Rock` is three terms in a hierarchical codelist, not one genre with an
    /// unusual name, and keeping it as a single string made it unfilterable:
    /// `genre=Rock` could not match a file whose tag also mentioned two other
    /// things (#86).
    #[serde(default)]
    pub genres: Vec<String>,
    /// The record label, from `TPUB` / `ORGANIZATION` / `LABEL`.
    ///
    /// Its own field because it is otherwise homeless: a label sampler is a
    /// *compilation*, so its `albumartist` belongs to "Various Artists" and
    /// the label has nowhere else to go. Bandcamp writes the label into
    /// `albumartist` for want of anywhere better, which is what makes a label
    /// appear in the library as though it were an artist.
    pub label: Option<String>,
    pub duration: Duration,
    pub bitrate: Option<u32>,
    pub samplerate: Option<u32>,
    pub channels: Option<u8>,
    /// Seconds since the epoch, for incremental rescan.
    pub mtime: i64,
    /// File size in bytes.
    ///
    /// Not used for scanning — mtime decides that — but it is the strongest
    /// signal available for recognising a *moved* file, whose bytes are
    /// unchanged and whose path is not (#25).
    #[serde(default)]
    pub size: u64,
    /// What this file's *audio* is, independent of its tags and its path.
    ///
    /// `#[serde(default)]` so an existing cache loads with `None` and fills in
    /// as files are next read, rather than forcing a whole-library rehash on
    /// the first launch after an upgrade.
    ///
    /// `Option` because a file gmuse cannot parse still belongs in the library
    /// (#53) — it simply has no identity yet.
    #[serde(default)]
    pub audio_id: Option<crate::audio_id::AudioId>,
    /// The MusicBrainz recording id from the tags, where there is one — 91% of
    /// this library.
    ///
    /// Read here because the file is already open and the tags are already
    /// parsed, and because without it a history key would have to derive from
    /// the audio for every file, losing the one thing an external id buys: a
    /// re-encode keeping its history.
    #[serde(default)]
    pub mb_recording: Option<uuid::Uuid>,
    pub replaygain: Option<ReplayGain>,
}

impl Track {
    /// The artist a track should file under.
    ///
    /// DESIGN §11.4 asks for this rule to be decided early and written down:
    /// **`albumartist` wins, falling back to `artist`, then to a literal
    /// "Unknown Artist".** Filing by `artist` would scatter a compilation
    /// across a dozen one-track folders, and split a normal album the moment
    /// one track has a guest credit — which is exactly what
    /// `Anderson .Paak/Malibu` does in the test library.
    pub fn file_under(&self) -> &str {
        self.albumartist
            .as_deref()
            .or(self.artist.as_deref())
            .unwrap_or(UNKNOWN_ARTIST)
    }

    /// This file as a **row** — what a display template asks for.
    ///
    /// Here rather than only in `Scan::listed` because the remote holds a
    /// cloned `Track` so another thread can render a caller's own format, and
    /// it needs the same view without an arena to look in.
    pub fn listed(&self) -> crate::model::rows::Listed<'_> {
        crate::model::rows::Listed {
            file_under: self.file_under(),
            album: self.album_or_unknown(),
            disc: self.discnumber,
            track: self.tracknumber,
            title: self.display_title(),
            duration: self.duration,
            artist: self.artist.as_deref(),
            album_artist: self.albumartist.as_deref(),
            tagged_title: self.title.as_deref(),
            tagged_album: self.album.as_deref(),
            genre: &self.genres,
            label: self.label.as_deref(),
            year: self.date,
            bitrate: self.bitrate,
            sample_rate: self.samplerate,
            channels: self.channels,
        }
    }

    /// The album a track should file under, or [`NO_ALBUM`] when it has none.
    pub fn album_or_unknown(&self) -> &str {
        self.album.as_deref().unwrap_or(NO_ALBUM)
    }

    /// What to show when there is no title tag: the file name, which is far
    /// more useful than "Unknown".
    pub fn display_title(&self) -> &str {
        self.title.as_deref().unwrap_or_else(|| {
            self.path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("<unnamed>")
        })
    }
}

pub const UNKNOWN_ARTIST: &str = "Unknown Artist";
/// What a track with no album is filed under.
///
/// Lowercase on purpose: album titles are capitalised, so this reads as a
/// category rather than as a record called "Singles" — and there is no such
/// record, which is the whole point of leaving the tag empty.
///
/// This is presentation only. `Track::album` stays `None`, so the format
/// language's `[{album}]` groups still vanish and the filter's `!album?` still
/// finds exactly these tracks.
pub const NO_ALBUM: &str = "singles";

/// The arena.
///
/// `tracks[n]` is `TrackId::new(n)`. Removal leaves a `None` tombstone rather than
/// shifting, because shifting would invalidate every `TrackId` held in a
/// playlist or the queue (DESIGN §4).
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Scan {
    tracks: Vec<Option<Track>>,
    /// **Rebuilt on load, never persisted.** It is derivable from `tracks` in
    /// one pass, and not writing it is what lets [`TrackId`] have no
    /// `Serialize` at all — so the rule that an arena index must not outlive
    /// its arena stops being a comment and becomes something the compiler
    /// refuses to let anyone break.
    ///
    /// The same trick `Handle<T>` uses in `model/id.rs`, applied to the type
    /// it was modelled on.
    #[serde(skip)]
    by_path: HashMap<PathBuf, Slot>,
    /// Durable id to arena slot. **Rebuilt on load, never persisted**, for the
    /// same reason as `by_path`.
    ///
    /// The arena is an implementation detail that dies with this file (#86).
    /// What callers hold is the id of the *bytes*, so a playlist entry means
    /// the same thing after a restart, after a rescan, and on another machine.
    #[serde(skip)]
    by_id: BTreeMap<TrackId, Slot>,
    /// Count of tombstones, so callers can decide when a compaction is worth it.
    tombstones: usize,
}

/// Fixture bytes for a file that a test does not care about the audio of.
///
/// A row is named by its audio (#86), so a `Track` with no `audio_id` is a
/// file the store can hold and will never list — which is right, and which
/// makes every fixture that builds one by hand say what it is holding. This
/// derives them from the path, because distinct paths in a test mean distinct
/// files unless the test is about the case where they do not.
#[cfg(test)]
pub fn heard(mut track: Track) -> Track {
    let seed = track
        .path
        .to_string_lossy()
        .bytes()
        .fold(0u64, |a, b| a.wrapping_mul(31).wrapping_add(u64::from(b)));
    track.audio_id = Some(crate::audio_id::AudioId {
        scheme: 1,
        hash: u128::from(seed),
    });
    track
}

/// A row id for a fixture, deterministic and distinct per number.
///
/// Tests used to write `crate::store::row_of_test(3)` — an arena index, which is exactly
/// what a row id stopped being (#86). Derived from the number so the same
/// fixture gets the same id every run, and so a test that asserts an ordering
/// asserts a real one.
#[cfg(test)]
pub fn row_of_test(n: u32) -> TrackId {
    TrackId::derived(&n.to_le_bytes())
}

/// The row id a track has, if its bytes could be read.
///
/// One place, because "a row is named by its audio" is the rule the whole file
/// now turns on and it should be stated once.
fn row_id(track: &Track) -> Option<TrackId> {
    Some(Id::<Encoding>::of_audio(&track.audio_id?))
}

impl Scan {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuild the path index after loading.
    ///
    /// Must be called once on anything that came back from the cache, because
    /// the index is deliberately not stored. One pass over the arena, which at
    /// 5774 tracks is not a cost anybody can measure.
    pub fn reindex(&mut self) {
        self.by_path = self
            .tracks
            .iter()
            .enumerate()
            .filter_map(|(i, t)| t.as_ref().map(|t| (t.path.clone(), Slot::new(i as u32))))
            .collect();
        self.by_id = self
            .tracks
            .iter()
            .enumerate()
            .filter_map(|(i, t)| Some((row_id(t.as_ref()?)?, Slot::new(i as u32))))
            .collect();
    }

    /// Number of rows.
    ///
    /// Rows, not files: two paths holding identical bytes are one encoding and
    /// therefore one of these (#86). A file whose audio would not read is
    /// neither.
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Size of the id space, including tombstones.
    ///
    /// Test-only since `--repl` was retired: `print_library` was the one
    /// caller outside tests, and the arena's shape is not something any view
    /// shows. Kept rather than inlined into the tests because the invariant it
    /// checks — ids are never reused — is worth asserting by name.
    #[cfg(test)]
    pub fn capacity(&self) -> usize {
        self.tracks.len()
    }

    #[cfg(test)]
    pub fn tombstones(&self) -> usize {
        self.tombstones
    }

    pub fn get(&self, id: TrackId) -> Option<&Track> {
        self.at(*self.by_id.get(&id)?)
    }

    fn at(&self, slot: Slot) -> Option<&Track> {
        self.tracks.get(slot.index() as usize)?.as_ref()
    }

    /// The **row** at this path, if the file there has one.
    pub fn id_for_path(&self, path: &Path) -> Option<TrackId> {
        row_id(self.track_at(path)?)
    }

    /// The **file** at this path, row or not.
    ///
    /// Distinct from `id_for_path` on purpose. A file whose audio would not
    /// read is not a row, and the scanner still has to reason about it: move
    /// detection falls back to a shape signature precisely for those, and it
    /// cannot do that for a file it is unable to look up (#86).
    pub(crate) fn track_at(&self, path: &Path) -> Option<&Track> {
        self.at(*self.by_path.get(path)?)
    }

    /// Live tracks, with their ids.
    ///
    /// **A file whose audio could not be read is not here.** It has no
    /// encoding, so it has no row — and it is not playable either, since the
    /// same read fails. `Report::unhashed` names them rather than leaving them
    /// to vanish, which is the lesson !179 paid for.
    pub fn iter(&self) -> impl Iterator<Item = (TrackId, &Track)> {
        // Driven by the id index rather than the arena, so two paths holding
        // identical bytes are drawn once. Ordered, because a `BTreeMap` is:
        // walking the arena gave scan order, and a projection that changed
        // with the order files happened to be read would be a bad trade for a
        // hash lookup.
        self.by_id
            .iter()
            .filter_map(|(id, slot)| Some((*id, self.at(*slot)?)))
    }

    /// Insert a new track, or update the one already at that path.
    ///
    /// Updating in place is what keeps a rescan from invalidating ids: a file
    /// whose tags changed keeps the id a playlist is holding.
    /// The identity history hangs off, for a track this store holds.
    ///
    /// Here rather than on `Track` so that the *store* answers it: the key is a
    /// library fact, and a caller holding a `Track` is holding a copy of what
    /// the library knows.
    pub fn history_key(&self, id: TrackId) -> Option<String> {
        let t = self.get(id)?;
        crate::history::key_for(t.mb_recording, t.audio_id)
    }

    /// The same identity, typed — what a fact about this track names.
    ///
    /// `None` for a file with neither a MusicBrainz id nor readable audio.
    /// That is a real answer: there is nothing durable to record against, and
    /// recording against the path instead is the bug #81 closes.
    /// **Tests only, and it must stay that way.**
    ///
    /// It mints from the tags, so it cannot see a relink: a file pointed at
    /// another recording still reports the id its tags imply. Nothing noticed
    /// for a long time because the two agree for every file nobody has
    /// relinked. Where they differ, a rating would go to the song a file used
    /// to be — not seen in this library, because every relink applied so far
    /// also carried a merge and the fold redirects through those.
    ///
    /// The library's binding is the answer (`ui::recording_of`). This is left
    /// reachable only where a fixture wants the id a scan would mint, and the
    /// `cfg` is what stops it being reached for again.
    #[cfg(test)]
    pub fn recording_of(&self, id: TrackId) -> Option<Id<Recording>> {
        let t = self.get(id)?;
        crate::history::recording_id_for(t.mb_recording, t.audio_id)
    }

    /// Every live **file**, whatever it is a row of.
    ///
    /// Distinct from `iter`, which yields **rows** — and two files holding
    /// identical bytes are one row (#86). Anything reasoning about the disk
    /// wants this one: what left, what arrived, and what to write to the
    /// cache are questions about files. Making the scan walk rows silently
    /// hid the second copy of a duplicated file, so a move looked like a
    /// deletion and a saved cache lost the copy it did not list.
    pub(crate) fn files(&self) -> impl Iterator<Item = &Track> {
        self.tracks.iter().filter_map(Option::as_ref)
    }

    /// Insert or update, returning the row id when the file has one.
    ///
    /// `None` for a file whose audio would not read: there is no encoding to
    /// name it by. The track is still stored, so a rescan does not read it
    /// again, and `iter` leaves it out.
    pub fn upsert(&mut self, track: Track) -> Option<TrackId> {
        let id = row_id(&track);
        let slot = match self.by_path.get(&track.path).copied() {
            Some(slot) => {
                if let Some(old) = self.at(slot).and_then(row_id) {
                    self.by_id.remove(&old);
                }
                self.tracks[slot.index() as usize] = Some(track);
                slot
            }
            None => {
                let slot = Slot::new(self.tracks.len() as u32);
                self.by_path.insert(track.path.clone(), slot);
                self.tracks.push(Some(track));
                slot
            }
        };
        if let Some(id) = id {
            // Two paths, identical bytes: one row. The second wins, as
            // `HoldingsAsserted` does — it is a map keyed by encoding too.
            self.by_id.insert(id, slot);
        }
        id
    }

    /// Tombstone the track at `path`, if present.
    pub fn remove_path(&mut self, path: &Path) -> Option<TrackId> {
        let slot = self.by_path.remove(path)?;
        let id = self.at(slot).and_then(row_id);
        if let Some(taken) = self.tracks.get_mut(slot.index() as usize)
            && taken.take().is_some()
        {
            self.tombstones += 1;
        }
        // Two paths can share an id, and dropping one copy must not unlist the
        // other — so the id is re-pointed at a surviving holder where there is
        // one, and only dropped when there is none. The search is over
        // `by_path` rather than the arena because a tombstoned slot is gone
        // from it already, and removals are per-file during a scan of what
        // vanished: rare, and bounded by what actually left.
        if let Some(id) = id
            && self.by_id.get(&id) == Some(&slot)
        {
            match self
                .by_path
                .values()
                .copied()
                .find(|other| self.at(*other).and_then(row_id) == Some(id))
            {
                Some(other) => self.by_id.insert(id, other),
                None => self.by_id.remove(&id),
            };
        }
        id
    }

    /// Drop tombstones.
    ///
    /// **No longer a remap** (#86). It returned `old id -> new id` because ids
    /// were arena indices and rebuilding renumbered them, so every holder had
    /// to be walked. A row id is derived from the file's bytes now, so it does
    /// not move when the arena does and nothing needs telling.
    #[allow(
        dead_code,
        reason = "removal is specified as tombstone-or-rebuild; nothing rebuilds yet"
    )]
    pub fn compact(&mut self) {
        self.tracks.retain(Option::is_some);
        self.tombstones = 0;
        self.reindex();
    }
}

impl Scan {
    /// A compacted copy for persisting.
    ///
    /// Tombstones exist to keep live `TrackId`s valid within a session; there
    /// is nothing holding an id across a restart, so writing them to the cache
    /// would only grow the file and the id space for ever.
    pub(crate) fn clone_for_save(store: &Scan) -> Scan {
        let mut out = Scan::new();
        // Every file, not every row: a duplicated file that did not win its
        // id still has to come back after a restart, or the next scan reports
        // it as an arrival and the one before it as gone.
        for track in store.files() {
            out.upsert(track.clone());
        }
        out
    }
}

/// The read contract `ui/list.rs` names, answered from the arena.
///
/// A temporary home: it moves to the librarian with #86, and the renderer does
/// not change when it does — which is the point of the trait existing at all.
impl crate::model::rows::Listing for Scan {
    fn listed(&self, id: TrackId) -> Option<crate::model::rows::Listed<'_>> {
        Some(self.get(id)?.listed())
    }

    fn rows(&self) -> Vec<TrackId> {
        self.iter().map(|(id, _)| id).collect()
    }

    fn audio_of(&self, id: TrackId) -> Option<crate::audio_id::AudioId> {
        self.get(id)?.audio_id
    }

    /// **Minted from the tags, which is the wrong answer after a relink.**
    ///
    /// The store has no binding to consult — that lives in the library — so
    /// this reports the recording a file's tags imply. It agrees with the
    /// library for every file nobody has relinked, and this implementation
    /// goes with the store (#86).
    fn recording_of(&self, id: TrackId) -> Option<Id<crate::model::spine::Recording>> {
        let t = self.get(id)?;
        crate::history::recording_id_for(t.mb_recording, t.audio_id)
    }

    fn playable(&self, id: TrackId) -> Option<crate::model::rows::Playable<'_>> {
        let t = self.get(id)?;
        Some(crate::model::rows::Playable {
            locator: t.path.to_str()?,
            gain: t.replaygain.as_ref().map(|g| crate::model::rows::Loudness {
                track_db: g.track_gain,
                track_peak: g.track_peak,
                album_db: g.album_gain,
                album_peak: g.album_peak,
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    /// A track with no album is *filed* under a label, but its tag stays
    /// `None` — that is what keeps `!album?` meaningful and what makes the
    /// format language's `[{album}]` group vanish rather than print a label.
    #[test]
    fn a_track_with_no_album_files_under_singles_but_keeps_an_empty_tag() {
        let t = Track {
            path: std::path::PathBuf::from("/m/Singles/Mala - Changes.mp3"),
            artist: Some("Mala".into()),
            ..Default::default()
        };
        assert_eq!(t.album_or_unknown(), NO_ALBUM);
        assert_eq!(t.album, None, "filing must not invent a tag");
    }

    /// And a real album is untouched by any of it.
    #[test]
    fn a_real_album_files_under_its_own_name() {
        let t = Track {
            album: Some("London Zoo".into()),
            ..Default::default()
        };
        assert_eq!(t.album_or_unknown(), "London Zoo");
    }

    use super::*;

    /// **The two-arena hazard is a type error now.**
    ///
    /// This type's doc used to say it out loud: two arenas exist, `Scan` and
    /// `model::arena::Arena`, their handles were the same type, and a `Scan`
    /// index passed to a fresh `Arena` would *resolve* rather than be refused
    /// — silently naming a different entity. It was written down because
    /// nothing did it yet.
    ///
    /// **The hazard is gone rather than guarded** (#86). A row id is an `Id`,
    /// not a handle into anything, so there is no index to hand to the wrong
    /// arena. What is worth asserting instead is the property that replaced
    /// it: the id is derived from the bytes, so it is the same id every time
    /// those bytes are seen — by this process, by the next one, and on
    /// another machine.
    #[test]
    fn a_row_id_is_the_same_every_time_the_bytes_are() {
        let mut one = Scan::new();
        let mut two = Scan::new();
        // Inserted in a different order, so nothing about position can agree
        // by accident.
        one.upsert(track("/a.mp3"));
        let a = one.upsert(track("/b.mp3"));
        two.upsert(track("/b.mp3"));
        let b = two.upsert(track("/b.mp3"));
        assert_eq!(a, b);
        assert!(a.is_some());
    }

    /// **The cost of naming a row by its bytes, stated as a test.**
    ///
    /// Two files with identical audio are one encoding and therefore one row.
    /// Measured on the real library that is one pair, and the row is the
    /// truthful one: you hold one encoding, stored at two paths.
    /// `HoldingsAsserted` is a map keyed by encoding, so the model could not
    /// have said otherwise in any case.
    #[test]
    fn two_paths_with_identical_bytes_are_one_row() {
        let mut s = Scan::new();
        let a = s.upsert(with_bytes("/one.mp3", "same"));
        let b = s.upsert(with_bytes("/two.mp3", "same"));
        assert_eq!(a, b, "one encoding, one row");
        assert_eq!(s.iter().count(), 1, "and one row drawn");
        // Both paths still resolve, because both files are still there.
        assert_eq!(s.id_for_path(Path::new("/one.mp3")), a);
        assert_eq!(s.id_for_path(Path::new("/two.mp3")), b);
    }

    /// And dropping one copy does not unlist the other.
    #[test]
    fn removing_one_of_two_identical_files_keeps_the_row() {
        let mut s = Scan::new();
        s.upsert(with_bytes("/one.mp3", "same"));
        let id = s.upsert(with_bytes("/two.mp3", "same"));
        s.remove_path(Path::new("/two.mp3"));
        assert!(
            s.get(id.unwrap()).is_some(),
            "the other copy still holds these bytes"
        );
    }

    /// A file whose audio will not read has no encoding, so it has no row.
    ///
    /// It is still stored, so a rescan does not read it again, and it is not
    /// playable either — the same read fails.
    #[test]
    fn a_file_whose_bytes_would_not_read_is_not_a_row() {
        let mut s = Scan::new();
        let unreadable = Track {
            path: PathBuf::from("/broken.mp3"),
            ..Default::default()
        };
        assert_eq!(s.upsert(unreadable), None);
        assert_eq!(s.iter().count(), 0);
        assert_eq!(s.id_for_path(Path::new("/broken.mp3")), None);
    }

    /// A file, with bytes of its own.
    ///
    /// The audio id is what a row is named by now, so a fixture without one is
    /// a file the store can hold and never list. Derived from the path here
    /// because distinct paths in these tests mean distinct files — the tests
    /// that mean *the same bytes at two paths* say so explicitly.
    fn track(path: &str) -> Track {
        heard(Track {
            path: PathBuf::from(path),
            ..Default::default()
        })
    }

    /// A file at `path` whose audio is whatever `bytes` names.
    fn with_bytes(path: &str, bytes: &str) -> Track {
        Track {
            path: PathBuf::from(path),
            audio_id: Some(crate::audio_id::AudioId {
                scheme: 1,
                hash: u128::from(bytes.bytes().fold(0u64, |a, b| a * 31 + u64::from(b))),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn upsert_is_stable_for_a_path() {
        let mut s = Scan::new();
        let a = s.upsert(track("/a.mp3"));
        let b = s.upsert(track("/b.mp3"));
        assert_ne!(a, b);

        // Re-scanning the same file must not mint a new id, or playlists
        // holding it would silently point at nothing.
        let mut updated = track("/a.mp3");
        updated.title = Some("changed".into());
        let again = s.upsert(updated);
        assert_eq!(a, again);
        assert_eq!(s.len(), 2);
        assert_eq!(s.get(a.unwrap()).unwrap().title.as_deref(), Some("changed"));
    }

    #[test]
    fn removal_tombstones_rather_than_shifting() {
        let mut s = Scan::new();
        let a = s.upsert(track("/a.mp3"));
        let b = s.upsert(track("/b.mp3"));
        s.remove_path(Path::new("/a.mp3"));

        assert_eq!(s.len(), 1);
        assert_eq!(s.tombstones(), 1);
        assert!(s.get(a.unwrap()).is_none());
        // b's id survives the removal of a — the whole point.
        assert_eq!(s.get(b.unwrap()).unwrap().path, PathBuf::from("/b.mp3"));
        assert_eq!(s.iter().count(), 1);
    }

    /// **Compaction has nothing to report, and that is the point.**
    ///
    /// It used to return `old id -> new id`, because rebuilding the arena
    /// renumbered every row and each holder had to be walked. A row id is
    /// derived from the file's bytes now, so compaction moves the arena
    /// underneath and the ids do not move at all.
    #[test]
    fn compaction_moves_the_arena_and_not_the_ids() {
        let mut s = Scan::new();
        s.upsert(track("/a.mp3"));
        let b = s.upsert(track("/b.mp3"));
        let c = s.upsert(track("/c.mp3"));
        s.remove_path(Path::new("/a.mp3"));

        s.compact();
        assert_eq!(s.tombstones(), 0);
        assert_eq!(s.capacity(), 2, "the arena shrank");
        assert_eq!(
            s.get(b.unwrap()).unwrap().path,
            PathBuf::from("/b.mp3"),
            "and the id still finds it"
        );
        assert_eq!(s.id_for_path(Path::new("/c.mp3")), c);
    }

    #[test]
    fn file_under_prefers_albumartist() {
        let mut t = track("/x.mp3");
        assert_eq!(t.file_under(), UNKNOWN_ARTIST);
        t.artist = Some("Anderson Paak, ScHoolboy Q".into());
        assert_eq!(t.file_under(), "Anderson Paak, ScHoolboy Q");
        // A guest credit must not split the album off from its artist.
        t.albumartist = Some("Anderson .Paak".into());
        assert_eq!(t.file_under(), "Anderson .Paak");
    }

    #[test]
    fn display_title_falls_back_to_the_file_name() {
        let t = track("/music/01 Untitled.mp3");
        assert_eq!(t.display_title(), "01 Untitled.mp3");
    }
}
