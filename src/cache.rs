//! Persisting what a scan found, between runs.
//!
//! postcard, because the cache is a private format read only by the version of
//! gmuse that wrote it. There is no compatibility story: a format change bumps
//! `FORMAT_VERSION` and the next run rescans. That is a deliberate trade — a
//! rescan costs seconds, and a migration path for a derived cache would cost
//! more to maintain than it could ever save.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::scan::Holdings;

/// Bump on any change to `Held`, `Holdings`, `Scanned`, or this envelope.
const FORMAT_VERSION: u32 = 8;
const MAGIC: &[u8; 5] = b"gmuse";

#[derive(Serialize, Deserialize)]
struct Envelope {
    version: u32,
    roots: Vec<PathBuf>,
    store: Holdings,
    /// Which catalogs were active. Persisted so a choice survives a restart;
    /// the *definitions* come from the config, not from here.
    #[serde(default)]
    active: crate::catalog::Active,
}

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("cache io: {0}")]
    Io(#[from] io::Error),
    #[error("cache could not be written: {0}")]
    Encode(String),
}

/// Where the cache lives: `$XDG_CACHE_HOME/gmuse/library.postcard`, falling
/// back to `~/.cache`. cmus uses the XDG layout on macOS too, and matching it
/// keeps a dotfile-synced setup working across machines.
pub fn default_path() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("gmuse").join("library.postcard")
}

/// What came back from a cache read.
#[derive(Debug)]
pub enum Outcome {
    /// No cache file yet.
    Missing,
    /// A cache existed but could not be used — wrong version, or corrupt.
    ///
    /// Distinguished from `Missing` because it costs the user a rescan and
    /// they should be told, rather than silently starting from nothing.
    Discarded,
    Loaded {
        store: Holdings,
        roots: Vec<PathBuf>,
        active: crate::catalog::Active,
    },
}

/// Load a cache. Never an error the caller has to handle: a cache that cannot
/// be read just means a rescan.
pub fn load(path: &Path) -> Result<Outcome, CacheError> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Outcome::Missing),
        Err(e) => return Err(e.into()),
    };

    if bytes.len() < MAGIC.len() || &bytes[..MAGIC.len()] != MAGIC {
        return Ok(Outcome::Discarded);
    }

    match postcard::from_bytes::<Envelope>(&bytes[MAGIC.len()..]) {
        Ok(env) if env.version == FORMAT_VERSION => Ok(Outcome::Loaded {
            // **Nothing to rebuild.** There were two derived indices to
            // restore here — a path index and an id index — and neither was
            // stored, so that an arena handle could not be serialised at all.
            // A holding is keyed by its path and a row by its bytes, so what
            // comes off disk is already what is wanted (#86).
            store: env.store,
            roots: env.roots,
            active: env.active,
        }),
        // Wrong version or unparseable: not an error, just a rescan.
        Ok(_) | Err(_) => Ok(Outcome::Discarded),
    }
}

