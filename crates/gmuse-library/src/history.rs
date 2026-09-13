//! Per-track history: how often a track has been played, when, and when it
//! first appeared (issue #13).
//!
//! **Durable, not derived.** It cannot be rebuilt by rescanning, so it lives
//! under `data_dir` rather than in the cache — the Decision Log records what
//! happened the one time durable data shared a lifetime with derived data.
//! Written temp-then-rename.
//!
//! Under `data_dir` rather than `$XDG_STATE_HOME`, where it started, because
//! ratings and play counts are the one thing here worth backing up and the
//! state directory is exactly what a backup tool skips. The session and the
//! audit log stay behind: losing where you were is not a loss.
//!
//! **Keyed by identity, not by path and not by `CopyId`.**
//!
//! An arena index is meaningful only while the cache that produced it
//! survives, so a history keyed by *that* would silently reattach to different
//! tracks — which is what the original warning here was about, and it still
//! holds.
//!
//! A path is no better for the opposite reason: it is a property of a device's
//! copy rather than of the track, and it does not even survive a rename on one
//! machine. A renumber of one album stranded three entries here, one of them a
//! four-star rating on a file that still existed (§13, 2026-09-01).
//!
//! So the key is [`key_for`] — the recording id the model would mint, computed
//! from the file alone.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::audio_id::AudioId;
use crate::model::basis::Basis;
use crate::model::entity::Recording;
use crate::model::id::Id;

const VERSION: u32 = 1;

/// Four minutes, or half the track — last.fm's rule, and the usual one.
const LONG_ENOUGH: Duration = Duration::from_secs(240);

/// The identity history hangs off, computable from a file alone.
///
/// **The same id the model would mint**, by the same two rules: a MusicBrainz
/// recording id where the tags carry one, otherwise the audio's own hash. That
/// matters because history has to survive being migrated exactly once — a
/// second key would mean a second migration of the one file in this project
/// that cannot be rebuilt.
///
/// ## What it fixes
///
/// A path is a property of *a device's copy* of a track, not of the track. A
/// renumber of one album stranded three history entries here, one of them a
/// four-star rating on a file that still existed. Keyed by identity, a rename,
/// a move and a retag all leave history exactly where it was.
///
/// ## What it does not fix
///
/// For the 8% with no MusicBrainz id, the key derives from the bytes — so a
/// re-encode still loses their history. That is the case with nothing to hold
/// it by, and #57's fingerprints are what would close it.
#[allow(
    dead_code,
    reason = "the store was its only caller and is gone (#86) — the library answers `recording_of` now. Kept because the once-only migration still keys history by it, and because deleting the string form would leave `Id::of_basis` spelled out wherever a key is needed"
)]
pub fn key_for(mb_recording: Option<uuid::Uuid>, audio: Option<AudioId>) -> Option<String> {
    recording_id_for(mb_recording, audio).map(|id| id.to_string())
}

/// The same identity, typed.
///
/// [`key_for`] is what a TOML map key has to be; this is what everything else
/// should use, and having both means the string form exists in one place
/// rather than being spelled out wherever a key is needed.
#[allow(
    dead_code,
    reason = "the store was its only caller and is gone (#86) — the library answers `recording_of` now. Kept because the once-only migration still keys history by it, and because deleting the string form would leave `Id::of_basis` spelled out wherever a key is needed"
)]
pub fn recording_id_for(
    mb_recording: Option<uuid::Uuid>,
    audio: Option<AudioId>,
) -> Option<Id<Recording>> {
    // **Through `of_basis`, and that is the point of #89.** This function used
    // to mint on its own, so there were two ladders — this one and `ingest`'s
    // — that agreed only for as long as nobody edited either. Changing the
    // model's derivation and not this one made nine UI tests fail with a
    // rating attached to an id the librarian had never heard of, which is
    // exactly the failure two ladders produce.
    let basis = match (mb_recording, audio) {
        (Some(mbid), _) => Basis::MusicBrainz(mbid),
        (None, Some(a)) => Basis::Audio(a),
        // Neither: nothing stable to hang history on, and inventing one would
        // give an identity that changes on the next scan.
        (None, None) => return None,
    };
    Some(Id::<Recording>::of_basis(&basis))
}

#[cfg(test)]
mod key_tests {
    use super::*;

