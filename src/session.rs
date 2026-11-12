//! Where you were, restored on the next launch.
//!
//! §1 says the interface has to be usable when it is not in front of you; a
//! player that forgets its place every time it starts is the opposite of that.
//!
//! **Not the queue, since #321.** A queue is for now, and a saved playlist is
//! what lasts — `:queue save` is how one becomes the other. A session written
//! before that still has a `queue` field, which serde skips like any field it
//! does not know.
//!
//! **This is state, not cache.** It cannot be rebuilt by rescanning, so it does
//! not live in `library.postcard` — the Decision Log records what happened the
//! last time durable data shared a lifetime with derived data. Writes go
//! through a temporary file and a rename, so a crash mid-write leaves the
//! previous session intact rather than a truncated one.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::entity::Encoding;
use crate::model::id::Id;
use crate::model::spine::Recording;

/// Bumped when the shape changes. A file from another version is ignored
/// rather than half-read: losing one session's position is nothing, and
/// guessing at a field's meaning is how state files corrupt state.
const VERSION: u32 = 2;

/// What was playing, and which copy of it — issue #112.
///
/// **A pair, because a row names a copy** (#78). Resuming should reopen the
/// file that was open — the FLAC, not whichever the canonical rule would pick
/// now — and the copy is what says which. Absent for a recording this device
/// no longer holds, which resumes through `open` rather than refusing: a
/// drive being unmounted is not the same as a file being gone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Playing {
    pub recording: Id<Recording>,
    #[serde(default)]
    pub copy: Option<Id<Encoding>>,
}

/// `$XDG_STATE_HOME/gmuse/session.toml`, beside the audit log.
pub fn default_path() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("state")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("gmuse").join("session.toml")
}

/// The playback modes, as text.
///
/// Stored by name rather than by serde-deriving the enums: the file is meant to
/// be readable and hand-editable, and a rename in the code should not silently
/// change what a saved file means.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Modes {
    pub shuffle: bool,
    pub repeat: bool,
    pub repeat_current: bool,
    pub continue_playing: bool,
    pub scope: String,
    pub replaygain: String,
    pub active: String,
    /// The shuffle seed.
    ///
    /// Shuffle here is a *seeded permutation* (§5), so the order being heard is
    /// a function of this and the pool. Without it, resuming mid-shuffle
    /// reseeds and quietly loses your place in the sequence while appearing to
    /// carry on.
    pub rng_state: u64,
}

/// One session's worth of "where I was".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub version: u32,
    /// Which playlist view 3 had open, by **name**.
    ///
    /// The name, not the tracks: every playlist is a file or a rule now, so
    /// the contents have a home of their own and a copy here could only go
    /// stale. Reopening by name gets whatever the file says today, which is
    /// what you would want after editing it in another program.
    #[serde(default)]
    pub open_playlist: Option<String>,
    /// Which view was in front, by number.
    ///
    /// By number rather than by name because that is what `:view 3` and `3gt`
    /// mean, and what the tab strip shows. Without it you did not land where
    /// you left: reopening a playlist switches to view 3, so every launch
    /// arrived there whatever you had actually been looking at.
    #[serde(default)]
    pub view: Option<u8>,
    #[serde(default)]
    pub current: Option<Playing>,
    #[serde(default)]
    pub position_secs: f64,
    pub modes: Modes,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("writing {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("serialising the session: {0}")]
    Encode(#[from] toml::ser::Error),
}

/// Read a saved session. `None` for anything unreadable, unparseable, or from
/// another version — all of which mean "start fresh", never an error worth
/// interrupting a launch for.
pub fn load(path: &Path) -> Option<Session> {
    let text = std::fs::read_to_string(path).ok()?;
    let session: Session = toml::from_str(&text).ok()?;
    (session.version == VERSION).then_some(session)
}

/// Write the session, atomically.
///
/// Temp file then rename: a crash or a full disk leaves the previous session
/// in place rather than a truncated one. Overwriting in place is how the
/// library roots were lost once already.
pub fn save(path: &Path, session: &Session) -> Result<(), SessionError> {
    let mut session = session.clone();
    session.version = VERSION;
    let text = toml::to_string_pretty(&session)?;

    write_whole(path, &text)
}

/// Temp file then rename, so a reader sees the old file or the new one and never half of
/// either. Shared by the view's session and the engine's.
fn write_whole(path: &Path, text: &str) -> Result<(), SessionError> {
    let write = |path: &Path| -> Result<(), std::io::Error> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text.as_bytes())?;
        std::fs::rename(&tmp, path)
    };
    write(path).map_err(|source| SessionError::Write {
        path: path.to_path_buf(),
        source,
    })
}

