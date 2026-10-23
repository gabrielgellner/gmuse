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

use crate::operations::scan::Holdings;

/// Bump on any change to `Held`, `Holdings`, `Scanned`, or this envelope.
const FORMAT_VERSION: u32 = 9;
const MAGIC: &[u8; 5] = b"gmuse";

#[derive(Serialize, Deserialize)]
struct Envelope {
    version: u32,
    roots: Vec<PathBuf>,
    store: Holdings,
    /// The view that was in force, as its source line (#119).
    ///
    /// Persisted so a choice survives a restart. Was a list of active catalog
    /// names; a catalog is a saved view with a second name, so what is
    /// remembered is the one thing that says what is in scope.
    #[serde(default)]
    view: Option<String>,
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

/// Where the measurement cache lives, beside the scan cache — #188.
///
/// Its own file rather than a field in [`Envelope`], because the two are
/// dropped for different reasons and on different schedules. The scan cache is
/// discarded whenever gmuse moves music, since the paths it names are stale;
/// the measurements are keyed by path *and validated by a `stat`*, so a moved
/// file simply misses and nothing is wrong. Folding them together would throw
/// away 59 GB of correct answers every time an import renamed one album.
pub fn measurements_path() -> PathBuf {
    default_path().with_file_name("measurements.postcard")
}

/// Bump on any change to `Reads`, `Seen` or `Scanned` — **or to how any field
/// of `Scanned` is measured**, even where its type is unchanged.
///
/// The second half is #309. A read is cached by a file's size and mtime, so a
/// better measurement is never run on a file that has not changed: it is
/// handed the answer the old code gave. `4f95d56` changed how `length_ms` is
/// measured and left the shape alone, and only an unrelated shape change two
/// days later (the bump to 2) flushed the stale reads. That was luck.
///
/// 2 — the cache holds a whole [`crate::model::ingest::Scanned`] rather than
/// just the audio measurement (#190), so a v1 file describes a different
/// shape entirely.
const READS_VERSION: u32 = 2;

#[derive(Serialize, Deserialize)]
struct ReadsEnvelope {
    version: u32,
    seen: crate::model::ingest::Reads,
}

/// Load what earlier runs read. A cache that cannot be read is not an error —
/// it is a slow first run, which is the trade this module already makes
/// everywhere else.
pub fn load_measurements(path: &Path) -> crate::model::ingest::Reads {
    let Ok(bytes) = std::fs::read(path) else {
        return Default::default();
    };
    if bytes.len() < MAGIC.len() || &bytes[..MAGIC.len()] != MAGIC {
        return Default::default();
    }
    let body = &bytes[MAGIC.len()..];

    // **The version is read before the payload, not after it.** postcard is
    // not self-describing, so parsing a v1 file as the current shape is not
    // reliably an error — it can succeed and hand back nonsense, and nonsense
    // here is a *wrong* row rather than a slow one. The envelope puts the
    // version first, so taking that one field settles it before anything is
    // trusted.
    //
    // This was the other way round and it worked only because a shape change
    // happened to fail to parse. That is luck, and #190 is exactly the change
    // that would have spent it.
    let Ok((version, rest)) = postcard::take_from_bytes::<u32>(body) else {
        return Default::default();
    };
    if version != READS_VERSION {
        return Default::default();
    }
    postcard::from_bytes::<crate::model::ingest::Reads>(rest).unwrap_or_default()
}

/// Keep what this run learned. **Never fatal**: failing to write a cache must
/// not fail the command that filled it, because nothing here is the answer —
/// it is only how fast the next answer arrives.
pub fn save_measurements(path: &Path, seen: &crate::model::ingest::Reads) {
    let envelope = ReadsEnvelope {
        version: READS_VERSION,
        seen: crate::model::ingest::Reads::clone_of(seen),
    };
    let Ok(body) = postcard::to_allocvec(&envelope) else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut bytes = MAGIC.to_vec();
    bytes.extend_from_slice(&body);
    let _ = std::fs::write(path, bytes);
}

/// Where the folded projection of **this log** lives — #194, #258.
///
/// Its own file beside the others, for the reason the measurement cache has
/// one: they are dropped for different reasons. The scan cache goes whenever
/// gmuse moves music; the measurements go when a file changes; this goes when
/// the fold's shape changes. Sharing a file would throw all three away
/// whenever any one of them needed to go.
///
/// **Keyed by the data directory, which is what #258 needed.** It was one fixed
/// path, and `commit` recorded exactly why that stopped it refreshing the
/// snapshot after a write: *"the snapshot lives at one fixed path under the
/// cache directory while the log is named by a parameter, so a write against
/// any other data directory — every test that applies a proposal — would
/// overwrite the snapshot of the real library with its own."*
///
/// A UUIDv5 of the path rather than a hash of it, because this project already
/// trusts that derivation to be the same on two machines and across versions —
/// `DefaultHasher` promises neither, and a key that changes under you is a
/// silent cache miss rather than a loud one.
pub fn projection_path(data_dir: &Path) -> PathBuf {
    // Canonical where it can be: two spellings of one directory are one log, and
    // `~/MusicLibrary/.gmuse` reached through a symlink must not get a second
    // snapshot. A directory that does not exist yet cannot be canonicalised, and
    // for that case the literal path is the best key there is.
    let key = data_dir
        .canonicalize()
        .unwrap_or_else(|_| data_dir.to_path_buf());
    let id = crate::model::id::Id::<crate::model::entity::Device>::derived(
        key.as_os_str().as_encoded_bytes(),
    );
    default_path().with_file_name(format!("projection-{}.postcard", id.as_uuid().simple()))
}

/// Bump on any change to `Catalogue`, `Position` or anything they hold — **or
/// to what the fold computes from the log**, even where the shape is unchanged.
///
/// **Which is often**, and deliberately cheap: the snapshot is derived, so a
/// bump costs one slow run and never a migration. That is the invariant that
/// makes changing the projection's shape ordinary rather than dangerous.
///
/// 3 — #309. The fold began keeping an encoding's *latest* description rather
/// than its first. Nothing about the catalogue's shape changed, so by the old
/// rule this would not have been bumped — and every existing snapshot would
/// have gone on serving the first-description answer, because a snapshot is
/// trusted whenever the log's print matches and fixing the fold does not
/// change the log. A fix that is correct and invisible is the failure #309 is
/// about, and the rule above is widened so the next one is not.
///
/// 4 — #323. `Catalogue` holds saved rules beside saved views.
const PROJECTION_VERSION: u32 = 4;

/// **What the logs looked like when a snapshot was taken** — #194.
///
/// The snapshot is only worth having if a caller can tell it is current
/// **without reading the log**, and the obvious check cannot: `advance` takes
/// the records, so asking "am I behind" means parsing 184ms of log to find out
/// the answer was no.
///
/// So the same trade as the read cache: `stat` first. A log whose files are
/// the length and age they were when the fold happened has nothing new in it.
///
/// **This lives here and not on the projection.** L1 may not touch the
/// filesystem — that is the rule the whole of `access-layers.md` turns on — so
/// the staleness check belongs to the adapter that owns the cache, the way
/// `Reads` does.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LogPrint(Vec<(PathBuf, u64, i64)>);

impl LogPrint {
    /// Whether the log this describes is gone — #258.
    ///
    /// **Not one file of it left.** A log that lost a device's file has changed rather
    /// than vanished, and `current_projection` already refuses a snapshot whose print
    /// does not match. Requiring *all* of them to be missing is what keeps a sweep from
    /// deleting a snapshot of a library that is merely one file lighter.
    ///
    /// An empty print means a directory with no logs in it, which is abandoned by any
    /// reading — and is also what a scratch directory leaves behind.
    fn abandoned(&self) -> bool {
        self.0.iter().all(|(path, _, _)| !path.exists())
    }
}

impl LogPrint {
    /// Take a print of every log in a data directory. Cheap: one `stat` each,
    /// and there is one file per device.
    #[allow(dead_code, reason = "read by #195, measured and tested here")]
    pub fn of(data_dir: &Path) -> Self {
        let mut out = Vec::new();
        for path in crate::model::durable::logs_in(data_dir) {
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            let age = meta
                .modified()
                .ok()
                .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                .and_then(|d| i64::try_from(d.as_millis()).ok())
                .unwrap_or_default();
            out.push((path, meta.len(), age));
        }
        out.sort();
        LogPrint(out)
    }
}

/// Load a folded projection **only if the logs have not changed since**.
///
/// `None` means fold: missing, corrupt, from another version, or taken before
/// somebody appended. All four are the same answer and none of them is an
/// error.
pub fn current_projection(path: &Path, now: &LogPrint) -> Option<crate::projection::Projected> {
    let (print, projected) = load_projection(path)?;
    (print == *now).then_some(projected)
}

/// Load a folded projection. `None` for anything but a snapshot this version
/// wrote — missing, corrupt and stale are the same answer, which is a fold.
fn load_projection(path: &Path) -> Option<(LogPrint, crate::projection::Projected)> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() < MAGIC.len() || &bytes[..MAGIC.len()] != MAGIC {
        return None;
    }
    // **Version before payload**, the lesson of #190: postcard is not
    // self-describing, so parsing an older shape is not reliably an error — it
    // can succeed and hand back nonsense, and nonsense here is a library that
    // says the wrong thing rather than one that is slow to answer.
    let (version, rest) = postcard::take_from_bytes::<u32>(&bytes[MAGIC.len()..]).ok()?;
    if version != PROJECTION_VERSION {
        return None;
    }
    postcard::from_bytes(rest).ok()
}

