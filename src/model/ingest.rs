//! Reading a file into the catalogue — issue #70, `domain-model.md` §3.
//!
//! Tags are **claims, not facts**. The boundary resolves them against what is
//! already catalogued, in order, and never rejects anything:
//!
//! 1. a MusicBrainz id in the tags
//! 2. an acoustic fingerprint match — #57, not built
//! 3. a name and title match above a stated threshold
//! 4. otherwise a **provisional** entity, flagged for review
//!
//! ## Why it never rejects
//!
//! A validator that refuses bad tags makes a library of this age unimportable.
//! Measured 2026-09-03 over 6500 files: 5532 of 6002 carry MusicBrainz ids in
//! ID3, 462 of 469 as Vorbis comments and 29 of 29 in MP4 atoms — 92.7%, which
//! leaves **477 files** that step 1 cannot help with.
//!
//! Those 477 are not obscure. The largest groups are Adele, Stromae, James
//! Blake, Stephan Bodzin and Burial — all thoroughly catalogued upstream. It is
//! a **tagging** gap, not an identity gap, and a lookup pass closes most of it.
//!
//! So bad input degrades to "needs review" rather than to rejection or, far
//! worse, to a silently invented fact. [`Confidence`] is what records the
//! degradation, and what makes a guess reviewable later instead of
//! indistinguishable from certainty.
//!
//! ## The trap in the Vorbis names
//!
//! MusicBrainz's `musicbrainz_trackid` comment holds the **recording** id,
//! while `musicbrainz_releasetrackid` holds the track id — the opposite of what
//! the names suggest, for historical reasons. Reading the wrong one binds every
//! file to a per-release identity that changes between editions, which is
//! precisely the flattening this model exists to undo. `lofty` maps both
//! correctly, so `ItemKey::MusicBrainzRecordingId` is the right key on every
//! format; the trap is recorded here because the next person to reach past
//! `lofty` will meet it.

use std::path::Path;

use lofty::config::ParseOptions;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::prelude::{Accessor, ItemKey};
use lofty::probe::Probe;

use crate::model::basis::Basis;
use crate::model::instance::Scanned;
use crate::model::spine::PartialDate;

/// How much the catalogue trusts an identification.
///
/// Ordered, so "at least matched" is a comparison and a review surface can sort
/// the least certain to the top.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
// Lowercase on the wire, for the reason `Source` is: a proposal file is
// something a person types.
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// Minted because nothing matched. A guess, and marked as one.
    Provisional,
    /// Matched on a name, above the threshold.
    Matched,
    /// An external authority named it: a MusicBrainz id in the tags.
    Confirmed,
}

impl Confidence {
    /// How it is written in a filter — the review queue is
    /// `genre[basis=discogs & confidence!=confirmed]` (#93).
    pub fn written(self) -> &'static str {
        match self {
            Confidence::Provisional => "provisional",
            Confidence::Matched => "matched",
            Confidence::Confirmed => "confirmed",
        }
    }
}

/// Why a file could not be read at all.
///
/// Only ever an I/O or format failure. **Bad metadata is not an error** — that
/// is the whole point of the ladder.
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error("could not read {path}: {message}")]
    Unreadable { path: String, message: String },
}

/// Read one file into the claim it makes about itself.
///
/// A file whose tags will not parse still comes back: `lofty` abandons a whole
/// file over one malformed APEv2 item — five files here do exactly that — and
/// losing the track over it would be the rejection this module refuses. The
/// Every word in `MusicBrainz Album Type`, in order — #133.
///
/// **Two shapes, one answer.** Picard `/`-joins them into one value and other
/// taggers write a frame each; splitting every value on `/` reads both, and
/// the primary type is whichever comes first.
fn types(tag: Option<&lofty::tag::Tag>) -> Vec<String> {
    tag.map(|t| {
        t.get_strings(ItemKey::MusicBrainzReleaseType)
            .flat_map(|v| v.split('/'))
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .collect()
    })
    .unwrap_or_default()
}

/// title falls back to the file stem, which is a playable, sortable row rather
/// than a hole.
/// The suffix a mastering tool leaves on a filename — #143.
///
/// One spelling, in the layer that reads files, because the Python `quality`
/// pass tests for the same thing and two spellings would let them drift. A
/// *suffix* on the stem and never a substring: `openpn.mp3` is a file called
/// `openpn`.
pub const PROCESSED_MARKER: &str = "_pn";

/// Whether this file's name records that a mastering tool re-encoded it.
pub fn processed(path: &Path) -> bool {
    path.file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|stem| stem.ends_with(PROCESSED_MARKER))
}

/// What reading a file already told us, so a later pass need not read it again
/// — #188, widened to the whole read by #190.
///
/// **The expensive answer, guarded by a cheap question.** Reading a file costs
/// twice: `audio_id::measure` walks every packet, because identity here is the
/// audio and a tag edit must not change it; and `lofty` parses the tags. On
/// this library that is 59 GB and ~0.33ms a file respectively, and `shelf
/// query` did both twice — once to mint and once to publish holdings — so an
/// `artist=` filter returning four rows spent three seconds discovering
/// nothing.
///
/// Nothing was being discovered because the log already records all of it. The
/// tags were consumed at import and the design holds that they are never
/// consulted again; re-reading them answers *"is what the log says still
/// true"*, which is a real question and is `shelf verify`'s, where it is a
/// deliberate act rather than the cost of asking anything at all.
///
/// So: `stat` first. A path whose length and mtime are unchanged since we last
/// read it holds what we last read from it. That is what every
/// content-addressed store keeps beside its hashes, and why they stay fast.
///
/// **One cache, not two.** A [`Scanned`] carries its own [`AudioId`], so the
/// walk that wants a whole scan and the walk that wants only the identity are
/// answered from the same entry. They are the two walks in `query::rows`, and
/// before this they each paid separately.
///
/// **Derived, and disposable.** This is not a fact and must never become one —
/// the log records claims about music, and "this file was 4.2 MB at 10:03" is
/// not one. Same trade as `cache.rs`: private format, no compatibility story,
/// thrown away and rebuilt whenever it cannot be read.
///
/// **What it deliberately cannot do is notice a replaced file that kept its
/// size and mtime.** Nothing outside gmuse does that by accident, and the one
/// thing that would — a restore preserving timestamps — is what `shelf verify`
/// exists to catch. The guarantee here is speed, not truth; `verify` still
/// reads every byte.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Reads {
    seen: std::collections::BTreeMap<std::path::PathBuf, Seen>,
    /// Counted so a caller can say what a walk cost, which is the number that
    /// made this worth building.
    #[serde(skip)]
    hits: u64,
    #[serde(skip)]
    misses: u64,
}

/// One remembered read: what the file looked like, and what it held.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Seen {
    len: u64,
    /// Milliseconds since the epoch. An `i64` because a clock can predate it,
    /// and a file restored from an archive routinely does.
    modified_ms: i64,
    scanned: Scanned,
}

/// How a file looks without opening it — the cheap question.
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

