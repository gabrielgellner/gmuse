//! An identity for a file's audio, stable under everything a tagger does.
//!
//! `history.toml` is keyed by path and every event names one. A path is a
//! property of *a device's copy* of a track, not of the track, so nothing keyed
//! by it can cross a machine — and on one machine it does not even survive a
//! rename. That cost real data here: a renumber of one album stranded three
//! history entries, one of them a four-star rating on a file that still
//! existed (§13, 2026-09-01).
//!
//! ## What is hashed, and what is deliberately not
//!
//! The **encoded packets**, in order, with every metadata block excluded.
//!
//! The sync design proposes hashing *decoded* frames. Measured on this library,
//! that costs about six times as much and buys nothing: everything a tagger
//! touches is outside the packets by construction, so the packet hash is
//! already stable across a title change, a 4 KB frame, an added or stripped
//! ID3v1, and a downgrade from ID3v2.4 to v2.3 — tested on all five container
//! formats here, including Ogg, Opus and FLAC where the tags live *inside* the
//! stream rather than in a block bolted to the end.
//!
//! Neither hash unifies formats. The same recording as FLAC and as MP3 decodes
//! to *different* samples, so a decoded hash would not match either. Matching
//! across encodings is what a MusicBrainz recording id or an AcoustID is for
//! (#57); this answers the narrower and cheaper question of whether two files
//! hold the same audio.
//!
//! ## Why XXH3-128 and not a cryptographic hash
//!
//! Nothing here is adversarial — this distinguishes files, it does not defend
//! against a forged one. 128 bits puts a collision far past any library size
//! that will exist, and XXH3 is a finalised, published spec rather than an
//! implementation detail. `std`'s `DefaultHasher` is explicitly documented as
//! unstable across Rust releases, which disqualifies it for anything durable.
//!
//! ## The version byte, which is the point of the whole module
//!
//! An id is only useful if it never changes for unchanged audio. The framing
//! here is Symphonia's packet boundaries, which are **not** part of its public
//! contract — a version bump could reframe a format and silently invalidate
//! every event ever recorded, the failure #35 exists to prevent.
//!
//! So the id carries the scheme that produced it, and
//! [`tests::the_framing_is_pinned`] asserts a known file's hash against a
//! literal. If Symphonia reframes anything, that test fails loudly rather than
//! the library going quietly wrong.

use std::fmt;
use std::path::Path;

use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use twox_hash::xxhash3_128::Hasher as XxHash3_128;

/// Which scheme produced an id.
///
/// Stored with the id, not assumed. When the framing has to change, old ids
/// stay readable and are known to be old rather than being silently compared
/// against new ones.
pub const SCHEME: u8 = 1;

/// A file's audio, identified.
/// `Default` is scheme 0 and hash 0 — a value no real file produces, since
/// [`SCHEME`] starts at 1. It exists so partial test fixtures can be built with
/// `..Default::default()`, and it is deliberately not a *valid* id.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct AudioId {
    pub scheme: u8,
    pub hash: u128,
}

impl fmt::Display for AudioId {
    /// `v1:` then 32 hex digits — greppable, and it says what made it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}:{:032x}", self.scheme, self.hash)
    }
}

impl std::str::FromStr for AudioId {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let rest = s.strip_prefix('v').ok_or(())?;
        let (scheme, hash) = rest.split_once(':').ok_or(())?;
        Ok(AudioId {
            scheme: scheme.parse().map_err(|_| ())?,
            hash: u128::from_str_radix(hash, 16).map_err(|_| ())?,
        })
    }
}

/// Why a file's audio could not be identified.
#[derive(Debug, thiserror::Error)]
pub enum AudioIdError {
    #[error("could not open {path}: {source}")]
    Open {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not read {path} as audio: {message}")]
    Format {
        path: std::path::PathBuf,
        message: String,
    },
}

/// What one pass over a file's packets learns: its identity, and its length.
///
/// **The length comes free.** This walk already visits every audio packet to
/// hash it, and every packet carries the duration of its own frames — so
/// counting them costs nothing and is *exact*, which is more than the
/// container can say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Measured {
    pub audio: AudioId,
    /// Playing time in milliseconds, summed from the packets. `None` where the
    /// track carries no time base to convert them with.
    pub length_ms: Option<u32>,
}

