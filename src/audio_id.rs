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

use std::path::Path;

use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use twox_hash::xxhash3_128::Hasher as XxHash3_128;

// **The type lives in `gmuse-ids`, and is named here as it always was.**
// Every caller says `audio_id::AudioId`; moving the declaration must not move
// the name, or the split stops being a rename and becomes 84,000 lines of
// churn for nothing.
pub use gmuse_ids::audio_id::AudioId;

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