impl Reads {
    /// [`read`], answered from a previous read where the file is unchanged.
    pub fn scanned(&mut self, path: &Path) -> Result<Scanned, IngestError> {
        let now = glance(path);
        if let Some((len, modified_ms)) = now
            && let Some(seen) = self.seen.get(path)
            && seen.len == len
            && seen.modified_ms == modified_ms
        {
            self.hits += 1;
            return Ok(seen.scanned.clone());
        }
        self.misses += 1;
        let scanned = read(path)?;
        // **Stored against the glance taken *before* the read**, not after.
        // Taking it after would record the state of a file that changed while
        // we were reading it as though we had read the new bytes, which is the
        // one way a cache like this goes quietly wrong.
        if let Some((len, modified_ms)) = now {
            self.seen.insert(
                path.to_path_buf(),
                Seen {
                    len,
                    modified_ms,
                    scanned: scanned.clone(),
                },
            );
        }
        Ok(scanned)
    }

    /// Just the identity, for the walk that publishes holdings.
    ///
    /// **The same entry as [`Reads::scanned`]**, because a `Scanned` carries
    /// its `AudioId`. A path this has not seen is read whole rather than
    /// hashed alone: the two walks always run together, so the tags are wanted
    /// a moment later anyway, and keeping one guard is worth more than saving
    /// a tag parse on a file that is new.
    pub fn audio(&mut self, path: &Path) -> Result<crate::audio_id::AudioId, IngestError> {
        self.scanned(path).map(|s| s.audio)
    }

    /// How much of the last walk was answered without opening a file.
    pub fn tally(&self) -> (u64, u64) {
        (self.hits, self.misses)
    }

    /// A copy for the writer, because serialising takes a value and the caller
    /// owns theirs. Cheap relative to what it saves re-reading.
    pub fn clone_of(other: &Reads) -> Reads {
        Reads {
            seen: other.seen.clone(),
            hits: 0,
            misses: 0,
        }
    }

    /// Forget files that are no longer there, so the cache cannot grow for
    /// ever.
    ///
    /// **Every import renames what it places**, so a library that is worked on
    /// leaves dead paths behind at a steady rate and nothing else would remove
    /// them. A `stat` each is the same cheap question the cache is built on,
    /// and it runs once per command rather than per file read.
    pub fn prune_missing(&mut self) {
        self.seen.retain(|path, _| path.exists());
    }
}

pub fn read(path: &Path) -> Result<Scanned, IngestError> {
    let bad = |message: String| IngestError::Unreadable {
        path: path.display().to_string(),
        message,
    };

    let measured = crate::audio_id::measure(path).map_err(|e| bad(e.to_string()))?;
    let audio = measured.audio;
    let stem = || {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string()
    };

    let Ok(tagged) = Probe::open(path)
        .map_err(|e| bad(e.to_string()))?
        .options(ParseOptions::new())
        .read()
    else {
        return Ok(Scanned {
            audio,
            title: stem(),
            artist: String::new(),
            identity: None,
            // **Especially here** (#143). A file whose tags will not parse is
            // exactly the one whose filename is the only evidence there is,
            // and this branch dropped it — which is the shape of the whole
            // problem: the path knows something and nothing carries it.
            processed: processed(path),
            ..Default::default()
        });
    };

    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
    let get = |k: ItemKey| {
        tag.and_then(|t| t.get_string(k))
            .unwrap_or("")
            .trim()
            .to_string()
    };
    // **How many artists a credit's id field names** — #145, and the half of
    // that guard which only worked in ID3.
    //
    // Picard `/`-joins ids into one ID3 frame, so `contains('/')` answered the
    // question there and nowhere else. A Vorbis comment is natively repeated:
    // *Trio* carries three separate `musicbrainz_albumartistid` fields, no
    // slash anywhere, so the guard read `false` and let the credit rename
    // Dolly Parton to `Dolly Parton, Linda Ronstadt & Emmylou Harris` — which
    // filed all six of her albums under a name only one of them has.
    //
    // Measured on this library: 72 mp3 files slash-joined, where the old test
    // worked, and 11 ogg files repeated, where it did not.
    let names_several = |k: ItemKey| {
        tag.is_some_and(|t| {
            t.get_strings(k).take(2).count() > 1 || t.get_string(k).is_some_and(|v| v.contains('/'))
        })
    };

    let title = match tag.and_then(|t| t.title()) {
        Some(t) if !t.trim().is_empty() => t.trim().to_string(),
        _ => stem(),
    };

    Ok(Scanned {
        audio,
        title,
        artist: match tag.and_then(|t| t.artist()) {
            Some(a) if !a.trim().is_empty() => a.trim().to_string(),
            _ => String::new(),
        },
        // One key, every format: lofty maps ID3's UFID and Vorbis's
        // `musicbrainz_trackid` onto it, and both mean the recording. The top
        // rung of #89's ladder; a harvest pass fills the middle one.
        artist_mbid: primary_artist(&get(ItemKey::MusicBrainzArtistId)),
        credit_is_joined: names_several(ItemKey::MusicBrainzArtistId),
        // The album artist's own answer, which the track's does not give
        // (#145): a record credited to two people may have a single-artist
        // track credit, and on *One from the Heart* it does.
        album_credit_is_joined: names_several(ItemKey::MusicBrainzReleaseArtistId),
        // **What only the name knows, read while it still exists** (#143).
        // `shelf render` names a placed file from the library, so this is the
        // last moment the marker is there to be read.
        processed: processed(path),
        album_artist_mbid: primary_artist(&get(ItemKey::MusicBrainzReleaseArtistId)),
        // The ladder, best rung first (#89): MusicBrainz where the tags carry
        // it, otherwise whatever authority does name this performance. `None`
        // falls to the bytes, which is the rung a re-encode strands.
        identity: uuid::Uuid::parse_str(&get(ItemKey::MusicBrainzRecordingId))
            .ok()
            .map(Basis::MusicBrainz)
            .or_else(|| external(tag)),
        album: some(tag.and_then(|t| t.album()).map(|s| s.trim().to_string())),
        // Album artist first, because a guest credit on one track must not
        // split the album off from the artist it files under.
        album_artist: some(Some(get(ItemKey::AlbumArtist))),
        // **The release-group type, primary first** (#133). Picard writes it
        // `/`-joined — `album`, `album/compilation`, `album/compilation/dj-mix`
        // — and some taggers write one value per frame instead, so both are
        // read: every value the tag carries, split on `/`.
        //
        // A word this build has never heard of is **dropped rather than
        // guessed at**. `Other` in a set says nothing a query could use, and
        // the file still carries the word for a later build to read.
        release_kind: types(tag)
            .first()
            .and_then(|v| crate::model::spine::ReleaseType::parse(v))
            .unwrap_or_default(),
        release_secondary: types(tag)
            .iter()
            .skip(1)
            .filter_map(|v| crate::model::spine::ReleaseSecondary::parse(v))
            .collect(),
        disc: number(&get(ItemKey::DiscNumber)),
        track: number(&get(ItemKey::TrackNumber)),
        // **The totals, and they are the last half of `3/12`.** Where a tag
        // writes both in one frame, `number` above takes the first and this
        // takes the second; where the format carries them apart, both read
        // the whole value. Zero is nobody saying rather than a disc of no
        // tracks, which is not a thing (#134).
        // **Whatever notation the tagger used** (#136). `TKEY` carries the
        // musical spelling on some files and open key on others, and both
        // parse to one value here so nothing downstream sees a notation.
        key: crate::model::key::Key::parse(&get(ItemKey::InitialKey)),
        track_total: total(&get(ItemKey::TrackTotal), &get(ItemKey::TrackNumber)),
        disc_total: total(&get(ItemKey::DiscTotal), &get(ItemKey::DiscNumber)),
        mb_release_group: uuid::Uuid::parse_str(&get(ItemKey::MusicBrainzReleaseGroupId)).ok(),
        mb_release: uuid::Uuid::parse_str(&get(ItemKey::MusicBrainzReleaseId)).ok(),
        format: format_of(&tagged),
        // The audio bitrate where the container reports one, else the overall
        // rate. A verdict compares a cutoff against what the file *claims*, so
        // the claim has to come from the file rather than from a guess.
        // **Counted, not estimated.** `lofty` computes a duration from the
        // Xing header where there is one and from `size / first frame's
        // bitrate` where there is not — and on a VBR file whose first frame is
        // unrepresentative that is wildly wrong. Measured here: 33 files, in
        // four albums, one of them stating 1735s for a 287s track, because the
        // first frame read as 32 kbps against a real average of 175.
        //
        // The container is no better: ffprobe agreed with a decode for 1 of
        // those 33. Only counting frames is right, and `audio_id` already
        // walks every audio packet to hash it — so the exact answer costs
        // nothing and comes from the same pass.
        //
        // Falls back to `lofty` where the track carries no time base to
        // convert frames with, because a number that is usually right beats
        // none at all.
        length_ms: measured
            .length_ms
            .unwrap_or_else(|| tagged.properties().duration().as_millis() as u32),
        // Two dates, because the tags carry two and they mean different
        // things: `DATE` is this pressing, `ORIGINALDATE` the album. They
        // disagree for 820 files in this library.
        // **`Label` then `Publisher`.** ID3 puts the imprint in `TPUB`, which
        // lofty calls `Publisher`; Vorbis and MP4 use `LABEL`. Reading only
        // the second missed it on every MP3 — 30 rows in this library — which
        // the store's own reader had right and this one had not (#86).
        label: Some(get(ItemKey::Label))
            .filter(|s| !s.is_empty())
            .or_else(|| Some(get(ItemKey::Publisher)).filter(|s| !s.is_empty())),
        genres: crate::scan::split_genres(Some(&get(ItemKey::Genre))),
        levels: crate::model::encoding::Levels {
            track_cdb: cdb(&get(ItemKey::ReplayGainTrackGain)),
            track_peak_e4: peak_e4(&get(ItemKey::ReplayGainTrackPeak)),
            album_cdb: cdb(&get(ItemKey::ReplayGainAlbumGain)),
            album_peak_e4: peak_e4(&get(ItemKey::ReplayGainAlbumPeak)),
        },
        released: PartialDate::parse(&get(ItemKey::RecordingDate)),
        first_released: PartialDate::parse(&get(ItemKey::OriginalReleaseDate)),
        sample_rate: tagged.properties().sample_rate(),
        channels: tagged.properties().channels(),
        bitrate: tagged
            .properties()
            .audio_bitrate()
            .or_else(|| tagged.properties().overall_bitrate())
            .unwrap_or(0),
    })
}