/// Keep a folded projection, with the print of the logs it was folded from.
/// Never fatal — it is a cache.
pub fn save_projection(path: &Path, print: &LogPrint, projected: &crate::projection::Projected) {
    let Ok(mut body) = postcard::to_allocvec(&PROJECTION_VERSION) else {
        return;
    };
    let Ok(payload) = postcard::to_allocvec(&(print, projected)) else {
        return;
    };
    body.extend_from_slice(&payload);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut bytes = MAGIC.to_vec();
    bytes.extend_from_slice(&body);
    // **Beside, then rename**, like `durable::rewrite`: a crash mid-write
    // leaves the old snapshot or the new one, never half of one that would
    // then be read as a library.
    let beside = path.with_extension("postcard.writing");
    if std::fs::write(&beside, bytes).is_ok() {
        let _ = std::fs::rename(&beside, path);
        sweep_abandoned(path);
    }
}

/// How many snapshots may sit beside each other before a sweep is worth its cost.
///
/// **A threshold rather than a sweep per save**, because a save happens on every commit
/// and a sweep reads a header from every sibling. A working machine has one or two
/// libraries; thirty-two means something has been leaving them behind.
const CROWDED: usize = 32;

/// Delete snapshots whose log no longer exists — #258.
///
/// **The cost of keying the snapshot by its log**, and it is worth paying once rather
/// than living with: keying fixed the bug where two data directories overwrote each
/// other's snapshot, and created a smaller one where every scratch directory leaves a
/// file behind for ever. One run of this project's own suite left **92**.
///
/// A snapshot records the log files it was folded from, so "is this abandoned" is
/// answerable without guessing: if not one of those paths exists, the library it
/// describes is gone.
fn sweep_abandoned(just_wrote: &Path) {
    let Some(dir) = just_wrote.parent() else {
        return;
    };
    // **The one before keying, retired by name** — #258. `projection.postcard` was the
    // single fixed path, and no production code reads it any more; on an existing
    // install it is 5.7 MB describing a library that is now snapshotted under its own
    // key.
    //
    // **Unless it is the file just written.** A caller may still name that path — a test
    // in `projection/mod.rs` does — and the first version of this deleted the snapshot
    // it had that moment saved. The suite caught it, which is the argument for the test
    // that names its own path.
    let legacy = dir.join("projection.postcard");
    if legacy != just_wrote {
        let _ = std::fs::remove_file(&legacy);
    }

    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let ours: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("projection-") && n.ends_with(".postcard"))
        })
        .collect();
    if ours.len() <= CROWDED {
        return;
    }
    for path in ours {
        // Never the one just written, whatever its header says.
        if path == just_wrote {
            continue;
        }
        if print_of(&path).is_some_and(|p| p.abandoned()) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// The `LogPrint` from a snapshot's header, without decoding the projection behind it.
///
/// **A bounded read**, because the projection is 5.7 MB for a real library and the
/// header is a few paths. `(print, projected)` is a tuple, so postcard lays the print
/// down first and `take_from_bytes` stops there.
fn print_of(path: &Path) -> Option<LogPrint> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let mut head = vec![0u8; 64 * 1024];
    let read = file.read(&mut head).ok()?;
    head.truncate(read);
    if head.len() < MAGIC.len() || &head[..MAGIC.len()] != MAGIC {
        return None;
    }
    let (version, rest) = postcard::take_from_bytes::<u32>(&head[MAGIC.len()..]).ok()?;
    if version != PROJECTION_VERSION {
        return None;
    }
    postcard::take_from_bytes::<LogPrint>(rest)
        .ok()
        .map(|(print, _)| print)
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
        view: Option<String>,
    },
}