    fn audio(hash: u128) -> AudioId {
        AudioId { scheme: 1, hash }
    }

    /// **The bug this exists to end.** A rename moves the path and nothing
    /// else, so the key does not move — and the four-star rating stays where
    /// it was put.
    #[test]
    fn a_rename_cannot_strand_history() {
        let a = audio(0x8fb0_dcfb);
        assert_eq!(key_for(None, Some(a)), key_for(None, Some(a)));
    }

    /// A retag moves no bytes, so it moves no history. `audio_id` hashes the
    /// encoded packets and nothing else, which is what makes that true.
    #[test]
    fn a_retag_cannot_strand_history() {
        let mbid = uuid::Uuid::from_u128(0x1831_05b5);
        let a = audio(1);
        assert_eq!(
            key_for(Some(mbid), Some(a)),
            key_for(Some(mbid), Some(audio(2))),
            "an external id outranks the bytes, so even a re-encode keeps it"
        );
    }

    /// Different songs get different keys, or history would pool.
    #[test]
    fn two_songs_are_two_keys() {
        assert_ne!(key_for(None, Some(audio(1))), key_for(None, Some(audio(2))));
        assert_ne!(
            key_for(Some(uuid::Uuid::from_u128(1)), None),
            key_for(Some(uuid::Uuid::from_u128(2)), None)
        );
    }

    /// **The same id the model mints**, and this test earned its keep: when
    /// the model moved to `of_basis` and this file did not, it failed
    /// alongside nine UI tests whose ratings had attached to ids the librarian
    /// had never heard of. That is what two ladders do.
    #[test]
    fn the_key_agrees_with_what_ingest_would_mint() {
        let mbid = uuid::Uuid::from_u128(0x1831_05b5);
        assert_eq!(
            key_for(Some(mbid), None).unwrap(),
            Id::<Recording>::of_basis(&Basis::MusicBrainz(mbid)).to_string()
        );
        let a = audio(0xdead_beef);
        assert_eq!(
            key_for(None, Some(a)).unwrap(),
            Id::<Recording>::of_basis(&Basis::Audio(a)).to_string()
        );
    }

    /// Nothing stable to hang history on means no key, rather than one that
    /// would change on the next scan.
    #[test]
    fn a_file_with_nothing_to_identify_it_gets_no_key() {
        assert_eq!(key_for(None, None), None);
    }
}

/// `<data_dir>/history.toml`.
pub fn path_in(data_dir: &Path) -> PathBuf {
    data_dir.join("history.toml")
}

/// Where the history lived before it moved to `data_dir`.
///
/// Kept so [`migrate`] can find it. Not a fallback read: a version that
/// silently started from an empty history would look exactly like the ratings
/// being gone, which is the failure this whole move is meant to prevent.
fn legacy_path() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("state")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("gmuse").join("history.toml")
}

/// Move a pre-`data_dir` history into place, if there is one and nothing is
/// there yet. Returns where it came from, so the caller can say so.
///
/// Moved rather than copied, so there is one file and no question later about
/// which is current. A failed rename falls back to copy-then-remove, because
/// `data_dir` may well be on another disk — pointing it at a NAS is the whole
/// reason the setting exists.
pub fn migrate(to: &Path) -> Option<PathBuf> {
    migrate_from(&legacy_path(), to)
}

/// The half of [`migrate`] that does not read the environment.
///
/// Split out so it is testable: mutating `XDG_STATE_HOME` to exercise the real
/// entry point would race every other test in the process.
fn migrate_from(from: &Path, to: &Path) -> Option<PathBuf> {
    if to.exists() || !from.exists() || from == to {
        return None;
    }
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir).ok()?;
    }
    if std::fs::rename(from, to).is_err() {
        std::fs::copy(from, to).ok()?;
        // A copy that cannot be cleaned up is still a successful migration;
        // the stale original is harmless because nothing reads it.
        let _ = std::fs::remove_file(from);
    }
    Some(from.to_path_buf())
}

