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
/// title falls back to the file stem, which is a playable, sortable row rather
/// than a hole.
pub fn read(path: &Path) -> Result<Scanned, IngestError> {
    let bad = |message: String| IngestError::Unreadable {
        path: path.display().to_string(),
        message,
    };

    let audio = crate::audio_id::of(path).map_err(|e| bad(e.to_string()))?;
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
        disc: number(&get(ItemKey::DiscNumber)),
        track: number(&get(ItemKey::TrackNumber)),
        mb_release_group: uuid::Uuid::parse_str(&get(ItemKey::MusicBrainzReleaseGroupId)).ok(),
        format: format_of(&tagged),
        // The audio bitrate where the container reports one, else the overall
        // rate. A verdict compares a cutoff against what the file *claims*, so
        // the claim has to come from the file rather than from a guess.
        length_ms: tagged.properties().duration().as_millis() as u32,
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

pub fn confidence_of(scan: &Scanned, matches_known_name: bool) -> Confidence {
    if matches!(scan.identity, Some(Basis::MusicBrainz(_))) {
        Confidence::Confirmed
    } else if matches_known_name {
        Confidence::Matched
    } else {
        Confidence::Provisional
    }
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
pub fn normalise(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
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
