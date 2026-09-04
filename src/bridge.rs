//! From what the scanner read to what the librarian holds — issue #77.
//!
//! **Transitional, and it dies with `Store` (#86).** The player needs a
//! `Librarian` to read through before `Store` can go, and `Store` cannot go
//! until the player reads through a `Librarian`. This is the plank across that
//! gap: the scanner still fills a store, and this turns a store into the
//! catalogue the librarian projects from.
//!
//! ## Why not simply scan twice
//!
//! `populate` reads every file and would answer this exactly, at fifteen
//! seconds a launch on top of the seventeen the scan already costs. But
//! `Track` already carries everything `Scanned` wants — the scanner read the
//! tags and hashed the audio on the way past — so the second pass would be
//! re-reading 6500 files to learn what the first pass already knew.
//!
//! ## Nothing here is ever written
//!
//! The instance this builds is **scratch**. It is folded once for the
//! projection and dropped, and it never sees `flush`.
//!
//! That is not a shortcut, it is the rule: a scan reconciles what is on this
//! disk, and `library-api.md` says catalogue facts arrive by `import`. If a
//! launch appended `RecordingCatalogued` for every file it saw, the log would
//! grow by the size of the library every time gmuse started, recording as
//! history what a rescan can produce from scratch.
//!
//! ## What is lost in the crossing, and it is one thing
//!
//! `Track` has no MusicBrainz **release group** id, so albums here derive
//! their identity from the album artist and title rather than converging on an
//! upstream id. Two devices still agree — the derivation is the same on both —
//! but a retitled album mints a new one. Adding the field to a type that is
//! being deleted would be churn; `populate` reads it properly, and this goes
//! away when it takes over.

use std::collections::BTreeSet;

use crate::model::basis::Basis;
use crate::model::encoding::Format;
use crate::model::entity::Device;
use crate::model::id::Id;
use crate::model::instance::{Instance, Scanned};
use crate::model::log::Record;
use crate::model::seam::Librarian;
use crate::store::Store;

/// What the scanner found, in the shape the ingest ladder takes.
///
/// A file with no `audio_id` is left out: it has no identity, so there is
/// nothing to catalogue it as. That is the same call `populate` makes, and it
/// is a file gmuse could not read as audio at all rather than one with bad
/// tags.
fn scanned(store: &Store) -> Vec<Scanned> {
    store
        .iter()
        .filter_map(|(_, t)| {
            Some(Scanned {
                audio: t.audio_id?,
                title: t.display_title().to_string(),
                artist: t.artist.clone().unwrap_or_default(),
                identity: t.mb_recording.map(Basis::MusicBrainz),
                // `Track` carries no artist id, so an artist crossing this
                // plank is minted from its name and a guest credit still
                // splits it. `populate` reads the tag properly; this goes away
                // with `Store` (#86).
                artist_mbid: None,
                album_artist_mbid: None,
                album: t.album.clone(),
                album_artist: t.albumartist.clone(),
                disc: t.discnumber.map(|d| d as u16),
                track: t.tracknumber.map(|n| n as u16),
                mb_release_group: None,
                mb_release: None,
                length_ms: t.duration.as_millis() as u32,
                format: format_of(t),
                bitrate: t.bitrate.unwrap_or(0),
            })
        })
        .collect()
}

/// The container, from the file's extension.
///
/// The scanner does not record it, and the extension is what the decoder went
/// by anyway. An unknown extension is `None` rather than a guess — a verdict
/// on a format nobody identified would be a verdict on nothing.
fn format_of(track: &crate::store::Track) -> Option<Format> {
    let ext = track.path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "flac" => Format::Flac,
        "m4a" | "alac" => Format::Alac,
        "wav" | "aiff" | "aif" => Format::Wav,
        "mp3" => Format::Mp3,
        "aac" => Format::Aac,
        "ogg" | "oga" => Format::Vorbis,
        "opus" => Format::Opus,
        _ => return None,
    })
}

