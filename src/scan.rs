//! Library scanning: walk the filesystem, read tags, update the `Store`.
//!
//! Rescans are incremental by mtime — a file whose mtime is unchanged is not
//! reopened. On a library of any size the second scan is a `stat` per file and
//! nothing else, which is what makes a scan-on-startup tolerable.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

use lofty::config::ParseOptions;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::prelude::{Accessor, ItemKey};
use lofty::probe::Probe;

use crate::store::{ReplayGain, Store, Track};

/// Extensions we will try to read. Anything else in the tree is ignored
/// without opening it, which matters when a music folder also holds artwork,
/// playlists and stray video.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "ogg", "opus", "m4a", "mp4", "aac", "alac", "wav", "wave", "aiff", "aif", "ape",
    "wv", "mpc",
];

/// A file recognised at a new path rather than treated as a deletion and an
/// unrelated arrival.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Move {
    pub from: PathBuf,
    pub to: PathBuf,
}

#[derive(Debug, Default, Clone)]
pub struct ScanReport {
    pub added: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub removed: usize,
    /// Departures paired with arrivals. Counted in `added` and `removed` too,
    /// because that is what happened to the `Store`; what makes them moves is
    /// that everything keyed by path can follow them.
    pub moves: Vec<Move>,
    /// Files that looked like audio but could not be read.
    pub errors: Vec<(PathBuf, String)>,
    pub elapsed: Duration,
}

impl ScanReport {
    pub fn total(&self) -> usize {
        self.added + self.updated + self.unchanged
    }
}

impl std::fmt::Display for ScanReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} tracks ({} added, {} updated, {} unchanged, {} removed, {} errors) in {:.2}s{}",
            self.total(),
            self.added,
            self.updated,
            self.unchanged,
            self.removed,
            self.errors.len(),
            self.elapsed.as_secs_f64(),
            // Said out loud: a silent reassociation would be as bad as the
            // silent loss it replaces, and worse if it ever guesses wrong.
            match self.moves.len() {
                0 => String::new(),
                1 => "\n  1 track moved, and its history moved with it".to_string(),
                n => format!("\n  {n} tracks moved, and their history moved with them"),
            }
        )
    }
}

/// Scan `roots` into `store`, incrementally.
///
/// Tracks under a root that no longer exist on disk are removed. Tracks
/// outside every root are left alone, so scanning one folder does not evict a
/// library built from another.
pub fn scan(store: &mut Store, roots: &[PathBuf]) -> ScanReport {
    let start = Instant::now();
    let mut report = ScanReport::default();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    // Paths that were not in the store before this pass. Half of what move
    // detection needs; the other half is what went away.
    let mut arrived: Vec<PathBuf> = Vec::new();

    for root in roots {
        for entry in walkdir::WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
        {
            if !entry.file_type().is_file() || !is_audio(entry.path()) {
                continue;
            }
            let path = entry.path().to_path_buf();
            let meta = entry.metadata().ok();
            let mtime = meta
                .as_ref()
                .and_then(|m| m.modified().ok())
                .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);

            seen.insert(path.clone());

            // The incremental fast path: same file, same mtime, don't reopen.
            if let Some(id) = store.id_for_path(&path)
                && let Some(existing) = store.get(id)
                && existing.mtime == mtime
            {
                report.unchanged += 1;
                continue;
            }

            let existed = store.id_for_path(&path).is_some();
            match read_track(&path, mtime, size) {
                Ok(track) => {
                    store.upsert(track);
                    if existed {
                        report.updated += 1;
                    } else {
                        arrived.push(path.clone());
                        report.added += 1;
                    }
                }
                Err(e) => report.errors.push((path, e)),
            }
        }
    }

    // Anything under a root we did not see has gone away.
    let gone: Vec<PathBuf> = store
        .iter()
        .filter(|(_, t)| !seen.contains(&t.path) && roots.iter().any(|r| t.path.starts_with(r)))
        .map(|(_, t)| t.path.clone())
        .collect();

    // Pair the two halves up before the departures are destroyed: a file that
    // left one path and arrived at another is the same track, and everything
    // durable is keyed by path (#25).
    report.moves = detect_moves(store, &gone, &arrived);

    for path in gone {
        store.remove_path(&path);
        report.removed += 1;
    }

    report.elapsed = start.elapsed();
    report
}

/// What identifies a track independently of where it sits.
///
/// Everything describing the audio and its tags — but **not** the mtime. A
/// plain `mv` preserves it and requiring it would be stricter, but a copy
/// without `-p`, an rsync without `-a`, or a restore from a backup all reset
/// it, and those are exactly the reorganisations worth surviving.
///
/// `size` carries most of the weight: for a byte-identical move it is
/// conclusive, and it is why `Track` gained the field.
#[derive(Hash, PartialEq, Eq)]
struct Signature<'a> {
    size: u64,
    duration: Duration,
    bitrate: Option<u32>,
    samplerate: Option<u32>,
    channels: Option<u8>,
    tracknumber: Option<u32>,
    discnumber: Option<u32>,
    title: Option<&'a str>,
    artist: Option<&'a str>,
    album: Option<&'a str>,
}

