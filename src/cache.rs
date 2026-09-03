//! Persisting the `Store` between runs.
//!
//! postcard, because the cache is a private format read only by the version of
//! gmuse that wrote it. There is no compatibility story: a format change bumps
//! `FORMAT_VERSION` and the next run rescans. That is a deliberate trade — a
//! rescan costs seconds, and a migration path for a derived cache would cost
//! more to maintain than it could ever save.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::store::Store;

/// Bump on any change to `Track`, `Store`, or this envelope.
const FORMAT_VERSION: u32 = 6;
const MAGIC: &[u8; 5] = b"gmuse";

#[derive(Serialize, Deserialize)]
struct Envelope {
    version: u32,
    roots: Vec<PathBuf>,
    store: Store,
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
        store: Store,
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
            store: {
                // The path index is derived and deliberately not stored, so
                // that a `TrackId` cannot be serialised at all. Rebuilding it
                // here is the price, and it is one pass.
                let mut store = env.store;
                store.reindex();
                store
            },
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
    store: &Store,
    roots: &[PathBuf],
    active: &crate::catalog::Active,
) -> Result<(), CacheError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }

    let envelope = Envelope {
        version: FORMAT_VERSION,
        roots: roots.to_vec(),
        store: Store::clone_for_save(store),
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
    use crate::store::Track;
    use std::time::Duration;

    fn sample_store() -> Store {
        let mut s = Store::new();
        s.upsert(Track {
            path: PathBuf::from("/m/a.mp3"),
            artist: Some("Burial".into()),
            album: Some("Burial (S-T)".into()),
            title: Some("Night Bus".into()),
            tracknumber: Some(5),
            duration: Duration::from_secs(154),
            mtime: 42,
            ..Default::default()
        });
        s.upsert(Track {
            path: PathBuf::from("/m/b.opus"),
            title: Some("Years".into()),
            mtime: 43,
            ..Default::default()
        });
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
        let id = loaded.id_for_path(Path::new("/m/a.mp3")).unwrap();
        let t = loaded.get(id).unwrap();
        assert_eq!(t.artist.as_deref(), Some("Burial"));
        assert_eq!(t.duration, Duration::from_secs(154));
        assert_eq!(t.mtime, 42);

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

    #[test]
    fn tombstones_do_not_survive_a_save() {
        let dir = std::env::temp_dir().join("gmuse-cache-test-tomb");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("library.postcard");

        let mut store = sample_store();
        store.remove_path(Path::new("/m/a.mp3"));
        save(&path, &store, &[], &crate::catalog::Active::All).unwrap();

        let Outcome::Loaded { store: loaded, .. } = load(&path).unwrap() else {
            panic!("should have loaded")
        };
        assert_eq!(loaded.len(), 1);
        assert_eq!(
            loaded.tombstones(),
            0,
            "tombstones should be compacted away"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