/// What the prober says the audio is coded as.
///
/// `None` when it says something this model has no name for — an unreadable
/// header, not a missing format, and a distinction worth keeping because one
/// is a file to look at and the other is a gap in the enum.
fn format_of(tagged: &lofty::file::TaggedFile) -> Option<crate::model::encoding::Format> {
    use crate::model::encoding::Format as F;
    use lofty::file::FileType;
    Some(match tagged.file_type() {
        FileType::Flac => F::Flac,
        FileType::Mpeg => F::Mp3,
        FileType::Wav => F::Wav,
        FileType::Vorbis => F::Vorbis,
        FileType::Opus => F::Opus,
        FileType::Aac => F::Aac,
        FileType::Mp4 => F::Alac,
        _ => return None,
    })
}

/// An empty string is an absent tag, not a value.
fn some(s: Option<String>) -> Option<String> {
    s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// `4`, `4/12` and ` 4 ` are all four. A tag that says something else is
/// absent rather than zero, because zero is a position and "no idea" is not.
fn number(s: &str) -> Option<u16> {
    s.split(['/', '-']).next()?.trim().parse().ok()
}

/// The **total**, out of a tag that may carry it beside the position — #134.
///
/// `total` is what the format says outright; `beside` is the position field,
/// which in ID3 is a single `3/12` frame holding both. Whichever names a
/// second half wins, and a bare number in the total field is the total.
///
/// **Zero is nobody saying.** A disc of no tracks is not a thing, and a `0`
/// written by a tagger that fills every field must not read as a claim that
/// the album is empty — which would make every track on it look surplus.
fn total(total: &str, beside: &str) -> Option<u16> {
    let after_slash = |s: &str| s.split_once('/').map(|(_, n)| n.trim().to_string());
    after_slash(total)
        .or_else(|| after_slash(beside))
        .unwrap_or_else(|| total.trim().to_string())
        .parse()
        .ok()
        .filter(|n| *n > 0)
}

/// How confidently this file names a recording, before anything is minted.
///
/// Step 2 of the ladder is missing: an acoustic fingerprint would sit between
/// the external id and the name match, and is #57. Its absence is why a
/// mistagged file with no MusicBrainz id falls straight to a name comparison
/// rather than to the thing that would actually recognise it.
/// The **primary** artist's MusicBrainz id, out of a credit that may list
/// several.
///
/// Picard writes every credited artist's id, `/`-joined, in the order the
/// credit names them — so `2Pac feat. Nate Dogg` carries two. Taking only a
/// lone id and falling back to the name otherwise is what shatters an artist:
/// measured here, **every one of the fourteen `2Pac feat. …` files has a
/// joined id**, so every one of them fell to its credit string and minted its
/// own 2Pac.
///
/// The first is the primary artist, which is what "feat." means. That is
/// MusicBrainz's own ordering rather than a guess about this library.
fn primary_artist(field: &str) -> Option<uuid::Uuid> {
    uuid::Uuid::parse_str(field.split('/').next()?.trim()).ok()
}

/// Tags that can carry a link to where a file came from.
///
/// Tried in this order so two devices reading one file agree: a track with
/// both a shop link and a video link must not identify differently depending
/// on which tag happened to be read first.
const CARRIERS: &[ItemKey] = &[
    ItemKey::AudioFileUrl,
    ItemKey::AudioSourceUrl,
    ItemKey::Comment,
    ItemKey::Description,
];

/// The Beatport track id in a URL, if there is one.
///
/// `beatport.com/track/modulo-tierra-nolah-remix/9728882` — the slug is
/// decoration and the trailing number is the id. Matched by hand rather than
/// by a pattern language: two shapes do not earn a dependency, and §12 asks
/// for explicit over abstract.
fn beatport(text: &str) -> Option<String> {
    let rest = text.split_once("beatport.com/track/")?.1;
    let id: String = rest
        .rsplit('/')
        .next()?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    (!id.is_empty()).then_some(id)
}

/// The YouTube video id in a URL, if there is one.
///
/// Eleven characters of the base64url alphabet, which is what makes it
/// findable in free text — the live-session rips carry theirs inside a comment
/// like `Western AF — https://youtu.be/1lYigC2Jn20`.
fn youtube(text: &str) -> Option<String> {
    const MARKERS: [&str; 2] = ["youtube.com/watch?v=", "youtu.be/"];
    const LEN: usize = 11;
    for marker in MARKERS {
        if let Some(rest) = text.split_once(marker).map(|(_, r)| r) {
            let id: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                .take(LEN)
                .collect();
            if id.len() == LEN {
                return Some(id);
            }
        }
    }
    None
}

/// The first external id any carrier tag holds.
///
/// **Only what is already in the files.** Measured on this library: 113 of 592
/// DJ-crate tracks carry a Beatport track id in `WOAF`, and the live-session
/// rips carry a video id in a comment. Neither was reachable before, because
/// the identity field could hold nothing but a MusicBrainz id — and both name
/// the performance as well as an MBID does, for music MusicBrainz has never
/// heard of.
fn external(tag: Option<&lofty::tag::Tag>) -> Option<Basis> {
    // **`get_string` cannot see a URL frame.** lofty stores `WOAF` and friends
    // as `ItemValue::Locator`, and `get_string` returns only `Text` — so every
    // link came back empty and the harvest reported that the files carried no
    // external id. That is the fourth reader this session to silently not see
    // a value shape, after ffprobe's blindness to `UFID`, mutagen's raw ID3
    // frame names, and its list-valued Vorbis comments.
    let read = |key: ItemKey| -> String {
        tag.and_then(|t| t.get(key))
            .map(lofty::tag::TagItem::value)
            .and_then(|v| match v {
                lofty::tag::ItemValue::Text(s) | lofty::tag::ItemValue::Locator(s) => {
                    Some(s.clone())
                }
                lofty::tag::ItemValue::Binary(_) => None,
            })
            .unwrap_or_default()
    };
    for key in CARRIERS {
        let text = read(*key);
        if text.is_empty() {
            continue;
        }
        if let Some(key) = beatport(&text) {
            return Some(Basis::External {
                authority: "beatport".to_string(),
                key,
            });
        }
        if let Some(key) = youtube(&text) {
            return Some(Basis::External {
                authority: "youtube".to_string(),
                key,
            });
        }
    }
    None
}

#[allow(
    dead_code,
    reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — ingest sets confidence inline; this is the same rule addressable on its own"
)]
pub fn confidence_of(scan: &Scanned, matches_known_name: bool) -> Confidence {
    if matches!(scan.identity, Some(Basis::MusicBrainz(_))) {
        Confidence::Confirmed
    } else if matches_known_name {
        Confidence::Matched
    } else {
        Confidence::Provisional
    }
}

