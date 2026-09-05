//! Shelf scanning: walk the filesystem, read tags, update the `Scan`.
//!
//! Rescans are incremental by mtime — a file whose mtime is unchanged is not
//! reopened. On a library of any size the second scan is a `stat` per file and
//! nothing else. That is what *used* to make a scan on every startup
//! tolerable; since 2026-09-01 a launch does not scan at all, and this cheap
//! second pass is what makes an explicit `--rescan` feel free.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

use lofty::config::ParseOptions;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::prelude::{Accessor, ItemKey};
use lofty::probe::Probe;

use crate::store::{ReplayGain, Scan, Track};

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
    /// because that is what happened to the `Scan`; what makes them moves is
    /// that everything keyed by path can follow them.
    pub moves: Vec<Move>,
    /// Files that looked like audio but could not be read.
    pub errors: Vec<(PathBuf, String)>,
    /// How many tracks have an identity that survives being moved.
    ///
    /// Reported because it is the number that decides how much of the library
    /// is safe from the failure that stranded a four-star rating: a track with
    /// a key keeps its history through a rename, a renumber and a retag, and a
    /// track without one is still living on its path.
    pub identified: usize,
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
pub fn scan(store: &mut Scan, roots: &[PathBuf]) -> ScanReport {
    let start = Instant::now();
    // A root under another root would have every one of its files walked
    // twice — read twice on a cold scan, and counted twice in the report,
    // which is how a 311-track library came to announce 622 (#21).
    let roots = &prune_nested(roots);
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
    // **Files, not rows.** Two paths holding identical bytes are one row, so
    // walking rows would show only one of them — and the other, unseen, would
    // never be reported as gone. That turned a move into a deletion (#86).
    let gone: Vec<PathBuf> = store
        .files()
        .filter(|t| !seen.contains(&t.path) && roots.iter().any(|r| t.path.starts_with(r)))
        .map(|t| t.path.clone())
        .collect();

    // Pair the two halves up before the departures are destroyed: a file that
    // left one path and arrived at another is the same track, and everything
    // durable is keyed by path (#25).
    report.moves = detect_moves(store, &gone, &arrived);

    for path in gone {
        store.remove_path(&path);
        report.removed += 1;
    }

    report.identified = store
        .iter()
        .filter(|(id, _)| store.history_key(*id).is_some())
        .count();
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

/// What a departure and an arrival are matched on.
///
/// **The audio when it is known, the shape only when it is not.** The shape
/// includes the tags, so a pass that retags *and* moves in one go breaks it —
/// which is why this project carried a rule about doing those separately, and
/// why a renumber stranded three history entries here (#35). `Audio` has no
/// such weakness: the id is computed from the packets and a tagger cannot
/// touch it.
#[derive(PartialEq, Eq, Hash)]
enum Key<'a> {
    Audio(crate::audio_id::AudioId),
    Shape(Signature<'a>),
}

fn key(t: &Track) -> Key<'_> {
    match t.audio_id {
        Some(id) => Key::Audio(id),
        None => Key::Shape(signature(t)),
    }
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
fn detect_moves(store: &Scan, gone: &[PathBuf], arrived: &[PathBuf]) -> Vec<Move> {
    if gone.is_empty() || arrived.is_empty() {
        return Vec::new();
    }

    let mut departures: std::collections::HashMap<Key<'_>, Option<&PathBuf>> =
        std::collections::HashMap::new();
    for path in gone {
        let Some(t) = store.track_at(path) else {
            continue;
        };
        // `None` marks a signature seen more than once, so it stays ambiguous
        // even if a third arrives later.
        departures
            .entry(key(t))
            .and_modify(|slot| *slot = None)
            .or_insert(Some(path));
    }

    let mut counts: std::collections::HashMap<Key<'_>, usize> = std::collections::HashMap::new();
    for path in arrived {
        if let Some(t) = store.track_at(path) {
            *counts.entry(key(t)).or_insert(0) += 1;
        }
    }

    let mut moves = Vec::new();
    for path in arrived {
        let Some(t) = store.track_at(path) else {
            continue;
        };
        let k = key(t);
        if counts.get(&k) != Some(&1) {
            continue;
        }
        if let Some(Some(from)) = departures.get(&k) {
            moves.push(Move {
                from: (*from).clone(),
                to: path.clone(),
            });
        }
    }
    moves
}

/// Drop any root that lies under another, and any exact duplicate.
///
/// `Path::starts_with` compares whole components, so `/a/bc` is not under
/// `/a/b` — which is why this is not string prefix matching.
pub(crate) fn prune_nested(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut kept: Vec<PathBuf> = Vec::new();
    for root in roots {
        // Already covered by something kept — including by itself, which is
        // how duplicates fall out.
        if kept.iter().any(|k| root.starts_with(k)) {
            continue;
        }
        // This one covers things already kept; they are the redundant ones now.
        kept.retain(|k| !k.starts_with(root));
        kept.push(root.clone());
    }
    kept
}

pub(crate) fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let e = e.to_ascii_lowercase();
            AUDIO_EXTENSIONS.contains(&e.as_str())
        })
        .unwrap_or(false)
}

