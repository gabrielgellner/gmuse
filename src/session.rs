//! Where you were, restored on the next launch.
//!
//! §1 says the interface has to be usable when it is not in front of you; a
//! player that forgets its queue and position every time it starts is the
//! opposite of that.
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
    /// **Identities.**
    ///
    /// It held paths, for a good reason that has been overtaken: a `CopyId`
    /// is an arena index, only meaningful while the cache that produced it
    /// survives, so a queue keyed by one would silently restore *other
    /// tracks*. A path was the only durable thing to hand.
    ///
    /// A recording id is durable in a way a path is not — it survives a
    /// rename, a move, and being carried to another machine, none of which a
    /// path survives. And it is what everything else durable here is keyed by.
    #[serde(default)]
    pub queue: Vec<Id<Recording>>,
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

#[cfg(test)]
mod tests {
    use super::*;

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
            queue: vec![Id::derived(b"a"), Id::derived(b"b")],
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
