//! The track arena — DESIGN §4 (INVARIANT).
//!
//! One store; every view is a projection over it. Track identity is an integer
//! index into an arena, never a pointer or an `Rc`. That keeps the whole model
//! `Send`, makes views cheap to rebuild, and lets the same track appear in the
//! library, several playlists and the queue as the same `u32`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// A handle to a track. Stable for the life of the store.
///
/// Never derived from a path at read time — look it up once and keep the id.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TrackId(pub u32);

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
/// keys belong to the `Library` projection, not here.
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
    pub genre: Option<String>,
    pub duration: Duration,
    pub bitrate: Option<u32>,
    pub samplerate: Option<u32>,
    pub channels: Option<u8>,
    /// Seconds since the epoch, for incremental rescan.
    pub mtime: i64,
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
/// `tracks[n]` is `TrackId(n)`. Removal leaves a `None` tombstone rather than
/// shifting, because shifting would invalidate every `TrackId` held in a
/// playlist or the queue (DESIGN §4).
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Store {
    tracks: Vec<Option<Track>>,
    by_path: HashMap<PathBuf, TrackId>,
    /// Count of tombstones, so callers can decide when a compaction is worth it.
    tombstones: usize,
}

impl Store {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of live tracks.
    pub fn len(&self) -> usize {
        self.tracks.len() - self.tombstones
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Size of the id space, including tombstones.
    pub fn capacity(&self) -> usize {
        self.tracks.len()
    }

    pub fn tombstones(&self) -> usize {
        self.tombstones
    }

    pub fn get(&self, id: TrackId) -> Option<&Track> {
        self.tracks.get(id.0 as usize)?.as_ref()
    }

    pub fn id_for_path(&self, path: &Path) -> Option<TrackId> {
        self.by_path.get(path).copied()
    }

    /// Live tracks, with their ids.
    pub fn iter(&self) -> impl Iterator<Item = (TrackId, &Track)> {
        self.tracks
            .iter()
            .enumerate()
            .filter_map(|(i, t)| t.as_ref().map(|t| (TrackId(i as u32), t)))
    }

    /// Insert a new track, or update the one already at that path.
    ///
    /// Updating in place is what keeps a rescan from invalidating ids: a file
    /// whose tags changed keeps the id a playlist is holding.
    pub fn upsert(&mut self, track: Track) -> TrackId {
        if let Some(id) = self.by_path.get(&track.path).copied() {
            self.tracks[id.0 as usize] = Some(track);
            return id;
        }
        let id = TrackId(self.tracks.len() as u32);
        self.by_path.insert(track.path.clone(), id);
        self.tracks.push(Some(track));
        id
    }

    /// Tombstone the track at `path`, if present.
    pub fn remove_path(&mut self, path: &Path) -> Option<TrackId> {
        let id = self.by_path.remove(path)?;
        if let Some(slot) = self.tracks.get_mut(id.0 as usize)
            && slot.take().is_some()
        {
            self.tombstones += 1;
        }
        Some(id)
    }

    /// Drop tombstones and reassign ids, returning `old id -> new id`.
    ///
    /// A rebuild, not an index shift: every holder of a `TrackId` must be
    /// remapped through the returned table, so this is deliberately explicit
    /// rather than something a rescan does behind your back.
    ///
    /// Nothing calls this yet — there is no playlist or queue to remap until
    /// M4/M5. It exists now because §4 specifies removal as "a tombstone or a
    /// rebuild", and the rebuild half is far easier to get right (and test)
    /// alongside the tombstone half than bolted on later.
    #[allow(dead_code)]
    pub fn compact(&mut self) -> HashMap<TrackId, TrackId> {
        let mut remap = HashMap::with_capacity(self.len());
        let mut fresh = Vec::with_capacity(self.len());
        for (i, slot) in self.tracks.drain(..).enumerate() {
            if let Some(track) = slot {
                let new = TrackId(fresh.len() as u32);
                remap.insert(TrackId(i as u32), new);
                fresh.push(Some(track));
            }
        }
        self.tracks = fresh;
        self.tombstones = 0;
        self.by_path.clear();
        let entries: Vec<(PathBuf, TrackId)> = self
            .tracks
            .iter()
            .enumerate()
            .filter_map(|(i, t)| t.as_ref().map(|t| (t.path.clone(), TrackId(i as u32))))
            .collect();
        self.by_path.extend(entries);
        remap
    }
}

impl Store {
    /// A compacted copy for persisting.
    ///
    /// Tombstones exist to keep live `TrackId`s valid within a session; there
    /// is nothing holding an id across a restart, so writing them to the cache
    /// would only grow the file and the id space for ever.
    pub(crate) fn clone_for_save(store: &Store) -> Store {
        let mut out = Store::new();
        for (_, track) in store.iter() {
            out.upsert(track.clone());
        }
        out
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

    fn track(path: &str) -> Track {
        Track {
            path: PathBuf::from(path),
            ..Default::default()
        }
    }

    #[test]
    fn upsert_is_stable_for_a_path() {
        let mut s = Store::new();
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
        assert_eq!(s.get(a).unwrap().title.as_deref(), Some("changed"));
    }

    #[test]
    fn removal_tombstones_rather_than_shifting() {
        let mut s = Store::new();
        let a = s.upsert(track("/a.mp3"));
        let b = s.upsert(track("/b.mp3"));
        s.remove_path(Path::new("/a.mp3"));

        assert_eq!(s.len(), 1);
        assert_eq!(s.tombstones(), 1);
        assert!(s.get(a).is_none());
        // b's id survives the removal of a — the whole point.
        assert_eq!(s.get(b).unwrap().path, PathBuf::from("/b.mp3"));
        assert_eq!(s.iter().count(), 1);
    }

    #[test]
    fn compact_reports_the_remap() {
        let mut s = Store::new();
        let _a = s.upsert(track("/a.mp3"));
        let b = s.upsert(track("/b.mp3"));
        let c = s.upsert(track("/c.mp3"));
        s.remove_path(Path::new("/a.mp3"));

        let remap = s.compact();
        assert_eq!(s.tombstones(), 0);
        assert_eq!(s.capacity(), 2);
        assert_eq!(remap[&b], TrackId(0));
        assert_eq!(remap[&c], TrackId(1));
        assert_eq!(s.get(remap[&b]).unwrap().path, PathBuf::from("/b.mp3"));
        // Lookups still work after ids move.
        assert_eq!(s.id_for_path(Path::new("/c.mp3")), Some(remap[&c]));
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