fn signature(t: &Track) -> Signature<'_> {
    Signature {
        size: t.size,
        duration: t.duration,
        bitrate: t.bitrate,
        samplerate: t.samplerate,
        channels: t.channels,
        tracknumber: t.tracknumber,
        discnumber: t.discnumber,
        title: t.title.as_deref(),
        artist: t.artist.as_deref(),
        album: t.album.as_deref(),
    }
}

/// Pair departures with arrivals.
///
/// **Only when the signature is unique on both sides.** Two copies of one
/// track, or two untagged files of the same length, would otherwise be paired
/// arbitrarily — and a wrong pairing attaches somebody's rating to the wrong
/// song, which is worse than the loss this is preventing. Ambiguity is
/// declined rather than guessed.
fn detect_moves(store: &Store, gone: &[PathBuf], arrived: &[PathBuf]) -> Vec<Move> {
    if gone.is_empty() || arrived.is_empty() {
        return Vec::new();
    }

    let mut departures: std::collections::HashMap<Signature<'_>, Option<&PathBuf>> =
        std::collections::HashMap::new();
    for path in gone {
        let Some(t) = store.id_for_path(path).and_then(|id| store.get(id)) else {
            continue;
        };
        // `None` marks a signature seen more than once, so it stays ambiguous
        // even if a third arrives later.
        departures
            .entry(signature(t))
            .and_modify(|slot| *slot = None)
            .or_insert(Some(path));
    }

    let mut counts: std::collections::HashMap<Signature<'_>, usize> =
        std::collections::HashMap::new();
    for path in arrived {
        if let Some(t) = store.id_for_path(path).and_then(|id| store.get(id)) {
            *counts.entry(signature(t)).or_insert(0) += 1;
        }
    }

    let mut moves = Vec::new();
    for path in arrived {
        let Some(t) = store.id_for_path(path).and_then(|id| store.get(id)) else {
            continue;
        };
        let sig = signature(t);
        if counts.get(&sig) != Some(&1) {
            continue;
        }
        if let Some(Some(from)) = departures.get(&sig) {
            moves.push(Move {
                from: (*from).clone(),
                to: path.clone(),
            });
        }
    }
    moves
}

fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let e = e.to_ascii_lowercase();
            AUDIO_EXTENSIONS.contains(&e.as_str())
        })
        .unwrap_or(false)
}

/// Read one file's tags and audio properties.
///
/// Missing tags are normal, not errors: an untagged file still belongs in the
/// library, filed under "Unknown Artist" and shown by file name.
pub fn read_track(path: &Path, mtime: i64, size: u64) -> Result<Track, String> {
    let tagged = Probe::open(path)
        .map_err(|e| e.to_string())?
        // Guess from content as well as extension: a mislabelled file is
        // common enough in a real library that trusting the extension alone
        // loses tracks.
        .guess_file_type()
        .map_err(|e| e.to_string())?
        .options(ParseOptions::new())
        .read()
        .map_err(|e| e.to_string())?;

    let props = tagged.properties();
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());

    let mut track = Track {
        path: path.to_path_buf(),
        size,
        duration: props.duration(),
        bitrate: props.audio_bitrate().or_else(|| props.overall_bitrate()),
        samplerate: props.sample_rate(),
        channels: props.channels(),
        mtime,
        ..Default::default()
    };

    if let Some(tag) = tag {
        track.artist = clean(tag.artist().map(|s| s.to_string()));
        track.album = clean(tag.album().map(|s| s.to_string()));
        track.title = clean(tag.title().map(|s| s.to_string()));
        track.genre = clean(tag.genre().map(|s| s.to_string()));
        track.albumartist = clean(tag.get_string(ItemKey::AlbumArtist).map(str::to_string));
        track.tracknumber = tag.track();
        track.discnumber = tag.disk();
        track.date = tag
            .get_string(ItemKey::RecordingDate)
            .or_else(|| tag.get_string(ItemKey::Year))
            .and_then(parse_year);

        let rg = ReplayGain {
            track_gain: tag
                .get_string(ItemKey::ReplayGainTrackGain)
                .and_then(parse_db),
            track_peak: tag
                .get_string(ItemKey::ReplayGainTrackPeak)
                .and_then(|s| s.trim().parse().ok()),
            album_gain: tag
                .get_string(ItemKey::ReplayGainAlbumGain)
                .and_then(parse_db),
            album_peak: tag
                .get_string(ItemKey::ReplayGainAlbumPeak)
                .and_then(|s| s.trim().parse().ok()),
        };
        track.replaygain = (!rg.is_empty()).then_some(rg);
    }

    Ok(track)
}

