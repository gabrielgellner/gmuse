//! One holder at a time, enforced by the kernel rather than by a convention.
//!
//! **A crate of its own since #257**, and the reason is a dependency rather than a size:
//! the engine locks its socket, the librarian locks the log it appends to, and `main` locks
//! the process. The engine may no longer depend on `gmuse-library`, where this lived, and
//! the only crate below all three was `gmuse-ids` — whose charter forbids anything that
//! walks a disk, which writing a pid into a file is.
//!
//! So it names no library concept and cannot: what it locks is a path somebody hands it.
//!
//! Two gmuse processes on one machine are not a corruption risk — that was
//! measured before this was written, and the answer is narrower than it looks:
//!
//! - **Ratings and plays are safe.** They are appended to `events-<machine>.log`
//!   as one short `write_all` on a file opened `O_APPEND`, so two writers
//!   interleave whole lines and never tear one. The append-only design that
//!   exists for syncing two *machines* happens to make two *processes* safe too.
//! - **The library cache is safe**, being a whole-file write of something
//!   regenerable.
//! - **`session.toml` is not.** It is written whole, on quit, so whichever
//!   instance you quit last decides what the next launch restores.
//! - **The audio device is not.** Both instances open it and both play, which
//!   is the failure you actually notice.
//!
//! So the lock exists for the position file and for the speakers, and it is
//! deliberately not sold as protecting the history, which does not need it.
//!
//! **Why an OS lock and not a PID file.** `File::try_lock` is released by the
//! kernel when the process ends, *however* it ends — including `kill -9`, a
//! panic, and a terminal closing out from under a tmux popup. A PID file needs
//! a liveness check, a staleness rule, and cleanup on every exit path, and is
//! still racy between the check and the write. This has none of those, holds no
//! dependency, and needs no `unsafe`.
//!
//! The lock lives in `$XDG_STATE_HOME`, **not** in `data_dir`. `data_dir` is
//! the shared, possibly-synced directory — a lock there would claim to speak
//! for other machines, which it cannot, and advisory locking over a network
//! filesystem is not something to rely on. One machine, one lock.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use thiserror::Error;

/// The lock file's name inside the state directory.
pub const NAME: &str = "lock";

/// A held lock. Dropping it — or the process ending — releases it.
///
/// The [`File`] is the lock: it is kept alive for exactly as long as the lock
/// should be held, so there is no `release` to forget to call.
#[derive(Debug)]
pub struct Lock {
    _file: File,
}

/// Why a lock could not be taken.
#[derive(Debug, Error)]
pub enum LockError {
    /// Another gmuse holds it. `pid` is what that process wrote, when readable.
    #[error("another gmuse is already running{}", match pid {
        Some(p) => format!(" (pid {p})"),
        None => String::new(),
    })]
    Held { pid: Option<u32>, path: PathBuf },

    /// The lock file could not be opened at all.
    #[error("could not open the lock file {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// The lock file inside a state directory — `<state>/lock`.
///
/// **Takes the directory rather than working it out** — #213. The first cut of this
/// crate copied `config::default_state_dir`'s XDG rule here, because
/// `gmuse-engine` has no config module. Two copies of one rule is the drift this
/// day was spent removing, and they had already diverged in the copying: one
/// filtered a relative `XDG_STATE_HOME` and the other did not.
///
/// So the root passes what it already computes, and there is one rule.
pub fn path_in(state_dir: &std::path::Path) -> PathBuf {
    state_dir.join(NAME)
}

/// Take the lock, or say who has it.
///
/// The pid is written as the file's *contents* purely so the refusal can name
/// the other process. It is never read to decide whether the lock is held —
/// that is the kernel's answer, and a pid in a file is exactly the stale,
/// racy signal this design avoids.
pub fn acquire(path: &Path) -> Result<Lock, LockError> {
    let io = |source| LockError::Io {
        path: path.to_path_buf(),
        source,
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    // Not `truncate`: truncating before the lock is taken would blank the pid
    // of the instance that legitimately holds it.
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(io)?;

    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            return Err(LockError::Held {
                pid: read_pid(&mut file),
                path: path.to_path_buf(),
            });
        }
        Err(TryLockError::Error(source)) => return Err(io(source)),
    }

    // Ours now. Leave our pid for whoever is refused next.
    let _ = file.set_len(0);
    let _ = file.seek(SeekFrom::Start(0));
    let _ = write!(file, "{}", std::process::id());
    let _ = file.flush();

    Ok(Lock { _file: file })
}

/// Whatever pid the holder wrote, if it is still legible.
///
/// Best-effort by design: a missing or garbled pid costs a friendlier message
/// and nothing else, so every failure here is swallowed rather than raised.
fn read_pid(file: &mut File) -> Option<u32> {
    file.seek(SeekFrom::Start(0)).ok()?;
    let mut text = String::new();
    file.read_to_string(&mut text).ok()?;
    text.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "gmuse-lock-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("lock")
    }

    #[test]
    fn a_second_acquire_is_refused_while_the_first_is_held() {
        let path = temp();
        let _held = acquire(&path).expect("first");
        match acquire(&path) {
            Err(LockError::Held { .. }) => {}
            other => panic!("expected Held, got {other:?}"),
        }
    }

    #[test]
    fn releasing_lets_the_next_one_in() {
        let path = temp();
        let first = acquire(&path).expect("first");
        drop(first);
        acquire(&path).expect("second should get it after the first released");
    }

    #[test]
    fn the_refusal_names_the_holders_pid() {
        let path = temp();
        let _held = acquire(&path).expect("first");
        match acquire(&path) {
            Err(LockError::Held { pid, .. }) => {
                assert_eq!(
                    pid,
                    Some(std::process::id()),
                    "the pid on disk should be the holder's"
                );
            }
            other => panic!("expected Held, got {other:?}"),
        }
    }

    #[test]
    fn a_stale_file_from_a_dead_process_does_not_block() {
        // The whole point of an OS lock: a leftover file with someone else's
        // pid in it is not a lock, so nothing has to detect staleness.
        let path = temp();
        std::fs::write(&path, "999999").unwrap();
        acquire(&path).expect("a file nobody holds must not block");
    }
}
