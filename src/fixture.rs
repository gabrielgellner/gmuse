//! Libraries for tests to read.
//!
//! **What replaced the `Scan`/`Track` fixtures** (#86). A dozen test modules
//! built a store out of `Track` literals and handed it to whatever was under
//! test, because the store was what the player read. The library is what it
//! reads now, and a `Librarian` is built by *ingesting* — so a fixture goes
//! through the same ladder a real scan does rather than assembling the answer
//! by hand.
//!
//! That is worth more than convenience. A fixture that skipped ingest could
//! assert a projection nothing could actually produce; going through the fold
//! means a test that passes is a test of something reachable.

use crate::model::encoding::Format;
use crate::model::entity::{Device, Encoding};
use crate::model::id::Id;
use crate::model::instance::Scanned;
use crate::model::seam::Librarian;

/// The device a fixture library is held on.
pub fn device() -> Id<Device> {
    Id::<Device>::derived(b"fixture")
}

/// A file described the way its **tags** describe it.
///
/// The shape `store::Track` had, kept for fixtures only. A test that says what
/// a file is tagged with reads better than one that assembles a `Scanned` by
/// hand, and every one of these literals was already written — so this is the
/// old shape crossing into the new one, in one place, rather than a hundred
/// rewrites that would each be a chance to change what a test asserts.
#[derive(Debug, Clone, Default)]
pub struct Tagged {
    pub path: std::path::PathBuf,
    pub artist: Option<String>,
    pub albumartist: Option<String>,
    pub album: Option<String>,
    pub title: Option<String>,
    pub tracknumber: Option<u32>,
    pub discnumber: Option<u32>,
    pub date: Option<i32>,
    pub genres: Vec<String>,
    pub label: Option<String>,
    pub duration: std::time::Duration,
    pub bitrate: Option<u32>,
    pub samplerate: Option<u32>,
    pub channels: Option<u8>,
    pub mtime: i64,
    pub size: u64,
    pub audio_id: Option<crate::audio_id::AudioId>,
    pub levels: crate::model::encoding::Levels,
    pub mb_recording: Option<uuid::Uuid>,
}

impl Tagged {
    /// The holding this file would be.
    ///
    /// **The audio id is derived from the path** unless the fixture names one,
    /// because distinct paths in a test mean distinct files — and a test about
    /// two copies of one recording says so by giving them the same id.
    pub fn held(&self) -> crate::scan::Held {
        let audio = self.audio_id.unwrap_or(crate::audio_id::AudioId {
            scheme: 1,
            hash: u128::from(
                self.path
                    .to_string_lossy()
                    .bytes()
                    .fold(0u64, |a, b| a.wrapping_mul(31).wrapping_add(u64::from(b))),
            ),
        });
        crate::scan::Held {
            at: self.path.clone(),
            mtime: self.mtime,
            size: self.size,
            seen: Scanned {
                audio,
                title: self
                    .title
                    .clone()
                    .unwrap_or_else(|| stem(&self.path).to_string()),
                artist: self.artist.clone().unwrap_or_default(),
                album: self.album.clone(),
                album_artist: self.albumartist.clone(),
                disc: self.discnumber.map(|n| n as u16),
                track: self.tracknumber.map(|n| n as u16),
                genres: self.genres.clone(),
                label: self.label.clone(),
                released: self
                    .date
                    .map(|y| crate::model::spine::PartialDate::year(y as i16)),
                identity: self
                    .mb_recording
                    .map(crate::model::basis::Basis::MusicBrainz),
                levels: self.levels,
                format: format_of(&self.path),
                bitrate: self.bitrate.unwrap_or(0),
                sample_rate: self.samplerate,
                channels: self.channels,
                length_ms: self.duration.as_millis() as u32,
                ..Default::default()
            },
        }
    }
}

/// A file's name, for a fixture with no title tag.
fn stem(path: &std::path::Path) -> &str {
    path.file_name().and_then(|n| n.to_str()).unwrap_or("")
}

/// The container, from the extension — what the old bridge guessed.
fn format_of(path: &std::path::Path) -> Option<Format> {
    Some(
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "flac" => Format::Flac,
            "m4a" | "alac" => Format::Alac,
            "wav" | "aiff" | "aif" => Format::Wav,
            "mp3" => Format::Mp3,
            "aac" => Format::Aac,
            "ogg" | "oga" => Format::Vorbis,
            "opus" => Format::Opus,
            _ => return None,
        },
    )
}

impl crate::scan::Holdings {
    /// Take in a fixture file. **Tests only.**
    pub fn upsert(&mut self, tagged: Tagged) -> crate::model::rows::CopyId {
        let held = tagged.held();
        let id = Id::<Encoding>::of_audio(&held.seen.audio);
        self.insert(held);
        id
    }
}

/// A librarian holding what a scan found.
///
/// The other half of [`Tagged`]: a fixture builds holdings in tag terms and
/// this folds them the way a real launch does, so what a test reads is what
/// the player would.
pub fn from_holdings(held: &crate::scan::Holdings) -> Librarian {
    crate::bridge::librarian(held, &std::collections::BTreeSet::new(), device(), 100)
}

impl Tagged {
    /// The row this file would draw, for a test that renders or matches one.
    ///
    /// Goes through the fold rather than assembling a `Listed` by hand, so a
    /// fixture cannot assert a row the library could not produce.
    pub fn drawn(&self) -> crate::model::rows::Drawn {
        let mut h = crate::scan::Holdings::new();
        h.insert(self.held());
        let l = from_holdings(&h);
        let id = Id::<Encoding>::of_audio(&self.held().seen.audio);
        crate::model::rows::Listing::listed(&l, id)
            .expect("a fixture file draws a row")
            .owned()
    }
}