/// Seconds since the epoch. Passed in rather than read, so the callers that
/// record are testable without a clock.
/// The same instant in **milliseconds**, which is what the clock stamps in.
///
/// Two units, named apart, because mixing them is not a rounding error: a
/// stamp made from seconds sits a thousandfold below one made from
/// milliseconds, so every launch reports implausible drift and every write
/// sorts before everything already in the log. That happened, and it happened
/// where no test could see it — the tests pass their own `now`, so they were
/// self-consistent in whichever unit they chose.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Seconds since the epoch — what a *fact* records, and what the filter
/// language's `days_since_*` compares against.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Whether a track was played rather than skipped past.
///
/// Half of it, or four minutes, whichever comes first. A track of unknown
/// length has no half to take, so it has to earn the full four minutes —
/// otherwise a zero duration would count a play the instant it started.
pub fn counts_as_played(position: Duration, duration: Duration) -> bool {
    let threshold = if duration.is_zero() {
        LONG_ENOUGH
    } else {
        (duration / 2).min(LONG_ENOUGH)
    };
    position >= threshold
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    #[serde(default)]
    pub plays: u32,
    #[serde(default)]
    pub last_played: Option<i64>,
    /// Stars, 1 to 5. `None` is unrated, which is distinct from a low rating
    /// and is what `!rating?` finds.
    ///
    /// **Ours, not the file's.** §1 keeps tag editing out of scope, so a
    /// rating lives here rather than in a `POPM` frame. That costs
    /// portability — trollstov carves out exactly this exception for exactly
    /// that reason — and buys never being the program that rewrote someone's
    /// library. See issue #14 if that trade is ever revisited.
    #[serde(default)]
    pub rating: Option<u8>,
    /// When gmuse first saw the file.
    ///
    /// Not the file's mtime, which moves for reasons that have nothing to do
    /// with the music — writing ReplayGain tags across the library reset every
    /// one of them in an afternoon.
    pub added: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct History {
    #[serde(default)]
    version: u32,
    /// Sorted, so the file does not churn between saves for no reason.
    #[serde(default)]
    entries: BTreeMap<String, Entry>,
    #[serde(skip)]
    dirty: bool,
}

impl History {
    /// Read the history. Anything unreadable, unparseable, or from another
    /// version starts empty — losing counts is bad, but guessing at a field's
    /// meaning across a format change is how a store corrupts itself.
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        match toml::from_str::<History>(&text) {
            Ok(h) if h.version == VERSION => h,
            _ => Self::default(),
        }
    }

    /// Every entry, by the path it was filed under.
    ///
    /// Exposed for the one job left to this file: carrying `added` across to
    /// the identity-keyed log (#81). Nothing at runtime should want this —
    /// iterating a path-keyed history is the shape being retired.
    pub fn by_path(&self) -> impl Iterator<Item = (&str, &Entry)> {
        self.entries.iter().map(|(k, e)| (k.as_str(), e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gmuse-history-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// The one job left to this file: hand the base's `added` dates to the
    /// migration, which turns them into `Fact::RecordingAcquired` (#81).
    #[test]
    fn the_base_reads_back_for_the_migration() {
        let dir = tmpdir("read-base");
        let path = path_in(&dir);
        std::fs::write(
            &path,
            format!(
                "version = {VERSION}\n\n\
                 [entries.\"/m/a.mp3\"]\nplays = 3\nrating = 4\nadded = 100\n\n\
                 [entries.\"/m/b.mp3\"]\nplays = 0\nadded = 200\n"
            ),
        )
        .unwrap();

        let h = History::load(&path);
        let entries: Vec<(&str, i64)> = h.by_path().map(|(k, e)| (k, e.added)).collect();
        assert_eq!(entries, [("/m/a.mp3", 100), ("/m/b.mp3", 200)]);
        assert_eq!(h.by_path().next().unwrap().1.rating, Some(4));
    }

    /// A file from another version, or no file at all, reads as empty rather
    /// than as a guess. Guessing at a field's meaning across a format change
    /// is how a store corrupts itself.
    #[test]
    fn an_unreadable_base_is_empty_rather_than_wrong() {
        let dir = tmpdir("bad-base");
        assert_eq!(History::load(&path_in(&dir)).by_path().count(), 0);

        let path = path_in(&dir);
        std::fs::write(&path, "version = 999\n[entries.\"/m/a.mp3\"]\nadded = 1\n").unwrap();
        assert_eq!(
            History::load(&path).by_path().count(),
            0,
            "a future version was read as though it were this one"
        );
    }
}
