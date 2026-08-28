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
const FORMAT_VERSION: u32 = 1;
const MAGIC: &[u8; 5] = b"gmuse";

#[derive(Serialize, Deserialize)]
struct Envelope {
    version: u32,
    roots: Vec<PathBuf>,
    store: Store,
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

/// Load a cache. A missing, stale or corrupt file is `Ok(None)` — the caller
/// rescans, which is always correct, just slower.
pub fn load(path: &Path) -> Result<Option<(Store, Vec<PathBuf>)>, CacheError> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };

    if bytes.len() < MAGIC.len() || &bytes[..MAGIC.len()] != MAGIC {
        return Ok(None);
    }

    match postcard::from_bytes::<Envelope>(&bytes[MAGIC.len()..]) {
        Ok(env) if env.version == FORMAT_VERSION => Ok(Some((env.store, env.roots))),
        // Wrong version or unparseable: not an error, just a rescan.
        Ok(_) | Err(_) => Ok(None),
    }
}

/// Write the cache, creating the directory if needed.
///
/// Writes to a temporary file and renames, so an interrupted save leaves the
/// previous cache intact rather than a truncated one.
pub fn save(path: &Path, store: &Store, roots: &[PathBuf]) -> Result<(), CacheError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }

    let envelope = Envelope {
        version: FORMAT_VERSION,
        roots: roots.to_vec(),
        store: Store::clone_for_save(store),
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
        save(&path, &store, &roots).unwrap();

        let (loaded, loaded_roots) = load(&path).unwrap().unwrap();
        assert_eq!(loaded_roots, roots);
        assert_eq!(loaded.len(), 2);
        let id = loaded.id_for_path(Path::new("/m/a.mp3")).unwrap();
        let t = loaded.get(id).unwrap();
        assert_eq!(t.artist.as_deref(), Some("Burial"));
        assert_eq!(t.duration, Duration::from_secs(154));
        assert_eq!(t.mtime, 42);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_is_not_an_error() {
        let path = std::env::temp_dir().join("gmuse-cache-does-not-exist.postcard");
        let _ = std::fs::remove_file(&path);
        assert!(load(&path).unwrap().is_none());
    }

    #[test]
    fn garbage_is_ignored_rather_than_fatal() {
        let dir = std::env::temp_dir().join("gmuse-cache-test-garbage");
        std::fs::create_dir_all(&dir).unwrap();

        // Not ours at all.
        let foreign = dir.join("foreign");
        std::fs::write(&foreign, b"this is not a gmuse cache").unwrap();
        assert!(load(&foreign).unwrap().is_none());

        // Ours, but truncated mid-record.
        let truncated = dir.join("truncated");
        let store = sample_store();
        save(&truncated, &store, &[]).unwrap();
        let bytes = std::fs::read(&truncated).unwrap();
        std::fs::write(&truncated, &bytes[..bytes.len() / 2]).unwrap();
        assert!(load(&truncated).unwrap().is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tombstones_do_not_survive_a_save() {
        let dir = std::env::temp_dir().join("gmuse-cache-test-tomb");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("library.postcard");

        let mut store = sample_store();
        store.remove_path(Path::new("/m/a.mp3"));
        save(&path, &store, &[]).unwrap();

        let (loaded, _) = load(&path).unwrap().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(
            loaded.tombstones(),
            0,
            "tombstones should be compacted away"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