/// A ReplayGain value, as hundredths of a decibel.
///
/// Tags write it as `-7.28 dB`, sometimes with the unit and sometimes
/// without, and occasionally with a `+`. Anything that will not parse is
/// absent rather than zero — nought decibels is a claim that a file needs no
/// correction, and "nobody said" is not.
fn cdb(raw: &str) -> Option<i32> {
    let text = raw
        .trim()
        .trim_end_matches(|c: char| c.is_alphabetic() || c.is_whitespace());
    let db: f32 = text.trim().parse().ok()?;
    db.is_finite().then(|| (db * 100.0).round() as i32)
}

/// A ReplayGain peak, as ten-thousandths of full scale.
///
/// Linear rather than logarithmic, so it has its own unit — see
/// [`crate::model::encoding::Levels`]. A negative peak is not a peak.
fn peak_e4(raw: &str) -> Option<u32> {
    let peak: f32 = raw.trim().parse().ok()?;
    (peak.is_finite() && peak >= 0.0).then(|| (peak * 10_000.0).round() as u32)
}

/// A name reduced to what a match should ignore.
///
/// Deliberately shallow — case and whitespace, nothing more. A matcher that
/// folds accents would merge `Christian Löffler` with an ASCII-folded
/// duplicate, and this library has three folded names that are **wanted back**
/// as their real spellings, so folding here would defeat the repair rather than
/// help it.
///
/// Runs of internal whitespace collapse, because two spaces between two words
/// is a typo and never a distinction — and this is the function concept ids are
/// minted through, so a distinction it draws is a concept nobody meant to
/// create.
///
/// **Whitespace around a comma collapses too** — #153. `split_whitespace`
/// cannot see a space that is *missing*, so `Sante, Sidney Charles` and
/// `Sante,Sidney Charles` minted two artists from one credit typed by two
/// taggers. Four pairs in this library, all from DJ-pool tags, and invisible
/// until now because a file carrying a MusicBrainz artist id never reaches the
/// name path at all.
///
/// **The joiner's spelling is deliberately left alone.** Folding `&` to `and`
/// is the obvious next step and it crosses the line above: a merge repairs a
/// split and nothing repairs a fold, so `Dom & Roland` — one person, whose
/// stage name contains an ampersand — must not become two. No two artists are
/// distinguished by whether a space follows a comma; plenty are distinguished
/// by everything else.
pub fn normalise(name: &str) -> String {
    let tidy = |part: &str| part.split_whitespace().collect::<Vec<_>>().join(" ");
    // Empty parts dropped, so a trailing comma does not leave a dangling
    // separator that is itself a distinction.
    name.split(',')
        .map(tidy)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gmuse-ingest-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// **A remembered read is used only while the file has not moved
    /// underneath it** — #188, widened by #190.
    ///
    /// The cache exists because reading a file costs a walk of every packet
    /// plus a tag parse, and a library that has not changed should not be
    /// re-read to discover that. What it must never do is hand back a stale
    /// answer for a file that *has* changed, so the `stat` is the whole of its
    /// correctness and this is the test of it.
    #[test]
    fn a_read_is_kept_only_while_the_file_is_unchanged() {
        let d = dir("reads");
        let path = d.join("a.wav");
        wav(&path, 1000);

        let mut reads = Reads::default();
        let first = reads.scanned(&path).expect("it reads");
        assert_eq!(reads.tally(), (0, 1), "nothing was known yet");

        // Asked again, unchanged: answered without opening the file.
        let again = reads.scanned(&path).expect("it answers");
        assert_eq!(again, first, "the same file reads the same");
        assert_eq!(reads.tally(), (1, 1), "and it did not open it again");

        // **The identity comes off the same entry**, which is the whole reason
        // one cache serves both walks: `populate` wants the scan and
        // `reconcile` wants only the id, and before this they each paid.
        assert_eq!(reads.audio(&path).expect("cached"), first.audio);
        assert_eq!(reads.tally(), (2, 1), "the second walk is free");

        // **Different audio, and the identity changes with it.** A rewrite is
        // what the cache must notice, and the point of noticing is that the id
        // is different: a stale hit here would have the library naming bytes
        // that are not there.
        wav(&path, 2000);
        let rewritten = reads.scanned(&path).expect("it reads again");
        assert_eq!(reads.tally(), (2, 2), "a changed file is read");
        assert_ne!(
            rewritten.audio, first.audio,
            "different audio must not keep the old id"
        );
        assert_ne!(
            rewritten.length_ms, first.length_ms,
            "nor the old length, which is what a tracklist match turns on"
        );

        // **A file that cannot be looked at is a miss, never a hit.** Guessing
        // here is the one failure this must not have, and a deleted file must
        // surface as the error it is rather than as a remembered answer.
        std::fs::remove_file(&path).unwrap();
        assert!(
            reads.scanned(&path).is_err(),
            "a gone file is not still known"
        );

        let _ = std::fs::remove_dir_all(&d);
    }

    /// **A cache from an older shape is ignored, not misread** — #190.
    ///
    /// postcard is not self-describing, so parsing yesterday's bytes as
    /// today's type is not reliably an error: it can succeed and hand back
    /// nonsense. Nonsense here is a *wrong* row rather than a slow one, which
    /// is the one failure this whole mechanism must not have.
    ///
    /// So the version is taken from the front of the envelope before the
    /// payload is trusted. This was the other way round and worked only
    /// because a shape change happened to fail to parse — which is luck, and
    /// widening the cache to hold a whole `Scanned` is exactly the change that
    /// would have spent it.
    #[test]
    fn a_cache_written_by_an_older_shape_is_ignored() {
        let d = dir("reads-version");
        let path = d.join("a.wav");
        wav(&path, 1500);

        let mut reads = Reads::default();
        let scanned = reads.scanned(&path).expect("it reads");

        let at = d.join("reads.postcard");
        crate::cache::save_measurements(&at, &reads);
        let mut back = crate::cache::load_measurements(&at);
        assert_eq!(
            back.scanned(&path).expect("it answers"),
            scanned,
            "a round trip did not survive"
        );
        assert_eq!(back.tally(), (1, 0), "and it answered without reading");

        // A file whose envelope says some other version: ignored outright,
        // whatever its payload happens to contain.
        let stale = {
            let mut bytes = b"gmuse".to_vec();
            bytes.extend_from_slice(&postcard::to_allocvec(&1u32).expect("a version"));
            bytes.extend_from_slice(&[0xff; 64]);
            bytes
        };
        std::fs::write(&at, &stale).unwrap();
        let mut older = crate::cache::load_measurements(&at);
        assert_eq!(older.tally(), (0, 0), "a v1 file must not be believed");
        older.scanned(&path).expect("it reads from scratch");
        assert_eq!(older.tally(), (0, 1), "nothing was trusted from it");

        // And plain rubbish is the same answer: a slow run, never a wrong one.
        std::fs::write(&at, b"not a cache at all").unwrap();
        let empty = crate::cache::load_measurements(&at);
        assert_eq!(empty.tally(), (0, 0));

        let _ = std::fs::remove_dir_all(&d);
    }

    /// **What only the filename knows, read at the last moment it exists** —
    /// #143.
    ///
    /// `_pn` is a mastering tool's mark, and the cutoff test cannot reach it:
    /// a competent 320-to-320 pass leaves no wall, and 130 of this
    /// collection's 144 marked files measure clean. The suffix is the only
    /// record that it happened.
    ///
    /// **And import destroys it.** `shelf render` names a placed file
    /// `{track} {title}` from the library — measured, 147 marked files on the
    /// NAS and in staging and **0 in the library**. So this is read while
    /// scanning and not proposed afterwards, because afterwards there is
    /// nothing left to read.
    #[test]
    fn a_mastering_marker_is_read_from_the_name() {
        let d = dir("mastering-marker");
        let marked = d.join("120 - G - John Wizards - Muizenberg (Original Mix)_pn.mp3");
        let plain = d.join("121 - Em - Jem Cooke - Slowly Burning (Dub).mp3");
        wav(&marked, 600);
        wav(&plain, 600);

        assert!(read(&marked).unwrap().processed);
        assert!(!read(&plain).unwrap().processed);

        // **A suffix on the stem, never a substring.** `openpn.mp3` is a file
        // called `openpn`, and the Python `quality` pass makes the same
        // distinction against the same constant.
        let word = d.join("openpn.mp3");
        wav(&word, 600);
        assert!(!read(&word).unwrap().processed);

        // And the extension is not part of the stem, so a marked FLAC reads
        // the same way a marked MP3 does.
        let flac = d.join("a_pn.flac");
        wav(&flac, 600);
        assert!(processed(&flac));
    }

    /// **A tag whose artist-id field is repeated rather than slash-joined** —
    /// the half of #145's guard that only worked in ID3.
    ///
    /// Picard `/`-joins ids into one ID3 frame, which is what the old test
    /// looked for. A Vorbis comment is natively repeated: *Trio* carries three
    /// separate `musicbrainz_albumartistid` fields with no slash anywhere, so
    /// the guard read `false` and the credit renamed Dolly Parton to
    /// `Dolly Parton, Linda Ronstadt & Emmylou Harris` — filing all six of her
    /// albums under a name only one of them has.
    ///
    /// Written as a real file, because the bug is in *reading* one: the model
    /// test for #145 sets the flag by hand and could never have caught this.
    ///
    /// **The fixture is ID3 rather than Vorbis**, because a WAV cannot hold a
    /// Vorbis comment and there is no FLAC fixture here. It exercises the same
    /// branch — two values under one key — and the slash spelling is asserted
    /// below, so both halves of the test are covered. The library's real
    /// instance is `Trio`, in Vorbis, and it reads correctly now.
    #[test]
    fn a_repeated_artist_id_says_the_credit_names_several() {
        use lofty::config::WriteOptions;
        use lofty::prelude::TagExt;
        use lofty::tag::{ItemValue, Tag, TagItem, TagType};

        let d = dir("repeated-artist-id");
        let one = d.join("solo.wav");
        let several = d.join("trio.wav");
        wav(&one, 600);
        wav(&several, 600);

        let dolly = "1d543e07-d0d2-4834-a8db-d65c50c2a856";
        let write = |path: &Path, ids: &[&str], credit: &str| {
            let mut t = Tag::new(TagType::Id3v2);
            t.insert_text(ItemKey::AlbumArtist, credit.to_string());
            for id in ids {
                t.push(TagItem::new(
                    ItemKey::MusicBrainzReleaseArtistId,
                    ItemValue::Text((*id).to_string()),
                ));
            }
            t.save_to_path(path, WriteOptions::default()).unwrap();
        };
        write(&one, &[dolly], "Dolly Parton");
        write(
            &several,
            &[dolly, "498f2581-be21-4eef-8756-fbb89d79b1c0"],
            "Dolly Parton, Linda Ronstadt & Emmylou Harris",
        );

        assert!(
            !read(&one).unwrap().album_credit_is_joined,
            "one id names one artist"
        );
        assert!(
            read(&several).unwrap().album_credit_is_joined,
            "**the bug**: a repeated field carries no slash, so the guard read \
             `false` and let a three-way credit rename Dolly Parton"
        );

        // And the ID3 spelling the guard was written for still answers.
        let slashed = d.join("id3.wav");
        wav(&slashed, 600);
        let mut t = Tag::new(TagType::Id3v2);
        t.insert_text(
            ItemKey::MusicBrainzReleaseArtistId,
            format!("{dolly}/498f2581-be21-4eef-8756-fbb89d79b1c0"),
        );
        t.save_to_path(&slashed, WriteOptions::default()).unwrap();
        assert!(read(&slashed).unwrap().album_credit_is_joined);
    }

    /// A tiny real WAV, so there is something to hash and tag.
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

    /// Writes the tag Picard actually writes.
    ///
    /// The recording id goes in as a **real `UFID` frame** rather than through
    /// `Tag::insert_text`, which silently drops it in lofty 0.25 — the generic
    /// write path does not reach the special case that turns the key into a
    /// frame. Reading is the direction that matters here and it works; the
    /// fixture just has to be honest about what is in the file.
    fn tag(path: &Path, title: &str, artist: &str, mbid: Option<&str>) {
        use lofty::config::WriteOptions;
        use lofty::id3::v2::{Frame, Id3v2Tag, UniqueFileIdentifierFrame};
        use lofty::prelude::{Accessor, TagExt};

        let mut t = Id3v2Tag::default();
        t.set_title(title.to_string());
        t.set_artist(artist.to_string());
        if let Some(id) = mbid {
            t.insert(Frame::UniqueFileIdentifier(UniqueFileIdentifierFrame::new(
                "http://musicbrainz.org",
                id.as_bytes().to_vec(),
            )));
        }
        t.save_to_path(path, WriteOptions::default()).unwrap();
    }

    /// A file carrying **both** dates, which is the case they differ in.
    fn tag_dated(path: &Path, pressed: &str, album: &str) {
        use lofty::config::WriteOptions;
        use lofty::id3::v2::Id3v2Tag;
        use lofty::prelude::{ItemKey, TagExt};
        use lofty::tag::{Accessor, ItemValue, Tag, TagItem};

        let mut t = Id3v2Tag::default();
        t.set_title("Second Bad Vilbel".into());
        let mut tag: Tag = t.into();
        tag.insert(TagItem::new(
            ItemKey::RecordingDate,
            ItemValue::Text(pressed.into()),
        ));
        tag.insert(TagItem::new(
            ItemKey::OriginalReleaseDate,
            ItemValue::Text(album.into()),
        ));
        tag.save_to_path(path, WriteOptions::default()).unwrap();
    }

    /// **The pressing and the album are different dates**, and reading one for
    /// the other is the reissue trap (#59). They disagree for 820 files here:
    /// `Tri Repetae++` is a 1996 pressing of a 1995 record.
    /// **Levels are read as integers, and a unit is not a number.**
    ///
    /// Tags write `-7.28 dB` with the unit, without it, and occasionally with
    /// a `+`. Nought decibels is a claim that a file needs no correction, so
    /// anything unparseable is **absent** rather than zero.
    #[test]
    fn replay_gain_is_read_as_hundredths_and_a_bad_value_is_absent() {
        assert_eq!(cdb("-7.28 dB"), Some(-728));
        assert_eq!(cdb("-7.28"), Some(-728));
        assert_eq!(cdb("+3.5 dB"), Some(350));
        assert_eq!(cdb("0.00 dB"), Some(0), "no correction is a claim");
        assert_eq!(cdb(""), None, "and nobody saying so is not");
        assert_eq!(cdb("loud"), None);

        assert_eq!(peak_e4("0.988525"), Some(9885));
        assert_eq!(peak_e4("1.0"), Some(10_000));
        assert_eq!(peak_e4("-1"), None, "a negative peak is not a peak");
        assert_eq!(peak_e4("nan"), None);
    }

    /// Writes an `InitialKey` (`TKEY`) in whatever notation is asked for.
    fn tag_keyed(path: &Path, value: &str) {
        use lofty::config::WriteOptions;
        use lofty::id3::v2::Id3v2Tag;
        use lofty::prelude::{ItemKey, TagExt};
        use lofty::tag::{ItemValue, Tag, TagItem};

        let mut tag: Tag = Id3v2Tag::default().into();
        tag.insert(TagItem::new(
            ItemKey::InitialKey,
            ItemValue::Text(value.into()),
        ));
        tag.save_to_path(path, WriteOptions::default()).unwrap();
    }

    /// Writes `MusicBrainz Album Type` the way Picard does — `/`-joined.
    fn tag_typed(path: &Path, value: &str) {
        use lofty::config::WriteOptions;
        use lofty::id3::v2::Id3v2Tag;
        use lofty::prelude::{ItemKey, TagExt};
        use lofty::tag::{ItemValue, Tag, TagItem};

        let mut tag: Tag = Id3v2Tag::default().into();
        tag.insert(TagItem::new(
            ItemKey::MusicBrainzReleaseType,
            ItemValue::Text(value.into()),
        ));
        tag.save_to_path(path, WriteOptions::default()).unwrap();
    }

    /// **A key is read from the tag and becomes a claim** — #136.
    ///
    /// Whatever notation the tagger used: this library carries `Cmin` on some
    /// files and open-key `4m` on others, and a file whose `TKEY` says `4m`
    /// has `F#m` written in its own comment.
    #[test]
    fn a_key_is_read_whatever_notation_the_tagger_used() {
        use crate::model::key::Key;
        let d = dir("key");
        let read_key = |name: &str, tagged: &str| {
            let p = d.join(format!("{name}.wav"));
            wav(&p, 4410);
            tag_keyed(&p, tagged);
            read(&p).expect("reads").key
        };

        assert_eq!(read_key("musical", "Am"), Key::parse("Am"));
        assert_eq!(read_key("spelled", "Cmin"), Key::parse("Cm"));
        assert_eq!(
            read_key("openkey", "4m"),
            Key::parse("F#m"),
            "open key, which is what this library's dance files carry"
        );
        assert_eq!(read_key("camelot", "8A"), Key::parse("Am"));

        // Unreadable is nobody saying. Inventing one would put a claim in the
        // log that no tag ever made.
        assert_eq!(read_key("junk", "banana"), None);

        let p = d.join("bare.wav");
        wav(&p, 4410);
        assert_eq!(read(&p).expect("reads").key, None, "no tag, no claim");
    }

    /// **The totals are the second half of `3/12`** — #134.
    ///
    /// ID3 writes the position and the total in one frame; Vorbis writes them
    /// apart. Both have to read, and a `0` written by a tagger that fills
    /// every field must not read as a claim that the album is empty.
    #[test]
    fn a_total_is_read_from_either_shape_and_zero_is_nobody_saying() {
        assert_eq!(total("12", ""), Some(12), "written on its own");
        assert_eq!(total("", "3/12"), Some(12), "the second half of the pair");
        assert_eq!(total("3/12", "3/12"), Some(12), "both halves in one frame");
        assert_eq!(total(" 12 ", ""), Some(12));

        assert_eq!(total("", ""), None, "nobody said");
        assert_eq!(total("", "3"), None, "a position is not a total");
        assert_eq!(total("0", ""), None, "a disc of no tracks is not a thing");
        assert_eq!(total("", "1/0"), None);
        assert_eq!(total("many", ""), None);
    }

    /// **A release is one primary type and any number of secondary ones** —
    /// #133, and the shape the tags in this library actually carry.
    ///
    /// `album/compilation/dj-mix` is on real files here. A single flat enum
    /// has to pick one of those three and throw the other two away.
    #[test]
    fn a_release_type_is_a_primary_and_a_set_of_secondaries() {
        use crate::model::spine::{ReleaseSecondary, ReleaseType};
        let d = dir("albumtype");

        let read_type = |name: &str, tagged: &str| {
            let p = d.join(format!("{name}.wav"));
            wav(&p, 4410);
            tag_typed(&p, tagged);
            let s = read(&p).expect("reads");
            (s.release_kind, s.release_secondary)
        };

        assert_eq!(
            read_type("plain", "album"),
            (ReleaseType::Album, Default::default())
        );
        assert_eq!(
            read_type("single", "single"),
            (ReleaseType::Single, Default::default()),
            "the type #133 is about"
        );
        assert_eq!(
            read_type("comp", "album/compilation"),
            (
                ReleaseType::Album,
                [ReleaseSecondary::Compilation].into_iter().collect()
            )
        );
        assert_eq!(
            read_type("three", "album/compilation/dj-mix"),
            (
                ReleaseType::Album,
                [ReleaseSecondary::Compilation, ReleaseSecondary::DjMix]
                    .into_iter()
                    .collect()
            ),
            "a flat enum would have to drop two of these"
        );
        assert_eq!(
            read_type("cased", "EP/Live"),
            (
                ReleaseType::Ep,
                [ReleaseSecondary::Live].into_iter().collect()
            ),
            "a tag is written by whatever wrote it"
        );

        // **A word nothing here knows is dropped, not guessed at.** `Other` in
        // a set says nothing a query could act on, and the file still carries
        // the word for a build that learns it.
        assert_eq!(
            read_type("unknown", "album/sandwich"),
            (ReleaseType::Album, Default::default())
        );

        // **No tag is the default, and the default is a default.** Almost
        // everything is an album, so it is the cheapest wrong answer there is.
        let p = d.join("bare.wav");
        wav(&p, 4410);
        let s = read(&p).expect("reads");
        assert_eq!(
            (s.release_kind, s.release_secondary),
            (ReleaseType::Album, Default::default())
        );
    }

    #[test]
    fn the_pressing_and_the_album_are_read_separately() {
        let d = dir("dated");
        let p = d.join("t.wav");
        wav(&p, 4410);
        tag_dated(&p, "1996-03-16", "1995");

        let s = read(&p).expect("reads");
        assert_eq!(
            s.released,
            Some(crate::model::spine::PartialDate {
                year: 1996,
                month: Some(3),
                day: Some(16)
            }),
            "the pressing's date"
        );
        assert_eq!(
            s.first_released,
            Some(crate::model::spine::PartialDate::year(1995)),
            "the album's date — reading the pressing's here is the trap itself"
        );
    }

    #[test]
    fn a_tagged_file_reads_as_the_claim_it_makes() {
        let d = dir("tagged");
        let p = d.join("t.wav");
        wav(&p, 4410);
        let mbid = "183105b5-0000-4000-8000-000000000000";
        tag(&p, "Callisto", "Stephan Bodzin & Marc Romboy", Some(mbid));

        let s = read(&p).expect("reads");
        assert_eq!(s.title, "Callisto");
        assert_eq!(s.artist, "Stephan Bodzin & Marc Romboy");
        // The shape of the audio, read off the same properties the bitrate
        // is. The fixture is 44.1 kHz stereo by construction — see `wav`.
        assert_eq!(s.sample_rate, Some(44_100), "the rate was not read");
        // Every file in this library carries ReplayGain, and it is what a
        // fader applies — not `Encoding::gain`, which is what an analyser
        // found.
        assert!(s.levels.is_empty(), "the fixture writes no ReplayGain");
        assert_eq!(s.channels, Some(2), "the channel count was not read");
        assert_eq!(
            s.identity,
            uuid::Uuid::parse_str(mbid).ok().map(Basis::MusicBrainz)
        );
        assert_eq!(confidence_of(&s, false), Confidence::Confirmed);
    }

    /// **Ingest is total.** A file with no tags at all is still a row, with a
    /// title taken from its name.
    #[test]
    fn an_untagged_file_is_still_ingested() {
        let d = dir("untagged");
        let p = d.join("04 Unknown Thing.wav");
        wav(&p, 4410);

        let s = read(&p).expect("reads");
        assert_eq!(
            s.title, "04 Unknown Thing",
            "the name is better than a hole"
        );
        assert!(s.artist.is_empty() && s.identity.is_none());
        assert_eq!(
            confidence_of(&s, false),
            Confidence::Provisional,
            "a guess, and marked as one"
        );
    }

    /// The middle rung: no external id, but the catalogue already knows the
    /// name.
    #[test]
    fn a_known_name_is_matched_rather_than_invented() {
        let d = dir("matched");
        let p = d.join("t.wav");
        wav(&p, 4410);
        tag(&p, "Luna", "Stephan Bodzin", None);

        let s = read(&p).expect("reads");
        assert_eq!(confidence_of(&s, true), Confidence::Matched);
        assert_eq!(
            confidence_of(&s, false),
            Confidence::Provisional,
            "and unknown is provisional, not an error"
        );
    }

    /// Confidence is ordered so a review surface can sort by it, and so
    /// "at least matched" is a comparison.
    #[test]
    fn confidence_is_ordered_from_least_certain() {
        assert!(Confidence::Provisional < Confidence::Matched);
        assert!(Confidence::Matched < Confidence::Confirmed);
    }

    /// Case and space, and nothing else. Folding accents would merge
    /// `Christian Löffler` with the ASCII duplicate this library is trying to
    /// get *rid* of.
    #[test]
    fn matching_is_shallow_on_purpose() {
        assert_eq!(
            normalise("  Emmy Lou Harris "),
            normalise("emmy lou harris")
        );
        assert_ne!(
            normalise("Christian Löffler"),
            normalise("Christian Loffler"),
            "accents are a real difference here, not noise"
        );
    }

    /// **A space after a comma is never a distinction** — #153.
    ///
    /// `split_whitespace` cannot see a space that is missing, so these minted
    /// two artists from one credit. All four pairs in this library are DJ-pool
    /// tags typed by two different taggers.
    #[test]
    fn a_comma_is_spaced_the_same_way_however_it_was_typed() {
        assert_eq!(
            normalise("Sante, Sidney Charles"),
            normalise("Sante,Sidney Charles")
        );
        assert_eq!(
            normalise("Daniel Dubb , m.O.N.R.O.E."),
            normalise("Daniel Dubb,m.O.N.R.O.E."),
            "a space before the comma is the same typo from the other side"
        );
        assert_eq!(normalise("Sante,Sidney Charles"), "sante, sidney charles");
    }

    /// **The joiner's spelling is not normalised, and that is the point.**
    ///
    /// `Dom & Roland` is one person — MusicBrainz says so, `drum & bass artist
    /// Dominic Angas` — and folding `&` to `and` is the step that would make
    /// him two. A merge repairs a split; nothing repairs a fold (#152).
    #[test]
    fn the_joiner_is_left_exactly_as_written() {
        assert_ne!(
            normalise("Dom & Roland"),
            normalise("Dom and Roland"),
            "an ampersand is not a spelling of `and` for a name that contains one"
        );
        assert_ne!(
            normalise("Neko Case & Her Boyfriends"),
            normalise("Neko Case and Her Boyfriends")
        );
    }

    /// A trailing comma must not leave a separator that is itself a difference.
    #[test]
    fn an_empty_part_leaves_nothing_behind() {
        assert_eq!(normalise("Sante,"), normalise("Sante"));
        assert_eq!(normalise("Sante,,Sidney"), normalise("Sante, Sidney"));
        assert_eq!(normalise(","), "");
    }

    /// The shallowness this was already promising still holds — a comma is not
    /// a licence to start folding other things.
    #[test]
    fn a_name_with_no_comma_is_untouched_by_the_comma_rule() {
        assert_eq!(normalise("  Emmy Lou Harris "), "emmy lou harris");
        assert_ne!(
            normalise("Christian Löffler"),
            normalise("Christian Loffler")
        );
    }

    /// A file that is not audio is the one thing that is an error — there is
    /// nothing to catalogue, as against nothing to trust.
    #[test]
    fn a_file_that_is_not_audio_is_an_error() {
        let d = dir("junk");
        let p = d.join("t.wav");
        std::fs::write(&p, b"not a wav").unwrap();
        assert!(read(&p).is_err());
    }
}