/// Trim, drop empties, and strip embedded NULs.
///
/// ID3v2 in the wild contains NUL-padded and NUL-separated strings; leaving
/// them in produces text that renders as a blank or truncates a terminal line.
fn clean(s: Option<String>) -> Option<String> {
    let s = s?;
    let s = s.replace('\0', " ");
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// Pull a four-digit year out of whatever the date field holds — `1997`,
/// `1997-03-21`, `1997/03`, and `(1997)` all appear in real tags.
fn parse_year(s: &str) -> Option<i32> {
    let digits: String = s
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    let year: i32 = digits.parse().ok()?;
    (1000..=9999).contains(&year).then_some(year)
}

/// ReplayGain values are written as `-7.53 dB`.
fn parse_db(s: &str) -> Option<f32> {
    s.trim()
        .trim_end_matches(|c: char| c.is_alphabetic() || c.is_whitespace())
        .trim()
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(path: &str, size: u64, title: &str) -> Track {
        Track {
            path: PathBuf::from(path),
            size,
            title: Some(title.into()),
            artist: Some("An Artist".into()),
            album: Some("An Album".into()),
            tracknumber: Some(3),
            duration: Duration::from_secs(211),
            bitrate: Some(320),
            samplerate: Some(44100),
            channels: Some(2),
            mtime: 1000,
            ..Default::default()
        }
    }

    /// The dominant case: a folder rename, so the bytes and tags are identical
    /// and only the path changed.
    #[test]
    fn a_file_at_a_new_path_is_recognised_as_the_same_track() {
        let mut store = Store::new();
        store.upsert(track("/m/old/03 Song.mp3", 5_000_000, "Song"));
        store.upsert(track("/m/Singles/03 Song.mp3", 5_000_000, "Song"));

        let moves = detect_moves(
            &store,
            &[PathBuf::from("/m/old/03 Song.mp3")],
            &[PathBuf::from("/m/Singles/03 Song.mp3")],
        );
        assert_eq!(
            moves,
            vec![Move {
                from: PathBuf::from("/m/old/03 Song.mp3"),
                to: PathBuf::from("/m/Singles/03 Song.mp3"),
            }]
        );
    }

    /// mtime is deliberately *not* in the signature: a copy without `-p`, an
    /// rsync without `-a`, and a restore from backup all reset it, and those
    /// are exactly the reorganisations worth surviving.
    #[test]
    fn a_move_that_reset_the_mtime_is_still_a_move() {
        let mut store = Store::new();
        store.upsert(track("/m/old/a.mp3", 5_000_000, "Song"));
        let mut arrived = track("/m/new/a.mp3", 5_000_000, "Song");
        arrived.mtime = 9_999_999;
        store.upsert(arrived);

        let moves = detect_moves(
            &store,
            &[PathBuf::from("/m/old/a.mp3")],
            &[PathBuf::from("/m/new/a.mp3")],
        );
        assert_eq!(moves.len(), 1);
    }

    #[test]
    fn size_alone_distinguishes_two_otherwise_identical_tracks() {
        let mut store = Store::new();
        store.upsert(track("/m/old/a.mp3", 5_000_000, "Song"));
        store.upsert(track("/m/new/a.mp3", 4_000_000, "Song"));

        assert!(
            detect_moves(
                &store,
                &[PathBuf::from("/m/old/a.mp3")],
                &[PathBuf::from("/m/new/a.mp3")]
            )
            .is_empty()
        );
    }

    #[test]
    fn a_retagged_file_is_not_a_move() {
        let mut store = Store::new();
        store.upsert(track("/m/old/a.mp3", 5_000_000, "Song"));
        store.upsert(track("/m/new/a.mp3", 5_000_000, "A Different Song"));

        assert!(
            detect_moves(
                &store,
                &[PathBuf::from("/m/old/a.mp3")],
                &[PathBuf::from("/m/new/a.mp3")]
            )
            .is_empty()
        );
    }

    /// The dangerous case. Pairing arbitrarily would attach a rating to the
    /// wrong song, which is worse than the loss this is preventing.
    #[test]
    fn two_identical_departures_are_ambiguous_and_declined() {
        let mut store = Store::new();
        store.upsert(track("/m/one/a.mp3", 5_000_000, "Song"));
        store.upsert(track("/m/two/a.mp3", 5_000_000, "Song"));
        store.upsert(track("/m/new/a.mp3", 5_000_000, "Song"));

        assert!(
            detect_moves(
                &store,
                &[PathBuf::from("/m/one/a.mp3"), PathBuf::from("/m/two/a.mp3")],
                &[PathBuf::from("/m/new/a.mp3")]
            )
            .is_empty(),
            "an ambiguous departure was paired anyway"
        );
    }

    #[test]
    fn two_identical_arrivals_are_ambiguous_and_declined() {
        let mut store = Store::new();
        store.upsert(track("/m/old/a.mp3", 5_000_000, "Song"));
        store.upsert(track("/m/one/a.mp3", 5_000_000, "Song"));
        store.upsert(track("/m/two/a.mp3", 5_000_000, "Song"));

        assert!(
            detect_moves(
                &store,
                &[PathBuf::from("/m/old/a.mp3")],
                &[PathBuf::from("/m/one/a.mp3"), PathBuf::from("/m/two/a.mp3")]
            )
            .is_empty(),
            "an ambiguous arrival was paired anyway"
        );
    }

    /// A real addition beside a real move: the move is found and the newcomer
    /// is left alone.
    #[test]
    fn a_genuine_addition_alongside_a_move_is_not_swept_up() {
        let mut store = Store::new();
        store.upsert(track("/m/old/a.mp3", 5_000_000, "Song"));
        store.upsert(track("/m/new/a.mp3", 5_000_000, "Song"));
        store.upsert(track("/m/new/b.mp3", 7_000_000, "Another"));

        let moves = detect_moves(
            &store,
            &[PathBuf::from("/m/old/a.mp3")],
            &[PathBuf::from("/m/new/a.mp3"), PathBuf::from("/m/new/b.mp3")],
        );
        assert_eq!(moves.len(), 1);
        assert_eq!(moves[0].to, PathBuf::from("/m/new/a.mp3"));
    }

    #[test]
    fn nothing_gone_or_nothing_arrived_means_no_moves() {
        let mut store = Store::new();
        store.upsert(track("/m/new/a.mp3", 5_000_000, "Song"));
        assert!(detect_moves(&store, &[], &[PathBuf::from("/m/new/a.mp3")]).is_empty());
        assert!(detect_moves(&store, &[PathBuf::from("/m/old/a.mp3")], &[]).is_empty());
    }

    /// A reassociation the listener never sees is as bad as the silent loss it
    /// replaces — worse, if it ever guesses wrong.
    #[test]
    fn the_report_says_when_tracks_moved() {
        let mut r = ScanReport::default();
        // Not `contains("moved")` — "0 removed" contains it.
        assert!(
            !r.to_string().contains('\n'),
            "no moves must add no second line"
        );
        r.moves.push(Move {
            from: PathBuf::from("/a"),
            to: PathBuf::from("/b"),
        });
        assert!(r.to_string().contains("1 track moved"), "{r}");
        r.moves.push(Move {
            from: PathBuf::from("/c"),
            to: PathBuf::from("/d"),
        });
        assert!(r.to_string().contains("2 tracks moved"), "{r}");
    }

    #[test]
    fn years_come_out_of_messy_dates() {
        assert_eq!(parse_year("1997"), Some(1997));
        assert_eq!(parse_year("1997-03-21"), Some(1997));
        assert_eq!(parse_year("1997/03/21"), Some(1997));
        assert_eq!(parse_year("(1997)"), Some(1997));
        assert_eq!(parse_year("recorded 2016 live"), Some(2016));
        assert_eq!(parse_year(""), None);
        assert_eq!(parse_year("unknown"), None);
        // Two digits is not a year we can trust.
        assert_eq!(parse_year("97"), None);
    }

    #[test]
    fn replaygain_units_are_stripped() {
        assert_eq!(parse_db("-7.53 dB"), Some(-7.53));
        assert_eq!(parse_db("  +2.10dB "), Some(2.10));
        assert_eq!(parse_db("0.00"), Some(0.0));
        assert_eq!(parse_db("loud"), None);
    }

    #[test]
    fn strings_are_cleaned() {
        assert_eq!(clean(Some("  Burial ".into())), Some("Burial".into()));
        assert_eq!(clean(Some("".into())), None);
        assert_eq!(clean(Some("   ".into())), None);
        // Embedded NULs are common in ID3v2 and render as blanks.
        assert_eq!(clean(Some("A\0B".into())), Some("A B".into()));
        assert_eq!(clean(Some("Name\0".into())), Some("Name".into()));
        assert_eq!(clean(None), None);
    }

    #[test]
    fn extension_filter_is_case_insensitive() {
        assert!(is_audio(Path::new("/a/b.MP3")));
        assert!(is_audio(Path::new("/a/b.opus")));
        assert!(is_audio(Path::new("/a/b.FLAC")));
        assert!(!is_audio(Path::new("/a/cover.jpg")));
        assert!(!is_audio(Path::new("/a/notes.txt")));
        assert!(!is_audio(Path::new("/a/noext")));
    }
}
