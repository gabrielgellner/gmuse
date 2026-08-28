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

#[derive(Debug, Default, Clone)]
pub struct ScanReport {
    pub added: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub removed: usize,
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
            "{} tracks ({} added, {} updated, {} unchanged, {} removed, {} errors) in {:.2}s",
            self.total(),
            self.added,
            self.updated,
            self.unchanged,
            self.removed,
            self.errors.len(),
            self.elapsed.as_secs_f64()
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
            let mtime = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);

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
            match read_track(&path, mtime) {
                Ok(track) => {
                    store.upsert(track);
                    if existed {
                        report.updated += 1;
                    } else {
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
    for path in gone {
        store.remove_path(&path);
        report.removed += 1;
    }

    report.elapsed = start.elapsed();
    report
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
pub fn read_track(path: &Path, mtime: i64) -> Result<Track, String> {
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