/// What reading a file's bytes already told us, so a later pass need not read
/// them again — #188.
///
/// **The expensive answer, guarded by a cheap question.** `measure` reads every
/// packet of a file, which is the point: identity here is the audio, so a tag
/// edit must not change it and a file's own bytes are the only thing that can
/// say. Measured on this library that is 59 GB a walk, and `shelf query` walked
/// twice — once to mint and once to publish holdings — so an `artist=` filter
/// returning four rows read 118 GB to do it.
///
/// None of that work was discovering anything. The log already records which
/// encoding sits at which locator; re-deriving it answers *"is that still
/// true"*, which is a real question and is `shelf verify`'s, where it is a
/// deliberate act rather than the cost of asking anything at all.
///
/// So: `stat` first. A path whose length and mtime are unchanged since we last
/// read it has the audio we last read from it, and a path that is new or has
/// moved underneath us is measured. That is what every content-addressed store
/// keeps beside its hashes, and it is why they stay fast.
///
/// **Derived, and disposable.** This is not a fact and must never become one —
/// the log records claims about music, and "this file was 4.2 MB at 10:03" is
/// not one. It is the same trade `cache.rs` already makes: private format, no
/// compatibility story, thrown away and rebuilt whenever it cannot be read.
///
/// **What it deliberately cannot do is notice a replaced file that kept its
/// size and mtime.** Nothing outside gmuse does that by accident, and the one
/// thing that would — a restore preserving timestamps — is what `shelf verify`
/// exists to catch. The guarantee here is speed, not truth; `verify` still
/// reads every byte, and nothing about it changes.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Measurements {
    seen: std::collections::BTreeMap<std::path::PathBuf, Seen>,
    /// Counted so a caller can say what the walk cost, which is the number
    /// that made this worth building.
    #[serde(skip)]
    hits: u64,
    #[serde(skip)]
    misses: u64,
}

/// One remembered read: what the file looked like, and what it measured.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Seen {
    len: u64,
    /// Milliseconds since the epoch. An `i64` because a clock can predate it
    /// and a file restored from an archive routinely does.
    modified_ms: i64,
    measured: Measured,
}

/// How a file looks without reading it — the cheap question.
///
/// `None` where it cannot be asked at all, which is treated as a miss: a file
/// we cannot `stat` is one we are about to fail to open anyway, and guessing
/// would be the one failure mode this must not have.
fn glance(path: &Path) -> Option<(u64, i64)> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    let ms = match modified.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_millis()).ok()?,
        // Before the epoch, which an archived file really can be.
        Err(e) => -i64::try_from(e.duration().as_millis()).ok()?,
    };
    Some((meta.len(), ms))
}

impl Measurements {
    /// [`measure`], answered from what we already read where the file has not
    /// changed since we read it.
    pub fn of(&mut self, path: &Path) -> Result<Measured, AudioIdError> {
        let now = glance(path);
        if let Some((len, modified_ms)) = now
            && let Some(seen) = self.seen.get(path)
            && seen.len == len
            && seen.modified_ms == modified_ms
        {
            self.hits += 1;
            return Ok(seen.measured);
        }
        self.misses += 1;
        let measured = measure(path)?;
        // **Stored against the glance taken *before* the read**, not after.
        // Taking it after would record the state of a file that changed while
        // we were reading it as though we had measured the new bytes, which is
        // the one way a cache like this goes quietly wrong.
        if let Some((len, modified_ms)) = now {
            self.seen.insert(
                path.to_path_buf(),
                Seen {
                    len,
                    modified_ms,
                    measured,
                },
            );
        }
        Ok(measured)
    }