#[cfg(test)]
mod external_tests {
    use super::*;

    /// **The shattering, in the shape it actually takes.** Every `2Pac feat. …`
    /// file carries a joined id; taking only a lone one made fourteen 2Pacs.
    #[test]
    fn a_joined_credit_yields_its_primary_artist() {
        let tupac = "17e4a1a4-3f7b-4dbb-8e1f-1b8b13b1c3a1";
        let nate = "6a1f1f6e-2a6a-4b5b-9e8a-1f3c5d7e9b0c";
        assert_eq!(
            primary_artist(&format!("{tupac}/{nate}")),
            uuid::Uuid::parse_str(tupac).ok(),
            "the primary artist is the first one credited"
        );
        assert_eq!(primary_artist(tupac), uuid::Uuid::parse_str(tupac).ok());
    }

    /// A field that is not an id at all yields nothing, rather than a guess.
    #[test]
    fn a_field_that_is_not_an_id_yields_nothing() {
        assert_eq!(primary_artist(""), None);
        assert_eq!(primary_artist("Various Artists"), None);
        assert_eq!(primary_artist("/"), None);
    }

    /// The real shape, from a file in the DJ crate.
    #[test]
    fn a_beatport_url_yields_its_track_id() {
        assert_eq!(
            beatport("http://www.beatport.com/track/modulo-tierra-nolah-remix/9728882"),
            Some("9728882".into())
        );
    }