/// Bumped when [`Left`]'s shape changes, for the reason [`VERSION`] is.
const LEFT_VERSION: u32 = 1;

/// **Where the engine was, so it can come back there with no view open.**
///
/// [`Session`] is a view's: which tab, which playlist, and — for a view that owns its
/// player — what was playing. An attached view restores nothing about playback (#219),
/// because playback is the engine's, and until this existed the engine remembered none
/// of it: one that was restarted came up with nothing loaded and waited for a view to
/// tell it what to play. That is the wrong way round for the case the engine exists
/// for, which is music with no view open — a media key pressed on a laptop that has
/// just woken has nothing to act on.
///
/// **The recording and the position, and nothing else.** Not the programme: #321
/// decided a queue is for now, and that stands — the engine opens a fresh programme
/// around this track exactly as a play gesture would. Not the modes: the librarian
/// already keeps those beside the log. Not the copy: which file plays is the
/// librarian's to choose on the day, and a drive that is unmounted today is not a
/// reason to forget the track.
///
/// **In the state directory, beside the view's session**, because it is the same kind
/// of thing: it cannot be rebuilt by scanning, and it belongs to this machine. The
/// data directory may be synced, and where one machine stopped listening is not
/// something to hand another.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Left {
    pub version: u32,
    pub recording: Id<Recording>,
    /// Into the track. Zero for one that had stopped: D-9, stopped is paused at the top.
    pub position_secs: f64,
}

impl Left {
    pub fn new(recording: Id<Recording>, position: std::time::Duration) -> Self {
        Self {
            version: LEFT_VERSION,
            recording,
            position_secs: position.as_secs_f64(),
        }
    }

    /// Where to seek to. A file is hand-editable, so a negative or non-finite number
    /// is read as the top of the track rather than passed to a seek.
    pub fn position(&self) -> std::time::Duration {
        std::time::Duration::try_from_secs_f64(self.position_secs).unwrap_or_default()
    }
}

/// `$XDG_STATE_HOME/gmuse/engine.toml`, beside [`default_path`]'s `session.toml`.
pub fn left_path() -> PathBuf {
    default_path().with_file_name("engine.toml")
}

/// Read where the engine was. `None` for anything unreadable or from another version,
/// which means "start with nothing loaded" — what every engine did before this.
pub fn load_left(path: &Path) -> Option<Left> {
    let text = std::fs::read_to_string(path).ok()?;
    let left: Left = toml::from_str(&text).ok()?;
    (left.version == LEFT_VERSION).then_some(left)
}