    /// A copy for the writer, because serialising takes a value and the
    /// caller owns theirs. Cheap relative to what it saves re-reading.
    pub fn clone_of(other: &Measurements) -> Measurements {
        Measurements {
            seen: other.seen.clone(),
            hits: 0,
            misses: 0,
        }
    }

    /// How much of the last walk was answered without reading a file.
    pub fn tally(&self) -> (u64, u64) {
        (self.hits, self.misses)
    }

    /// Forget files that are no longer there, so the cache cannot grow for
    /// ever.
    ///
    /// **Every import renames what it places**, so a library that is worked on
    /// leaves dead paths behind at a steady rate, and nothing else would ever
    /// remove them. A `stat` each is the same cheap question the cache is
    /// built on, and it runs once per command rather than per file read.
    ///
    /// Deliberately not "forget what this run did not ask about": a query
    /// narrowed to one artist still walks the whole library, but a caller that
    /// walked one folder would otherwise throw away everything it did not
    /// touch and pay for it on the next full walk.
    pub fn prune_missing(&mut self) {
        self.seen.retain(|path, _| path.exists());
    }
}

/// Hash a file's encoded audio packets, and measure how long they run.
///
/// Reads but never decodes: this is I/O plus a hash, not a decode of the
/// library, which is what made the cheap version worth measuring.
pub fn measure(path: &Path) -> Result<Measured, AudioIdError> {
    let file = std::fs::File::open(path).map_err(|source| AudioIdError::Open {
        path: path.to_path_buf(),
        source,
    })?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let bad = |message: String| AudioIdError::Format {
        path: path.to_path_buf(),
        message,
    };

    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| bad(e.to_string()))?;

    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| bad("no audio track".into()))?;
    let track_id = track.id;
    let time_base = track.time_base;

    let mut hasher = XxHash3_128::new();
    let mut packets = 0u64;
    let mut frames = 0u64;
    loop {
        match format.next_packet() {
            // Only the audio track. A cover-art or subtitle stream alongside it
            // is metadata by another name, and must not move the id.
            Ok(Some(p)) if p.track_id == track_id => {
                hasher.write(&p.data);
                packets += 1;
                // **The duration of all *valid* frames in the packet**, which
                // is what makes this exact where the container is a guess.
                frames = frames.saturating_add(p.dur.get());
            }
            Ok(Some(_)) => continue,
            Ok(None) => break,
            // A truncated tail is still a file we must identify; what was read
            // identifies it. Refusing would put the track back in the hole #53
            // just dug it out of.
            Err(_) => break,
        }
    }
    if packets == 0 {
        return Err(bad("no audio packets".into()));
    }

    Ok(Measured {
        audio: AudioId {
            scheme: SCHEME,
            hash: hasher.finish_128(),
        },
        // **Integer maths on the time base**, so the answer is exact rather
        // than a float that rounds: milliseconds are `frames × numer × 1000 /
        // denom`, and for the 44.1 kHz case that is `frames × 1000 / 44100`.
        length_ms: time_base.and_then(|base| {
            let numer = u64::from(base.numer.get());
            let denom = u64::from(base.denom.get());
            u32::try_from(frames.saturating_mul(numer).saturating_mul(1000) / denom).ok()
        }),
    })
}

