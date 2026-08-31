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

/// Bumped when the shape changes. A file from another version is ignored
/// rather than half-read: losing one session's position is nothing, and
/// guessing at a field's meaning is how state files corrupt state.
const VERSION: u32 = 1;

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
    pub aaa_mode: String,
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
    /// Paths, never `TrackId`s. An arena index is only meaningful for as long
    /// as the cache that produced it survives; a discarded cache reassigns
    /// every one, and a queue keyed by id would silently restore *other
    /// tracks*.
    #[serde(default)]
    pub queue: Vec<PathBuf>,
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
    pub current: Option<PathBuf>,
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

    fn sample() -> Session {
        Session {
            version: VERSION,
            queue: vec![PathBuf::from("/m/a.mp3"), PathBuf::from("/m/b.flac")],
            open_playlist: Some("mix".into()),
            view: Some(2),
            current: Some(PathBuf::from("/m/c.opus")),
            position_secs: 42.5,
            modes: Modes {
                shuffle: true,
                repeat: false,
                repeat_current: false,
                continue_playing: true,
                aaa_mode: "album".into(),
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