/// Write where the engine is, atomically.
pub fn save_left(path: &Path, left: &Left) -> Result<(), SessionError> {
    write_whole(path, &toml::to_string_pretty(left)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **What the engine wrote is what it reads**, and a file it cannot trust is no file.
    #[test]
    fn where_the_engine_was_survives_the_file() {
        let dir = std::env::temp_dir().join(format!("gmuse-left-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("state").join("engine.toml");
        assert_eq!(load_left(&path), None, "nothing written yet");

        let recording = Id::from_uuid(uuid::Uuid::from_u128(0x1234));
        let left = Left::new(recording, std::time::Duration::from_millis(90_500));
        save_left(&path, &left).expect("the directory is made on the way");
        let read = load_left(&path).expect("it was just written");
        assert_eq!(read.recording, recording);
        assert_eq!(read.position(), std::time::Duration::from_millis(90_500));

        // Another version is refused rather than half-read.
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("version = 1"),
            "the fixture below edits this: {text}"
        );
        std::fs::write(&path, text.replace("version = 1", "version = 99")).unwrap();
        assert_eq!(load_left(&path), None);

        // And a position nobody could seek to is the top of the track, not a panic.
        let mut odd = left.clone();
        odd.position_secs = -3.0;
        assert_eq!(odd.position(), std::time::Duration::ZERO);
        odd.position_secs = f64::NAN;
        assert_eq!(odd.position(), std::time::Duration::ZERO);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A session written by an older gmuse is discarded, not half-read.**
    ///
    /// It used to hold paths, and losing where you were is the one loss this
    /// program already accepts — where a partial read is not: a queue that
    /// silently came back three tracks short would look like the queue.
    ///
    /// **What refuses it is the type, not the version.** A path is not a
    /// UUID, so the parse fails before the version is looked at, and this
    /// test passes with `VERSION` left at 1. The bump is still right — it is
    /// what will refuse the next change, where the shapes happen to stay
    /// compatible and only the meaning moves — but nothing here proves it
    /// works, and a reader should not think otherwise.
    #[test]
    fn a_session_from_before_identities_is_refused() {
        let dir = std::env::temp_dir().join("gmuse-session-old");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.toml");
        std::fs::write(
            &path,
            "version = 1\nqueue = [\"/m/a.mp3\"]\ncurrent = \"/m/b.mp3\"\nposition_secs = 0.0\n",
        )
        .unwrap();

        assert!(
            load(&path).is_none(),
            "a path-keyed session was read as though it named identities"
        );
    }

    fn sample() -> Session {
        Session {
            version: VERSION,
            open_playlist: Some("mix".into()),
            view: Some(2),
            current: Some(Playing {
                recording: Id::derived(b"c"),
                copy: Some(Id::derived(b"copy")),
            }),
            position_secs: 42.5,
            modes: Modes {
                shuffle: true,
                repeat: false,
                repeat_current: false,
                continue_playing: true,
                scope: "album".into(),
                replaygain: "auto".into(),
                active: "library".into(),
                rng_state: 0x1234_5678_9abc_def0,
            },
        }
    }

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gmuse-session-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_session_round_trips() {
        let path = tmpdir("roundtrip").join("session.toml");
        save(&path, &sample()).unwrap();
        assert_eq!(load(&path), Some(sample()));
    }

    /// The seed is the difference between resuming a shuffle and restarting
    /// one, so it has to survive the file exactly — a `u64` that loses its top
    /// bits to a float or a signed integer would reseed silently.
    #[test]
    fn the_shuffle_seed_survives_exactly() {
        let path = tmpdir("seed").join("session.toml");
        let mut s = sample();
        s.modes.rng_state = u64::MAX;
        save(&path, &s).unwrap();
        assert_eq!(load(&path).unwrap().modes.rng_state, u64::MAX);
    }

    #[test]
    fn a_missing_or_corrupt_file_is_simply_no_session() {
        let dir = tmpdir("bad");
        assert_eq!(load(&dir.join("nothing.toml")), None);
        let junk = dir.join("junk.toml");
        std::fs::write(&junk, "this is not toml {{{").unwrap();
        assert_eq!(load(&junk), None);
    }

    /// A file from another version is ignored rather than half-read. Guessing
    /// at a field's meaning across a format change is how state files corrupt
    /// state.
    #[test]
    fn a_file_from_another_version_is_ignored() {
        let path = tmpdir("version").join("session.toml");
        save(&path, &sample()).unwrap();
        let text = std::fs::read_to_string(&path)
            .unwrap()
            .replace(&format!("version = {VERSION}"), "version = 999");
        std::fs::write(&path, text).unwrap();
        assert_eq!(load(&path), None);
    }

    /// The whole point of the temp-and-rename: a good file is never replaced
    /// by a broken one.
    #[test]
    fn a_failed_write_leaves_the_previous_session_intact() {
        let dir = tmpdir("atomic");
        let path = dir.join("session.toml");
        save(&path, &sample()).unwrap();

        // A directory where the temp file wants to be: the write fails, and
        // the rename never happens.
        std::fs::create_dir(path.with_extension("toml.tmp")).unwrap();
        let mut next = sample();
        next.position_secs = 999.0;
        assert!(save(&path, &next).is_err());

        assert_eq!(load(&path), Some(sample()), "the good file was damaged");
    }
}