/// Just the identity, for callers that do not want the length.
pub fn of(path: &Path) -> Result<AudioId, AudioIdError> {
    measure(path).map(|m| m.audio)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A tiny real WAV, so Symphonia has something to frame.
    fn wav(path: &Path, frames: usize) {
        let bytes = 4 * frames;
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(b"RIFF").unwrap();
        f.write_all(&((36 + bytes) as u32).to_le_bytes()).unwrap();
        f.write_all(b"WAVEfmt ").unwrap();
        f.write_all(&16u32.to_le_bytes()).unwrap();
        f.write_all(&1u16.to_le_bytes()).unwrap();
        f.write_all(&2u16.to_le_bytes()).unwrap();
        f.write_all(&44100u32.to_le_bytes()).unwrap();
        f.write_all(&176_400u32.to_le_bytes()).unwrap();
        f.write_all(&4u16.to_le_bytes()).unwrap();
        f.write_all(&16u16.to_le_bytes()).unwrap();
        f.write_all(b"data").unwrap();
        f.write_all(&(bytes as u32).to_le_bytes()).unwrap();
        for i in 0..frames {
            let s = ((i % 100) as i16).wrapping_mul(300);
            f.write_all(&s.to_le_bytes()).unwrap();
            f.write_all(&s.to_le_bytes()).unwrap();
        }
    }

    fn dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("gmuse-audioid-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// The whole point: a tagger cannot move the id.
    /// **The length is counted, not estimated**, which is the whole point of
    /// taking it from this pass.
    ///
    /// `lofty` computes a duration from the Xing header where there is one and
    /// from `size / first frame's bitrate` where there is not, and on a VBR
    /// file whose first frame is unrepresentative that is wildly wrong.
    /// Measured on the library: 34 files across four albums, one stating
    /// 1735s for a 287s track. The container is no better — ffprobe agreed
    /// with a decode for 1 of 33. Counting frames is exact, and this walk
    /// already visits every one of them.
    #[test]
    fn the_length_is_counted_from_the_packets() {
        let dir = std::env::temp_dir().join("gmuse-audioid-length");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // 44,100 frames at 44.1 kHz is one second, exactly.
        let one = dir.join("one.wav");
        wav(&one, 44_100);
        let m = measure(&one).expect("readable");
        assert_eq!(
            m.length_ms,
            Some(1000),
            "a one-second file did not measure one second"
        );

        // And it scales: half as many frames, half the time.
        let half = dir.join("half.wav");
        wav(&half, 22_050);
        assert_eq!(measure(&half).expect("readable").length_ms, Some(500));

        // The identity is unchanged by any of this — two different lengths are
        // two different sets of packets, and `of` still answers only that.
        assert_ne!(of(&one).unwrap(), of(&half).unwrap());
        assert_eq!(of(&one).unwrap(), m.audio);
    }

    #[test]
    fn tagging_does_not_change_the_id() {
        use lofty::config::WriteOptions;
        use lofty::prelude::{ItemKey, TagExt};
        use lofty::tag::{Tag, TagType};

        let d = dir("retag");
        let path = d.join("t.wav");
        wav(&path, 4410);
        let before = of(&path).expect("reads");

        let mut tag = Tag::new(TagType::Id3v2);
        tag.insert_text(ItemKey::TrackTitle, "A COMPLETELY DIFFERENT TITLE".into());
        tag.insert_text(ItemKey::Comment, "x".repeat(4000));
        tag.save_to_path(&path, WriteOptions::default()).unwrap();

        assert!(
            std::fs::metadata(&path).unwrap().len() > 4410 * 4,
            "the tag really was written"
        );
        assert_eq!(
            before,
            of(&path).expect("still reads"),
            "the audio did not change"
        );
    }

    /// And it is an identity, not a constant.
    #[test]
    fn different_audio_gets_a_different_id() {
        let d = dir("differs");
        let (a, b) = (d.join("a.wav"), d.join("b.wav"));
        wav(&a, 4410);
        wav(&b, 8820);
        assert_ne!(of(&a).unwrap(), of(&b).unwrap());
    }

    /// **The guard against a silent dependency bump.**
    ///
    /// The framing is Symphonia's packet boundaries, which are not part of its
    /// public contract. If an upgrade reframes WAV, this fails — which is the
    /// **A cached measurement is only used while the file has not moved
    /// underneath it** — #188.
    ///
    /// The cache exists because `measure` reads every byte, and a library that
    /// has not changed should not be re-read to discover that. What it must
    /// never do is hand back a stale answer for a file that *has* changed, so
    /// the `stat` is the whole of its correctness and this is the test of it.
    #[test]
    fn a_measurement_is_kept_only_while_the_file_is_unchanged() {
        let dir = std::env::temp_dir().join("gmuse-measurements");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.wav");
        wav(&path, 1000);

        let mut seen = Measurements::default();
        let first = seen.of(&path).expect("it measures");
        assert_eq!(seen.tally(), (0, 1), "nothing was known yet");

        // Asked again, unchanged: answered without reading.
        let again = seen.of(&path).expect("it answers");
        assert_eq!(again, first, "the same file measures the same");
        assert_eq!(seen.tally(), (1, 1), "and it did not read the bytes again");

        // **Different audio, and the length changes with it.** A rewrite is
        // what the cache must notice, and the point of noticing is that the id
        // is different: a stale hit here would have the library naming bytes
        // that are not there.
        wav(&path, 2000);
        let rewritten = seen.of(&path).expect("it measures again");
        assert_eq!(seen.tally(), (1, 2), "a changed file is read");
        assert_ne!(
            rewritten.audio, first.audio,
            "different audio must not keep the old id"
        );

        // And the new answer is what is kept from then on.
        assert_eq!(seen.of(&path).expect("cached"), rewritten);
        assert_eq!(seen.tally(), (2, 2));

        // **A file that cannot be looked at is a miss, never a hit.** Guessing
        // here is the one failure this must not have, and a deleted file must
        // surface as the error it is rather than as a remembered answer.
        std::fs::remove_file(&path).unwrap();
        assert!(seen.of(&path).is_err(), "a gone file is not still known");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **The cache is dropped rather than migrated, like every other one
    /// here** — and a round trip has to survive, or it is dropped every run.
    #[test]
    fn what_was_measured_survives_being_written_and_read_back() {
        let dir = std::env::temp_dir().join("gmuse-measurements-roundtrip");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.wav");
        wav(&path, 1234);

        let mut seen = Measurements::default();
        let measured = seen.of(&path).expect("it measures");

        let at = dir.join("measurements.postcard");
        crate::cache::save_measurements(&at, &seen);
        let mut back = crate::cache::load_measurements(&at);
        assert_eq!(
            back.of(&path).expect("it answers"),
            measured,
            "what came back does not measure the same"
        );
        assert_eq!(back.tally(), (1, 0), "and it answered without reading");

        // **Corrupt is the same as absent**: a slow run, never a wrong one.
        std::fs::write(&at, b"not a cache at all").unwrap();
        let mut empty = crate::cache::load_measurements(&at);
        assert_eq!(empty.tally(), (0, 0));
        empty.of(&path).expect("it measures from scratch");
        assert_eq!(empty.tally(), (0, 1), "nothing was trusted from a bad file");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// only way to find out before every recorded event is invalidated.
    #[test]
    fn the_framing_is_pinned() {
        let d = dir("pinned");
        let path = d.join("t.wav");
        wav(&path, 4410);
        assert_eq!(
            of(&path).unwrap().to_string(),
            "v1:8fb0dcfbf145dfff6649784265da0229",
            "symphonia reframed the audio, or the hash changed: every id in \
             every log is now wrong. Do not update this literal without \
             bumping SCHEME."
        );
    }

    #[test]
    fn an_id_round_trips_through_its_text() {
        let id = AudioId {
            scheme: 1,
            hash: 0x0123_4567_89ab_cdef_0123_4567_89ab_cdef,
        };
        assert_eq!(id.to_string(), "v1:0123456789abcdef0123456789abcdef");
        assert_eq!(id.to_string().parse::<AudioId>().unwrap(), id);
    }

    #[test]
    fn a_file_that_is_not_audio_is_an_error_not_an_id() {
        let d = dir("junk");
        let path = d.join("t.wav");
        std::fs::write(&path, b"this is not a wav").unwrap();
        assert!(of(&path).is_err());
    }
}