/// The library the player reads, assembled from this disk and this log.
///
/// Two sources, one fold. The scan says what is *here*; the log says what has
/// been decided about it — ratings, plays, when it was acquired. Folding them
/// together is what makes a row carry both, and doing it through the same
/// `Instance` the model uses everywhere else means the ladder that chooses a
/// recording's identity lives in exactly one place.
pub fn librarian(
    store: &Store,
    records: &BTreeSet<Record>,
    device: Id<Device>,
    now: i64,
) -> Librarian {
    // The log first: `ingest` will not mint over a binding it can see, and a
    // scan into an empty instance can see none. See `query::rows`.
    let mut scratch = Instance::new(device);
    scratch.merge(now, records);
    scratch.ingest(now, &scanned(store));
    Librarian::from_catalogue(&scratch.catalogue())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_id::AudioId;
    use crate::model::entity::Recording;
    use crate::store::Track;
    use std::path::PathBuf;
    use std::time::Duration;

    fn track(path: &str, title: &str, artist: &str, hash: u128) -> Track {
        Track {
            path: PathBuf::from(path),
            artist: Some(artist.into()),
            albumartist: Some(artist.into()),
            album: Some("An Album".into()),
            title: Some(title.into()),
            tracknumber: Some(1),
            duration: Duration::from_secs(200),
            bitrate: Some(320),
            audio_id: Some(AudioId { scheme: 1, hash }),
            ..Default::default()
        }
    }

    fn device() -> Id<Device> {
        Id::derived(b"test")
    }

    /// A file on disk becomes a row the player can draw.
    #[test]
    fn a_scanned_file_becomes_a_row() {
        let mut store = Store::new();
        store.upsert(track("/m/a.mp3", "Callisto", "Stephan Bodzin", 1));
        let l = librarian(&store, &BTreeSet::new(), device(), 1_000);

        let recording = l.recordings().next().expect("one recording");
        let row = l.row(recording).expect("a row");
        assert_eq!(row.title, "Callisto");
        assert_eq!(row.file_under, "Stephan Bodzin");
        assert_eq!(row.album.as_deref(), Some("An Album"));
        assert_eq!(row.format, Some(Format::Mp3), "read from the extension");
        assert_eq!(row.bitrate, Some(320));
        assert!(row.available, "a file on this disk is available");
    }

    /// **The whole point of folding both sources.** The scan knows the track
    /// exists; only the log knows it is worth four stars, and a row has to
    /// carry both or the player still needs two places to look.
    #[test]
    fn the_log_supplies_what_the_disk_cannot() {
        let mut store = Store::new();
        store.upsert(track("/m/a.mp3", "Callisto", "Stephan Bodzin", 1));

        // What the recording will be, derived the same way the ladder does.
        let scratch_l = librarian(&store, &BTreeSet::new(), device(), 1_000);
        let recording: Id<Recording> = scratch_l.recordings().next().unwrap();
        assert_eq!(scratch_l.row(recording).unwrap().rating, None);

        let mut writer = Instance::new(device());
        writer.rate(2_000, recording, Some(4));
        writer.play(2_100, recording, Id::derived(b"whatever"));

        let l = librarian(&store, writer.records(), device(), 3_000);
        let row = l.row(recording).expect("still a row");
        assert_eq!(row.rating, Some(4), "the rating did not survive the fold");
        assert_eq!(row.plays, 1);
    }

    /// **A recording has no length; its encodings do.**
    ///
    /// An edit, a radio cut and a truncated rip are one recording and three
    /// durations. A row reports the copy it would open, because "did I hear
    /// enough of it" is a question about the file that played — and ranking
    /// the copies to pick one was a rule invented to fill a field that should
    /// not have been filled. Under it, a twelve-second rip measured against a
    /// four-minute sibling could never have earned a play at all.
    #[test]
    fn a_row_reports_the_length_of_the_copy_it_would_open() {
        let mut store = Store::new();
        let mbid = uuid::Uuid::from_u128(0x1831_05b5);

        // One recording, one copy here, and it is the short one.
        let mut partial = track("/m/partial.mp3", "Callisto", "Stephan Bodzin", 2);
        partial.mb_recording = Some(mbid);
        partial.duration = Duration::from_secs(12);
        store.upsert(partial);

        let l = librarian(&store, &BTreeSet::new(), device(), 1_000);
        let recording = l.recordings().next().expect("one recording");
        assert_eq!(
            l.row(recording).unwrap().length,
            Duration::from_secs(12),
            "the row reported something other than the copy it holds"
        );
    }

    /// A file with no readable audio has no identity, so there is nothing to    /// A file with no readable audio has no identity, so there is nothing to
    /// catalogue it as. Left out rather than invented.
    #[test]
    fn a_file_with_no_audio_id_is_left_out() {
        let mut store = Store::new();
        let mut orphan = track("/m/orphan.mp3", "Unknown", "Nobody", 1);
        orphan.audio_id = None;
        store.upsert(orphan);
        store.upsert(track("/m/a.mp3", "Callisto", "Stephan Bodzin", 2));

        let l = librarian(&store, &BTreeSet::new(), device(), 1_000);
        assert_eq!(l.recordings().count(), 1);
    }

    /// Two files, identical audio, one recording — and the row still draws.
    /// The library has a real album duplicated in place under two naming
    /// conventions, and it must not appear twice.
    #[test]
    fn one_recording_however_many_copies() {
        let mut store = Store::new();
        store.upsert(track("/m/01 Callisto.mp3", "Callisto", "Stephan Bodzin", 1));
        store.upsert(track(
            "/m/1 - Callisto.mp3",
            "Callisto",
            "Stephan Bodzin",
            1,
        ));

        let l = librarian(&store, &BTreeSet::new(), device(), 1_000);
        assert_eq!(
            l.recordings().count(),
            1,
            "the duplicate became a second row"
        );
    }
}