/// Load a cache. Never an error the caller has to handle: a cache that cannot
/// be read just means a rescan.
/// Throw the cache away, because something outside the player moved the music.
///
/// **The writer says so; the reader never guesses.** A launch deliberately does
/// not scan — `main` says a change to the library is something you make, not
/// something a launch notices on your behalf — so the cache is trusted until
/// somebody replaces it. That is right for files appearing on disk behind
/// gmuse's back, and wrong for files *gmuse itself* moved: `shelf import
/// --write` renames every file it places, and left a cache still naming the
/// old paths. The player then opened `01 Cassius 1999.mp3` after the import
/// had renamed it to `1-01 …`, and reported the FLAC beside it as nothing at
/// all, because that cache had never seen one.
///
/// Discarding rather than rewriting, because the cache is derivable and the
/// 2026-08-27 decision is that it may be dropped at any time. The next launch
/// treats it as a first run and scans.
///
/// A cache that is not there is not an error.
pub fn discard(path: &Path) -> Result<(), CacheError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(CacheError::Io(e)),
    }
}

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
            view: env.view,
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
    view: Option<&str>,
) -> Result<(), CacheError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }

    let envelope = Envelope {
        version: FORMAT_VERSION,
        roots: roots.to_vec(),
        store: store.clone(),
        view: view.map(str::to_string),
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

    /// **An abandoned snapshot is swept, and a live one is not** — #258.
    ///
    /// Keying the snapshot by its log fixed two data directories overwriting each
    /// other's and created a smaller problem: one run of this project's suite left
    /// **92** files behind, one per scratch directory. This is the sweep, and the half
    /// that matters is the second assertion — a sweep that deleted everything would pass
    /// the first.
    #[test]
    fn a_snapshot_whose_log_is_gone_is_swept_and_one_in_use_is_not() {
        let dir = std::env::temp_dir().join(format!("gmuse-sweep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");

        // A log that exists, and one that does not.
        let live_log = dir.join("log-live.jsonl");
        std::fs::write(&live_log, "{}\n").expect("write");
        let gone_log = dir.join("log-gone.jsonl");

        let projected = crate::projection::Projected::default();
        let live = dir.join("projection-live.postcard");
        let gone = dir.join("projection-gone.postcard");
        save_projection(&live, &LogPrint(vec![(live_log.clone(), 3, 0)]), &projected);
        save_projection(&gone, &LogPrint(vec![(gone_log, 3, 0)]), &projected);

        // **The instrument is attached**: both were written, so the sweep below has
        // something to be right and wrong about.
        assert!(live.exists() && gone.exists(), "the fixture wrote nothing");
        assert!(
            print_of(&live).is_some(),
            "the header cannot be read back, so the sweep can only guess"
        );

        // Crowd the directory past the threshold, then sweep by writing one more.
        for n in 0..=CROWDED {
            save_projection(
                &dir.join(format!("projection-filler{n}.postcard")),
                &LogPrint(vec![(dir.join(format!("log-absent{n}.jsonl")), 1, 0)]),
                &projected,
            );
        }

        assert!(
            !gone.exists(),
            "a snapshot of a library that no longer exists was kept"
        );
        assert!(
            live.exists(),
            "a snapshot whose log is still there was swept — which would cost a real \
             library its fold every time anything else wrote"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
    use crate::operations::scan::Held;

    /// **A cache the writer knows is wrong is thrown away, not corrected.**
    ///
    /// An import renames every file it places, so the paths in the cache stop
    /// existing. The next launch treats a missing cache as a first run and
    /// scans, which is the only thing that can be right after music moved.
    #[test]
    fn a_discarded_cache_is_gone_and_discarding_twice_is_fine() {
        let dir = std::env::temp_dir().join("gmuse-cache-test-discard");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("library.postcard");
        std::fs::write(&path, b"anything").unwrap();
        assert!(path.exists());

        discard(&path).expect("discards");
        assert!(!path.exists(), "the cache is gone");

        // Two imports in a row, and the second must not fail for tidying up
        // after the first.
        discard(&path).expect("a cache that is not there is not an error");
    }

    fn held(at: &str, title: &str, artist: &str, mtime: i64, audio: u128) -> Held {
        // **A holding is its audio and where it is** (#141) — the title and
        // artist a caller passes are what a fixture *means*, and nothing on
        // disk carries them any more.
        let _ = (title, artist);
        Held {
            at: PathBuf::from(at),
            mtime,
            size: 4_000_000,
            audio: crate::audio_id::AudioId {
                scheme: 1,
                hash: audio,
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
        save(&path, &store, &roots, None).unwrap();

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
        assert_eq!(
            back.audio.hash, 1,
            "the audio id is what a cached holding is for"
        );
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

        save(&path, &store, &[], None).unwrap();
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

    /// The view in force must survive a restart, or choosing one would be a
    /// per-session chore.
    ///
    /// **One source line**, where this used to be a set of catalog names
    /// (#119). A catalog was a saved view with a second name, and what is
    /// remembered is the one thing that says what is in scope.
    #[test]
    fn the_view_in_force_round_trips() {
        let dir = std::env::temp_dir().join("gmuse-cache-test-view");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("library.postcard");

        let view = r#"saved="jazz" +saved="classical""#;
        save(&path, &sample_store(), &[], Some(view)).unwrap();
        let Outcome::Loaded { view: back, .. } = load(&path).unwrap() else {
            panic!("should have loaded")
        };
        assert_eq!(back.as_deref(), Some(view));

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
        save(&truncated, &store, &[], None).unwrap();
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
        save(&path, &store, &[], None).unwrap();

        let Outcome::Loaded { store: loaded, .. } = load(&path).unwrap() else {
            panic!("should have loaded")
        };
        assert_eq!(loaded.len(), 1);
        assert!(loaded.get(Path::new("/m/a.mp3")).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