/// Write the cache, creating the directory if needed.
///
/// Writes to a temporary file and renames, so an interrupted save leaves the
/// previous cache intact rather than a truncated one.
pub fn save(
    path: &Path,
    store: &Holdings,
    roots: &[PathBuf],
    active: &crate::catalog::Active,
) -> Result<(), CacheError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }

    let envelope = Envelope {
        version: FORMAT_VERSION,
        roots: roots.to_vec(),
        store: store.clone(),
        active: active.clone(),
    };

    let mut bytes = MAGIC.to_vec();
    bytes.extend_from_slice(
        &postcard::to_allocvec(&envelope).map_err(|e| CacheError::Encode(e.to_string()))?,
    );

    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::instance::Scanned;
    use crate::scan::Held;

    fn held(at: &str, title: &str, artist: &str, mtime: i64, audio: u128) -> Held {
        Held {
            at: PathBuf::from(at),
            mtime,
            size: 4_000_000,
            seen: Scanned {
                audio: crate::audio_id::AudioId {
                    scheme: 1,
                    hash: audio,
                },
                title: title.into(),
                artist: artist.into(),
                length_ms: 154_000,
                ..Default::default()
            },
        }
    }

    fn sample_store() -> Holdings {
        let mut s = Holdings::new();
        s.insert(held("/m/a.mp3", "Night Bus", "Burial", 42, 1));
        s.insert(held("/m/b.opus", "Years", "", 43, 2));
        s
    }

    #[test]
    fn round_trips() {
        let dir = std::env::temp_dir().join("gmuse-cache-test-rt");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("library.postcard");

        let store = sample_store();
        let roots = vec![PathBuf::from("/m")];
        save(&path, &store, &roots, &crate::catalog::Active::All).unwrap();

        let Outcome::Loaded {
            store: loaded,
            roots: loaded_roots,
            ..
        } = load(&path).unwrap()
        else {
            panic!("should have loaded")
        };
        assert_eq!(loaded_roots, roots);
        assert_eq!(loaded.len(), 2);
        let back = loaded
            .get(Path::new("/m/a.mp3"))
            .expect("the file came back");
        assert_eq!(back.seen.artist, "Burial");
        assert_eq!(back.seen.title, "Night Bus");
        assert_eq!(back.mtime, 42, "and what a rescan needs to skip it");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **Two paths holding identical bytes both have to come back.**
    ///
    /// They are one encoding and therefore one row, and a cache that wrote
    /// rows rather than files would keep one of them — so the next scan would
    /// report the other as an arrival. Holdings are keyed by path for exactly
    /// this reason (#86).
    #[test]
    fn both_copies_of_a_duplicated_file_survive_a_restart() {
        let dir = std::env::temp_dir().join("gmuse-cache-test-dupe");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("library.postcard");

        let mut store = Holdings::new();
        store.insert(held(
            "/m/13 This Is How It Goes Down.mp3",
            "It Goes Down",
            "P!nk",
            1,
            7,
        ));
        store.insert(held(
            "/m/13 This Is How It Goes Down (1).mp3",
            "It Goes Down",
            "P!nk",
            1,
            7,
        ));
        assert_eq!(store.len(), 2, "two files");

        save(&path, &store, &[], &crate::catalog::Active::All).unwrap();
        let Outcome::Loaded { store: back, .. } = load(&path).unwrap() else {
            panic!("should have loaded")
        };

        assert_eq!(back.len(), 2, "and two files back");
        assert!(
            back.get(Path::new("/m/13 This Is How It Goes Down.mp3"))
                .is_some()
        );
        assert!(
            back.get(Path::new("/m/13 This Is How It Goes Down (1).mp3"))
                .is_some(),
            "the second copy is the half a row-keyed cache loses"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A catalog choice must survive a restart, or picking one would be a
    /// per-session chore.
    #[test]
    fn the_active_set_round_trips() {
        let dir = std::env::temp_dir().join("gmuse-cache-test-active");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("library.postcard");

        let active = crate::catalog::Active::Only(
            ["jazz".to_string(), "classical".to_string()]
                .into_iter()
                .collect(),
        );
        save(&path, &sample_store(), &[], &active).unwrap();
        let Outcome::Loaded { active: back, .. } = load(&path).unwrap() else {
            panic!("should have loaded")
        };
        assert_eq!(back, active);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_is_not_an_error() {
        let path = std::env::temp_dir().join("gmuse-cache-does-not-exist.postcard");
        let _ = std::fs::remove_file(&path);
        assert!(matches!(load(&path).unwrap(), Outcome::Missing));
    }

    #[test]
    fn garbage_is_ignored_rather_than_fatal() {
        let dir = std::env::temp_dir().join("gmuse-cache-test-garbage");
        std::fs::create_dir_all(&dir).unwrap();

        // Not ours at all.
        let foreign = dir.join("foreign");
        std::fs::write(&foreign, b"this is not a gmuse cache").unwrap();
        assert!(matches!(load(&foreign).unwrap(), Outcome::Discarded));

        // Ours, but truncated mid-record.
        let truncated = dir.join("truncated");
        let store = sample_store();
        save(&truncated, &store, &[], &crate::catalog::Active::All).unwrap();
        let bytes = std::fs::read(&truncated).unwrap();
        std::fs::write(&truncated, &bytes[..bytes.len() / 2]).unwrap();
        // Discarded, not Missing: the user is about to pay for a rescan and
        // should be told why.
        assert!(matches!(load(&truncated).unwrap(), Outcome::Discarded));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A removed file is gone, and there is nothing to compact.**
    ///
    /// This used to assert that tombstones did not survive a save. Tombstones
    /// existed to keep an arena index valid within a session; holdings are a
    /// map keyed by path, so removing one removes it (#86).
    #[test]
    fn a_removed_file_does_not_come_back() {
        let dir = std::env::temp_dir().join("gmuse-cache-test-gone");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("library.postcard");

        let mut store = sample_store();
        store.remove(Path::new("/m/a.mp3"));
        save(&path, &store, &[], &crate::catalog::Active::All).unwrap();

        let Outcome::Loaded { store: loaded, .. } = load(&path).unwrap() else {
            panic!("should have loaded")
        };
        assert_eq!(loaded.len(), 1);
        assert!(loaded.get(Path::new("/m/a.mp3")).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