    /// The slug is decoration and may contain anything, including digits — the
    /// id is the *last* segment, not the first number in the string.
    #[test]
    fn the_slug_is_not_mistaken_for_the_id() {
        assert_eq!(
            beatport("beatport.com/track/808-nirvana-2016-remix/1234"),
            Some("1234".into())
        );
    }

    /// A link to the label or the artist is not a track id, and must not be
    /// read as one — `WPUB` carries a label URL on 127 crate files.
    #[test]
    fn a_label_url_is_not_a_track() {
        assert_eq!(
            beatport("http://www.beatport.com/label/submarine-vibes/38674"),
            None
        );
        assert_eq!(beatport("https://minimalfreaks.pw/whatever"), None);
    }

    /// Found inside free text, because that is where it lives: the rips carry
    /// theirs in a comment beside the session name.
    #[test]
    fn a_video_id_is_found_in_a_comment() {
        assert_eq!(
            youtube("Western AF — https://youtu.be/1lYigC2Jn20"),
            Some("1lYigC2Jn20".into())
        );
        assert_eq!(
            youtube("https://www.youtube.com/watch?v=DKFlBd9jJ2I&t=30s"),
            Some("DKFlBd9jJ2I".into())
        );
    }

    /// Eleven characters exactly. A truncated link is not an id, and half of
    /// one would be a *different* recording that two devices could disagree
    /// about — which is the one thing a basis may never do.
    #[test]
    fn a_truncated_video_link_is_not_an_id() {
        assert_eq!(youtube("https://youtu.be/1lYigC2"), None);
        assert_eq!(youtube("youtu.be/"), None);
    }