/// Build a track for a file whose tags could not be read.
///
/// The audio is fine — these come from a *tag* parse failure, not a decode one
/// — so the properties are whatever the second read recovered, and the name is
/// whatever the path can say. A track filed under "Unknown Artist" with its
/// file name for a title is the same shape as a genuinely untagged file, which
/// the library already handles; the point is that it is **there**.
fn from_path_alone(
    path: &Path,
    mtime: i64,
    size: u64,
    props: &lofty::properties::FileProperties,
) -> Track {
    Track {
        path: path.to_path_buf(),
        size,
        duration: props.duration(),
        bitrate: props.audio_bitrate().or_else(|| props.overall_bitrate()),
        samplerate: props.sample_rate(),
        channels: props.channels(),
        mtime,
        // Deliberately not guessed from the directory layout. `Artist/Album`
        // holds here and would not hold everywhere, and a wrong artist is worse
        // than an honest "Unknown" — it files the track somewhere you will not
        // think to look. The file name is the one thing the path says for
        // certain about *this* track.
        // A file whose tags will not parse still has readable *audio* — that
        // is the whole basis of the fallback — so it still gets an identity.
        audio_id: crate::audio_id::of(path).ok(),
        title: path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty()),
        ..Default::default()
    }
}

/// Read one file's tags and audio properties.
///
/// Missing tags are normal, not errors: an untagged file still belongs in the
/// library, filed under "Unknown Artist" and shown by file name.
pub fn read_track(path: &Path, mtime: i64, size: u64) -> Result<Track, String> {
    let probe = || {
        Probe::open(path)
            .map_err(|e| e.to_string())?
            // Guess from content as well as extension: a mislabelled file is
            // common enough in a real library that trusting the extension alone
            // loses tracks.
            .guess_file_type()
            .map_err(|e| e.to_string())
    };

    let tagged = match probe()?.options(ParseOptions::new()).read() {
        Ok(tagged) => tagged,
        // **A tag we cannot read must not cost us the track.** Five files here
        // carry an APEv2 tag with one malformed item, and lofty abandons the
        // whole file over it — including the ID3 tag beside it, which is
        // perfectly good, and the audio, which plays. Dropping them left songs
        // missing from the library with nothing on screen to say so.
        //
        // Reading again with tags off recovers everything else: duration,
        // bitrate, sample rate and channels all come back correct. Paired with
        // a title from the file name that is a playable, sortable track rather
        // than a hole.
        Err(tags_err) => {
            let bare = probe()?
                .options(ParseOptions::new().read_tags(false))
                .read()
                .map_err(|_| tags_err.to_string())?;
            return Ok(from_path_alone(path, mtime, size, bare.properties()));
        }
    };

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
        // Computed here and nowhere else: this is the one place the file is
        // already being opened, and the incremental scan means an unchanged
        // file is never reopened — so the whole-library cost is paid once
        // (16s for 5774 files) and a rescan pays a `stat`.
        audio_id: crate::audio_id::of(path).ok(),
        ..Default::default()
    };

    if let Some(tag) = tag {
        // One key, every format: lofty maps ID3's `UFID` and Vorbis's
        // `musicbrainz_trackid` onto it, and both mean the *recording* — the
        // trap being that `musicbrainz_releasetrackid` is the track id and
        // reads like the other way round.
        track.mb_recording = tag
            .get_string(ItemKey::MusicBrainzRecordingId)
            .and_then(|s| uuid::Uuid::parse_str(s.trim()).ok());
        track.artist = clean(tag.artist().map(|s| s.to_string()));
        track.album = clean(tag.album().map(|s| s.to_string()));
        track.title = clean(tag.title().map(|s| s.to_string()));
        track.genre = clean(tag.genre().map(|s| s.to_string()));
        track.albumartist = clean(tag.get_string(ItemKey::AlbumArtist).map(str::to_string));
        // `Publisher` is `TPUB` in ID3 and `ORGANIZATION` in Vorbis. `LABEL`
        // is neither standard nor rare — Bandcamp and Picard both write it —
        // so it is read as a fallback rather than ignored.
        track.label = clean(
            tag.get_string(ItemKey::Publisher)
                .or_else(|| tag.get_string(ItemKey::Label))
                .map(str::to_string),
        );
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
    /// How much of the library is safe from the failure that stranded a
    /// four-star rating.
    ///
    /// Measured 2026-09-02: **5774 of 5774, 100%**. Every file has either a
    /// MusicBrainz recording id or an audio hash, so every one keeps its
    /// history through a rename, a renumber and a retag once the key flips.
    ///
    /// Run with `cargo test --release how_much -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs the real library"]
    fn how_much_of_the_library_has_a_durable_identity() {
        let root = std::path::PathBuf::from(std::env::var("HOME").unwrap()).join("MusicLibrary");
        if !root.exists() {
            return;
        }
        let mut store = crate::store::Scan::new();
        let r = scan(&mut store, &[root]);
        eprintln!(
            "{} of {} tracks identified ({:.1}%)",
            r.identified,
            store.len(),
            100.0 * r.identified as f32 / store.len() as f32
        );
    }

    use super::*;

    /// A minimal 16-bit PCM WAV, so a scan test needs no music library.
    fn write_wav(path: &Path, frames: u32) {
        let (channels, rate, bps) = (1u16, 44_100u32, 2u16);
        let block = channels * bps;
        let data = frames * block as u32;
        let mut w: Vec<u8> = Vec::with_capacity(44 + data as usize);
        w.extend_from_slice(b"RIFF");
        w.extend_from_slice(&(36 + data).to_le_bytes());
        w.extend_from_slice(b"WAVEfmt ");
        w.extend_from_slice(&16u32.to_le_bytes());
        w.extend_from_slice(&1u16.to_le_bytes());
        w.extend_from_slice(&channels.to_le_bytes());
        w.extend_from_slice(&rate.to_le_bytes());
        w.extend_from_slice(&(rate * block as u32).to_le_bytes());
        w.extend_from_slice(&block.to_le_bytes());
        w.extend_from_slice(&(bps * 8).to_le_bytes());
        w.extend_from_slice(b"data");
        w.extend_from_slice(&data.to_le_bytes());
        for i in 0..frames {
            w.extend_from_slice(&((i % 1000) as i16).to_le_bytes());
        }
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, w).unwrap();
    }

    fn scan_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gmuse-scan-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// The wiring, not just the helper: `scan` itself must prune, or every
    /// file under an overlapping root is read twice and counted twice. This is
    /// what actually produced `622 tracks` for a 311-track library (#21).
    #[test]
    fn scanning_overlapping_roots_visits_each_file_once() {
        let d = scan_dir("overlapping");
        // Different lengths, so the two are different files. Written the same
        // once, which made them one row and made this test about duplicates
        // rather than about overlapping roots (#86).
        write_wav(&d.join("Dance/a.wav"), 4410);
        write_wav(&d.join("General/b.wav"), 8820);

        // The stale parent beside its two real children, as the cache had it.
        let roots = vec![d.join("Dance"), d.join("General"), d.clone()];
        let mut store = Scan::new();
        let report = scan(&mut store, &roots);

        assert_eq!(report.errors, vec![], "the fixture files did not read");
        assert_eq!(store.len(), 2);
        assert_eq!(
            report.total(),
            2,
            "each file was counted more than once: {report}"
        );
        assert_eq!(report.added, 2);
    }

    /// And the incremental path counts once too — the case that reported 622,
    /// where everything was `unchanged`.
    #[test]
    fn rescanning_overlapping_roots_counts_each_file_once() {
        let d = scan_dir("overlapping-rescan");
        write_wav(&d.join("Dance/a.wav"), 4410);
        write_wav(&d.join("General/b.wav"), 4410);
        let roots = vec![d.join("Dance"), d.join("General"), d.clone()];

        let mut store = Scan::new();
        scan(&mut store, &roots);
        let again = scan(&mut store, &roots);

        assert_eq!(again.unchanged, 2, "{again}");
        assert_eq!(again.total(), 2, "{again}");
        assert_eq!(again.removed, 0);
    }

    /// A root under another root walked every one of its files twice — read
    /// twice on a cold scan, and counted twice in the report. That is how a
    /// 311-track library came to announce 622 (#21).
    #[test]
    fn a_root_under_another_root_is_dropped() {
        let roots = [
            PathBuf::from("/m/Dance"),
            PathBuf::from("/m/General"),
            PathBuf::from("/m"),
        ];
        // The parent arrives last and swallows the two already kept.
        assert_eq!(prune_nested(&roots), vec![PathBuf::from("/m")]);

        // And the other way round: the parent first, children ignored.
        let roots = [
            PathBuf::from("/m"),
            PathBuf::from("/m/Dance"),
            PathBuf::from("/m/General"),
        ];
        assert_eq!(prune_nested(&roots), vec![PathBuf::from("/m")]);
    }

    #[test]
    fn unrelated_roots_all_survive() {
        let roots = [
            PathBuf::from("/m/Dance"),
            PathBuf::from("/m/General"),
            PathBuf::from("/other"),
        ];
        assert_eq!(prune_nested(&roots), roots.to_vec());
    }

    #[test]
    fn an_exact_duplicate_is_dropped() {
        let roots = [PathBuf::from("/m"), PathBuf::from("/m")];
        assert_eq!(prune_nested(&roots), vec![PathBuf::from("/m")]);
    }

    /// Component-wise, not string prefix: `/m/Bach` is not inside `/m/B`.
    #[test]
    fn a_shared_name_prefix_is_not_containment() {
        let roots = [PathBuf::from("/m/B"), PathBuf::from("/m/Bach")];
        assert_eq!(prune_nested(&roots), roots.to_vec());
    }

    #[test]
    fn nothing_in_nothing_out() {
        assert!(prune_nested(&[]).is_empty());
    }

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
        let mut store = Scan::new();
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
        let mut store = Scan::new();
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
        let mut store = Scan::new();
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
        let mut store = Scan::new();
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
        let mut store = Scan::new();
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
        let mut store = Scan::new();
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
        let mut store = Scan::new();
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
        let mut store = Scan::new();
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

    /// The recovery a failed tag parse falls back to (#53).
    ///
    /// Tested directly rather than by provoking `lofty`: the real trigger is an
    /// APEv2 tag with one malformed item, which five files in this library have
    /// and which cannot be synthesised reliably — an APE footer appended to a
    /// WAV is simply ignored, so a test built that way passes without ever
    /// reaching the fallback. Whether `lofty` trips is `lofty`'s business; what
    /// this project must guarantee is that when it does, the track survives
    /// with a name and its properties.
    #[test]
    fn a_track_recovered_from_a_bad_tag_keeps_its_name_and_properties() {
        let d = scan_dir("recovered");
        let path = d.join("07 A Real Song.wav");
        write_wav(&path, 4410);

        let props = Probe::open(&path)
            .unwrap()
            .options(ParseOptions::new().read_tags(false))
            .read()
            .unwrap();
        let track = from_path_alone(&path, 42, 999, props.properties());

        assert_eq!(track.path, path);
        assert_eq!(
            track.title.as_deref(),
            Some("07 A Real Song"),
            "named by its file, so it can be found rather than being a hole"
        );
        assert_eq!(track.mtime, 42);
        assert_eq!(track.size, 999);
        assert!(
            track.duration > Duration::ZERO,
            "the properties survive even when the tags do not"
        );
        // Not guessed from the directory layout: a wrong artist files the
        // track somewhere you would never think to look.
        assert_eq!(track.artist, None);
        assert_eq!(track.album, None);
    }

    /// The failure the old signature could not survive (#35).
    ///
    /// Retag and move in one pass. The shape key includes title, artist and
    /// album, so it changes at the same moment the path does and the pairing is
    /// lost — which is why this project carried a rule about doing the two
    /// separately, and why a renumber stranded a four-star rating here. The
    /// audio id is computed from the packets, so a tagger cannot reach it.
    #[test]
    fn a_file_that_moves_and_is_retagged_in_one_pass_is_still_recognised() {
        let d = scan_dir("moved-and-retagged");
        let from = d.join("old name.wav");
        write_wav(&from, 4410);
        {
            use lofty::config::WriteOptions;
            use lofty::tag::{Tag, TagExt, TagType};
            let mut tag = Tag::new(TagType::Id3v2);
            tag.insert_text(ItemKey::TrackTitle, "Before".into());
            tag.save_to_path(&from, WriteOptions::default()).unwrap();
        }

        let mut store = Scan::default();
        let report = scan(&mut store, std::slice::from_ref(&d));
        assert_eq!(report.added, 1);

        // Move it *and* rewrite the tags the old key relied on.
        let to = d.join("new name.wav");
        std::fs::rename(&from, &to).unwrap();
        {
            use lofty::config::WriteOptions;
            use lofty::tag::{Tag, TagExt, TagType};
            let mut tag = Tag::new(TagType::Id3v2);
            tag.insert_text(ItemKey::TrackTitle, "Completely Different".into());
            tag.insert_text(ItemKey::TrackArtist, "Someone Else".into());
            tag.insert_text(ItemKey::AlbumTitle, "Another Album".into());
            tag.save_to_path(&to, WriteOptions::default()).unwrap();
        }

        let report = scan(&mut store, std::slice::from_ref(&d));
        assert_eq!(
            report.moves.len(),
            1,
            "the audio is the same file however it is labelled"
        );
        assert_eq!(report.moves[0].from, from);
        assert_eq!(report.moves[0].to, to);
    }

    /// A label sampler's imprint belongs in `TPUB`, not in `albumartist`
    /// where it reads as an artist. Vorbis spells the same thing
    /// `ORGANIZATION`, and `LABEL` is a common third spelling.
    #[test]
    fn the_label_is_read_from_a_real_file() {
        let d = scan_dir("label");
        // A real WAV, so lofty parses it, with tags written onto it.
        let path = d.join("tagged.wav");
        write_wav(&path, 4410);

        use lofty::config::WriteOptions;
        use lofty::tag::{Tag, TagExt, TagType};
        let mut tag = Tag::new(TagType::Id3v2);
        tag.insert_text(ItemKey::TrackArtist, "ANTIGONE".into());
        tag.insert_text(ItemKey::AlbumArtist, "Various Artists".into());
        tag.insert_text(ItemKey::Publisher, "trip recordings".into());
        tag.save_to_path(&path, WriteOptions::default()).unwrap();

        let track = read_track(&path, 0, 0).expect("reads");
        assert_eq!(track.label.as_deref(), Some("trip recordings"));
        assert_eq!(track.artist.as_deref(), Some("ANTIGONE"));
        assert_eq!(track.albumartist.as_deref(), Some("Various Artists"));
        // The label must not leak into the identity the tree files under.
        assert_eq!(track.file_under(), "Various Artists");
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