    /// A channel or playlist link has no video id in it.
    #[test]
    fn a_link_that_is_not_a_video_yields_nothing() {
        assert_eq!(youtube("https://www.youtube.com/@WesternAF"), None);
        assert_eq!(beatport("nothing here"), None);
    }
}

#[cfg(test)]
mod cleaning {
    //! **The two readers became one** (#86), and these tests came with the
    //! helpers. They lived beside `scan::read_track`, which is gone: there is
    //! one tag reader now rather than two that could disagree about what a
    //! `-7.53 dB` or an embedded NUL means.

    use super::*;

    #[test]
    fn replaygain_units_are_stripped() {
        assert_eq!(cdb("-7.53 dB"), Some(-753));
        assert_eq!(cdb("  +2.10dB "), Some(210));
        assert_eq!(cdb("0.00"), Some(0));
        assert_eq!(cdb("loud"), None, "unparseable is absent, not nought");
    }

    #[test]
    fn a_peak_is_linear_and_never_negative() {
        assert_eq!(peak_e4("0.9885"), Some(9885));
        assert_eq!(peak_e4("1.0"), Some(10_000));
        assert_eq!(peak_e4("-0.5"), None, "a negative peak is not a peak");
    }

    /// Embedded NULs are common in ID3v2 and render as blanks.
    #[test]
    fn a_blank_field_is_absent_rather_than_empty() {
        assert_eq!(
            some(Some("  Burial ".to_string())),
            Some("Burial".to_string())
        );
        assert_eq!(some(Some(String::new())), None);
        assert_eq!(some(Some("   ".to_string())), None);
        assert_eq!(some(None), None);
    }
}
