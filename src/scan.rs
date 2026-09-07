//! Shelf scanning: walk the filesystem, read tags, update the `Scan`.
//!
//! Rescans are incremental by mtime — a file whose mtime is unchanged is not
//! reopened. On a library of any size the second scan is a `stat` per file and
//! nothing else. That is what *used* to make a scan on every startup
//! tolerable; since 2026-09-01 a launch does not scan at all, and this cheap
//! second pass is what makes an explicit `--rescan` feel free.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

/// Extensions we will try to read. Anything else in the tree is ignored
/// without opening it, which matters when a music folder also holds artwork,
/// playlists and stray video.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "ogg", "opus", "m4a", "mp4", "aac", "alac", "wav", "wave", "aiff", "aif", "ape",
    "wv", "mpc",
];

/// One file, as a scan found it.
///
/// **What replaced `store::Track`** (#86). A `Track` was a display record: the
/// player read its fields to draw a row, so it carried a title, an artist, a
/// genre and thirteen other things in the shape a list wanted them. The player
/// reads the library now, so what a scan produces has one job left — to be
/// *ingested* — and the model already has a type for that.
///
/// The three fields beside it are the ones a scan needs and the catalogue does
/// not: where the file is on this device, and enough to tell a rescan what it
/// need not reopen.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Held {
    /// Where this device can open it.
    pub at: PathBuf,
    /// Seconds since the epoch. The incremental fast path: an unchanged mtime
    /// means the file is not reopened.
    pub mtime: i64,
    /// File size in bytes.
    ///
    /// Not used for scanning — mtime decides that — but it is the strongest
    /// signal available for recognising a *moved* file whose bytes are
    /// unchanged and whose path is not (#25).
    pub size: u64,
    /// **The identity of its audio, and nothing else** — #141.
    ///
    /// This carried a whole `Scanned` — title, artist, album, genres, key,
    /// every tag the boundary reads. Since #140 nothing read any of it but the
    /// hash: the log is the catalogue and tags are read once, at `import`.
    ///
    /// Narrowing the field is what makes that a rule rather than a habit. The
    /// tags were sitting on every holding at every launch, so the next thing
    /// wanting an artist name in the scan layer would have found one — and
    /// "a retag is invisible" would have quietly stopped being true.
    pub audio: crate::audio_id::AudioId,
}

/// What this device holds, keyed by where it is.
///
/// **Keyed by path on purpose**, and it is the one place that is right: a
/// scan's whole job is to reconcile a set of *locations* against what was
/// there last time. Nothing downstream is keyed this way — the catalogue is
/// keyed by identity, and a row by its bytes — and this does not outlive the
/// scan that produced it except in the cache, which is derivable and may be
/// discarded at any time.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct Holdings {
    files: std::collections::BTreeMap<PathBuf, Held>,
    /// **Tests only**: what a fixture said each file's tags were — #141.
    ///
    /// A holding carries the audio's identity and nothing else, because a scan
    /// reads no tags. A synthetic fixture has no file to read, so the tags it
    /// *means* have to travel somehow, and they travel here — in a test build,
    /// where `import` is what a test is standing in for.
    ///
    /// Not on `Held`, which production constructs and could then be asked to
    /// fill with something it does not have.
    #[cfg(test)]
    #[serde(skip)]
    tagged: std::collections::BTreeMap<PathBuf, crate::model::instance::Scanned>,
}

impl Holdings {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take in a file as a fixture means it — **tests only**, #141.
    ///
    /// The holding gets the hash off the tags rather than being handed one
    /// separately, so a fixture cannot say one thing and hold another.
    #[cfg(test)]
    pub fn take(
        &mut self,
        at: PathBuf,
        mtime: i64,
        size: u64,
        seen: crate::model::instance::Scanned,
    ) {
        self.insert(Held {
            at: at.clone(),
            mtime,
            size,
            audio: seen.audio,
        });
        self.tagged.insert(at, seen);
    }

    /// What the fixture said the tags were — **tests only**.
    #[cfg(test)]
    pub fn tags(&self) -> Vec<crate::model::instance::Scanned> {
        self.tagged.values().cloned().collect()
    }

    /// The tags a fixture gave one file — **tests only**.
    #[cfg(test)]
    pub fn tag_at(&self, at: &Path) -> Option<&crate::model::instance::Scanned> {
        self.tagged.get(at)
    }

    pub fn get(&self, at: &Path) -> Option<&Held> {
        self.files.get(at)
    }

    pub fn insert(&mut self, held: Held) {
        self.files.insert(held.at.clone(), held);
    }

    pub fn remove(&mut self, at: &Path) {
        self.files.remove(at);
    }

    /// Every file, in path order.
    pub fn iter(&self) -> impl Iterator<Item = &Held> {
        self.files.values()
    }

    /// **Files, not rows.** Two paths holding identical bytes are two files
    /// and one encoding, and this counts what is on disk.
    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// A file recognised at a new path rather than treated as a deletion and an
/// unrelated arrival.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Move {
    pub from: PathBuf,
    pub to: PathBuf,
}

#[derive(Debug, Default, Clone)]
pub struct ScanReport {
    pub added: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub removed: usize,
    /// Departures paired with arrivals. Counted in `added` and `removed` too,
    /// because that is what happened to the `Scan`; what makes them moves is
    /// that everything keyed by path can follow them.
    pub moves: Vec<Move>,
    /// Files that looked like audio but could not be read.
    pub errors: Vec<(PathBuf, String)>,
    pub elapsed: Duration,
}

impl ScanReport {
    pub fn total(&self) -> usize {
        self.added + self.updated + self.unchanged
    }
}

impl std::fmt::Display for ScanReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} tracks ({} added, {} updated, {} unchanged, {} removed, {} errors) in {:.2}s{}",
            self.total(),
            self.added,
            self.updated,
            self.unchanged,
            self.removed,
            self.errors.len(),
            self.elapsed.as_secs_f64(),
            // Said out loud: a silent reassociation would be as bad as the
            // silent loss it replaces, and worse if it ever guesses wrong.
            match self.moves.len() {
                0 => String::new(),
                1 => "\n  1 track moved, and its history moved with it".to_string(),
                n => format!("\n  {n} tracks moved, and their history moved with them"),
            }
        )
    }
}

/// Scan `roots` into `store`, incrementally.
///
/// Tracks under a root that no longer exist on disk are removed. Tracks
/// outside every root are left alone, so scanning one folder does not evict a
/// library built from another.
pub fn scan(held: &mut Holdings, roots: &[PathBuf]) -> ScanReport {
    let start = Instant::now();
    // A root under another root would have every one of its files walked
    // twice — read twice on a cold scan, and counted twice in the report,
    // which is how a 311-track library came to announce 622 (#21).
    let roots = &prune_nested(roots);
    let mut report = ScanReport::default();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    // Paths that were not in the store before this pass. Half of what move
    // detection needs; the other half is what went away.
    let mut arrived: Vec<PathBuf> = Vec::new();

    // **The walk decides what to read; the reading happens after** (#122).
    //
    // Split because they cost nothing alike: deciding is a `stat` per file and
    // the mtime fast path, and reading is every audio packet of every changed
    // file — 24.7s of a 24.7s first scan on a 6,500-file library. Collected
    // first so the expensive half can be spread over the cores that were
    // watching it happen.
    let mut to_read: Vec<Candidate> = Vec::new();
    for root in roots {
        for entry in walkdir::WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_entry(worth_entering)
            .filter_map(Result::ok)
        {
            if !entry.file_type().is_file() || !is_audio(entry.path()) {
                continue;
            }
            let path = entry.path().to_path_buf();
            let meta = entry.metadata().ok();
            let mtime = meta
                .as_ref()
                .and_then(|m| m.modified().ok())
                .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);

            seen.insert(path.clone());

            // The incremental fast path: same file, same mtime, don't reopen.
            if held.get(&path).is_some_and(|h| h.mtime == mtime) {
                report.unchanged += 1;
                continue;
            }
            let existed = held.get(&path).is_some();
            to_read.push((path, mtime, size, existed));
        }
    }

    for (path, mtime, size, existed, audio) in read_all(to_read) {
        match audio {
            Ok(audio) => {
                held.insert(Held {
                    at: path.clone(),
                    mtime,
                    size,
                    audio,
                });
                if existed {
                    report.updated += 1;
                } else {
                    arrived.push(path);
                    report.added += 1;
                }
            }
            Err(e) => report.errors.push((path, e.to_string())),
        }
    }

    // Anything under a root we did not see has gone away.
    // **Files, not rows.** Two paths holding identical bytes are one row, so
    // walking rows would show only one of them — and the other, unseen, would
    // never be reported as gone. That turned a move into a deletion (#86).
    let gone: Vec<PathBuf> = held
        .iter()
        .filter(|h| !seen.contains(&h.at) && roots.iter().any(|r| h.at.starts_with(r)))
        .map(|h| h.at.clone())
        .collect();

    // Pair the two halves up before the departures are destroyed: a file that
    // left one path and arrived at another is the same track, and everything
    // durable is keyed by path (#25).
    report.moves = detect_moves(held, &gone, &arrived);

    for path in gone {
        held.remove(&path);
        report.removed += 1;
    }

    // **The count of authority-named files is gone from here** (#141). It read
    // `seen.identity`, which was the last thing on a holding that came from a
    // tag. A scan does not read tags, so it cannot answer it — and it is a
    // library question anyway: the log knows what has been identified, and
    // `shelf query --unidentified` is where to ask.
    report.elapsed = start.elapsed();
    report
}

/// A file the walk decided needs reading: where it is, what the filesystem
/// says about it, and whether the library had already heard of it.
type Candidate = (PathBuf, i64, u64, bool);

/// A [`Candidate`] and what reading it produced.
type Reading = (
    PathBuf,
    i64,
    u64,
    bool,
    Result<crate::audio_id::AudioId, crate::audio_id::AudioIdError>,
);

/// Read every file that changed, over as many threads as there are cores.
///
/// **Embarrassingly parallel and it always was**: each file is independent,
/// the only shared state was the map being written into, and the expensive
/// part is `audio_id::of` demuxing and hashing every packet — I/O, which an
/// SSD serves better several requests at a time than one.
///
/// **The order out is the order in.** `arrived` feeds `detect_moves`, and the
/// report is something a person reads; neither may depend on which thread
/// finished first. Each worker takes a strided slice and writes into its own
/// slot, so the result is the input order whatever the scheduling did.
///
/// `std::thread::scope` rather than a work-stealing pool: §12 says threads and
/// channels, the work is known before it starts, and a dependency for a
/// twenty-line fan-out would be one to record and justify.
fn read_all(work: Vec<Candidate>) -> Vec<Reading> {
    // Below this the threads cost more than they save, and a library of a
    // dozen files is a real case — a test fixture is one.
    const WORTH_SPREADING: usize = 32;

    let threads = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let one = |(path, mtime, size, existed): Candidate| {
        let audio = crate::audio_id::of(&path);
        (path, mtime, size, existed, audio)
    };
    if threads < 2 || work.len() < WORTH_SPREADING {
        return work.into_iter().map(one).collect();
    }

    let mut out: Vec<Option<Reading>> = (0..work.len()).map(|_| None).collect();
    // **Strided, not chunked**, because a library is not evenly expensive: an
    // album of FLACs sits together in the walk, so contiguous chunks would
    // hand one worker every large file on the record and leave the rest idle.
    let mut lanes: Vec<Vec<(usize, &mut Option<Reading>)>> =
        (0..threads).map(|_| Vec::new()).collect();
    for (at, slot) in out.iter_mut().enumerate() {
        lanes[at % threads].push((at, slot));
    }

    std::thread::scope(|scope| {
        for lane in lanes {
            let work = &work;
            scope.spawn(move || {
                for (at, slot) in lane {
                    let (path, mtime, size, existed) = &work[at];
                    let audio = crate::audio_id::of(path);
                    *slot = Some((path.clone(), *mtime, *size, *existed, audio));
                }
            });
        }
    });

    out.into_iter()
        .map(|slot| slot.expect("every index is in exactly one lane"))
        .collect()
}

/// **What a departure and an arrival are matched on: the audio.**
///
/// This used to be a choice between the audio id and a *shape* — size,
/// duration, bitrate and the tags — because a file whose audio would not read
/// had no id and still had to be recognisable across a move. The shape
/// included the tags, so a pass that retagged *and* moved in one go broke it,
/// which is why this project carried a rule about doing those separately and
/// why a renumber stranded three history entries (#35).
///
/// A `Held` always has an audio id (#86): a file whose audio will not read is
/// an error rather than a holding, because it has no encoding, therefore no
/// row, and nothing to play. So the weak half is gone, and what is left cannot
/// be broken by a tagger — the id is computed from the packets.
fn key(h: &Held) -> crate::audio_id::AudioId {
    h.audio
}

/// Pair departures with arrivals.
///
/// **Only when the audio is unique on both sides.** Two copies of one track
/// would otherwise be paired arbitrarily — and a wrong pairing attaches
/// somebody's rating to the wrong song, which is worse than the loss this is
/// preventing. Ambiguity is declined rather than guessed.
fn detect_moves(held: &Holdings, gone: &[PathBuf], arrived: &[PathBuf]) -> Vec<Move> {
    if gone.is_empty() || arrived.is_empty() {
        return Vec::new();
    }

    let mut departures: std::collections::HashMap<crate::audio_id::AudioId, Option<&PathBuf>> =
        std::collections::HashMap::new();
    for path in gone {
        let Some(t) = held.get(path) else {
            continue;
        };
        // `None` marks an audio id seen more than once, so it stays ambiguous
        // even if a third arrives later.
        departures
            .entry(key(t))
            .and_modify(|slot| *slot = None)
            .or_insert(Some(path));
    }

    let mut counts: std::collections::HashMap<crate::audio_id::AudioId, usize> =
        std::collections::HashMap::new();
    for path in arrived {
        if let Some(t) = held.get(path) {
            *counts.entry(key(t)).or_insert(0) += 1;
        }
    }

    let mut moves = Vec::new();
    for path in arrived {
        let Some(t) = held.get(path) else {
            continue;
        };
        let k = key(t);
        if counts.get(&k) != Some(&1) {
            continue;
        }
        if let Some(Some(from)) = departures.get(&k) {
            moves.push(Move {
                from: (*from).clone(),
                to: path.clone(),
            });
        }
    }
    moves
}

/// Drop any root that lies under another, and any exact duplicate.
///
/// `Path::starts_with` compares whole components, so `/a/bc` is not under
/// `/a/b` — which is why this is not string prefix matching.
pub(crate) fn prune_nested(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut kept: Vec<PathBuf> = Vec::new();
    for root in roots {
        // Already covered by something kept — including by itself, which is
        // how duplicates fall out.
        if kept.iter().any(|k| root.starts_with(k)) {
            continue;
        }
        // This one covers things already kept; they are the redundant ones now.
        kept.retain(|k| !k.starts_with(root));
        kept.push(root.clone());
    }
    kept
}

/// Whether a walk should step into this — #146.
///
/// **A dotted directory is not part of the library.** `.gmuse` holds the log
/// and `.departed/` holds what `remove` retired, and a scan that walked into
/// the second would hand back every file somebody had just taken out. The
/// convention is the usual one, so `.Trash` and `.Spotlight-V100` go with it.
///
/// The root itself is never skipped, however it is spelled: a library at
/// `~/.music` is a library.
pub(crate) fn worth_entering(entry: &walkdir::DirEntry) -> bool {
    entry.depth() == 0
        || !entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with('.'))
}

pub(crate) fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let e = e.to_ascii_lowercase();
            AUDIO_EXTENSIONS.contains(&e.as_str())
        })
        .unwrap_or(false)
}

// **`read_track` and `from_path_alone` lived here and are gone** (#86).
//
// They read a file's tags into a `Track` — sixteen display fields — and had a
// second, gentler path for a file whose tag block would not parse, so that it
// still appeared under "Unknown Artist" with its file name for a title rather
// than vanishing.
//
// `model::ingest::read` does both jobs and is the model's own boundary, so
// there is one tag reader rather than two that could drift. It keeps the same
// promise: **ingest is total**, and bad metadata is ingested and marked rather
// than refused. What it does refuse is a file whose *audio* will not read,
// which has no encoding and therefore no row.

/// The terms a genre tag names.
///
/// Taggers write several genres into one field and disagree about the
/// separator: `;` is the common one, `/` appears on older files, and a stray
/// `,` shows up too. Split on all three, trimmed, blanks dropped, order kept
/// — the first term is usually the broadest and that is what a one-line
/// display wants first.
///
/// **Not split on `&`**: `Drum & Bass` is one genre.
pub(crate) fn split_genres(tag: Option<&str>) -> Vec<String> {
    tag.map(|s| {
        s.split([';', '/', ','])
            .map(str::trim)
            .filter(|term| !term.is_empty())
            .map(str::to_string)
            .collect()
    })
    .unwrap_or_default()
}

// `parse_year` and `parse_db` lived here and are gone with the tag reader
// they served (#86). `PartialDate::parse` took the year leniency across —
// `(1997)` and `recorded 2016 live` both appear in real tags — and
// `ingest::cdb` and `ingest::peak_e4` read the ReplayGain fields.

#[cfg(test)]
mod tests {
    /// How much of the library is safe from the failure that stranded a
    /// four-star rating.
    ///
    /// Measured 2026-09-02: **5774 of 5774, 100%**. Every file has either a
    /// MusicBrainz recording id or an audio hash, so every one keeps its
    /// history through a rename, a renumber and a retag once the key flips.
    ///
    use super::*;
    use crate::model::instance::Scanned;
    use lofty::prelude::ItemKey;

    /// A minimal 16-bit PCM WAV, so a scan test needs no music library.
    fn write_wav(path: &Path, frames: u32) {
        let (channels, rate, bps) = (1u16, 44_100u32, 2u16);
        let block = channels * bps;
        let data = frames * block as u32;
        let mut w: Vec<u8> = Vec::with_capacity(44 + data as usize);
        w.extend_from_slice(b"RIFF");
        w.extend_from_slice(&(36 + data).to_le_bytes());
        w.extend_from_slice(b"WAVEfmt ");
        w.extend_from_slice(&16u32.to_le_bytes());
        w.extend_from_slice(&1u16.to_le_bytes());
        w.extend_from_slice(&channels.to_le_bytes());
        w.extend_from_slice(&rate.to_le_bytes());
        w.extend_from_slice(&(rate * block as u32).to_le_bytes());
        w.extend_from_slice(&block.to_le_bytes());
        w.extend_from_slice(&(bps * 8).to_le_bytes());
        w.extend_from_slice(b"data");
        w.extend_from_slice(&data.to_le_bytes());
        for i in 0..frames {
            w.extend_from_slice(&((i % 1000) as i16).to_le_bytes());
        }
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, w).unwrap();
    }

    fn scan_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gmuse-scan-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// **What comes back is in the order it went in**, whatever the threads
    /// did — #122.
    ///
    /// `arrived` feeds `detect_moves` and the report is something a person
    /// reads, so neither may depend on which worker finished first. Enough
    /// files to cross `WORTH_SPREADING`, and each one a different length so a
    /// misordering cannot look like a match.
    #[test]
    fn a_parallel_read_comes_back_in_the_order_it_was_asked_for() {
        let d = scan_dir("parallel-order");
        let mut want = Vec::new();
        for n in 0..64u32 {
            let path = d.join(format!("{n:03}.wav"));
            write_wav(&path, 1000 + n * 7);
            want.push(path);
        }

        let work: Vec<Candidate> = want.iter().map(|p| (p.clone(), 0, 0, false)).collect();
        let out = read_all(work);

        assert_eq!(out.len(), want.len());
        let got: Vec<_> = out.iter().map(|(p, ..)| p.clone()).collect();
        assert_eq!(got, want, "the fan-out shuffled its results");
        // And every one of them actually read, so an ordering that survived by
        // returning nothing would not pass.
        assert!(
            out.iter().all(|(_, _, _, _, seen)| seen.is_ok()),
            "a worker dropped a file"
        );
    }

    /// A library too small to be worth spreading is read on this thread, and
    /// gets the same answers — a fixture is a real case of this.
    #[test]
    fn a_small_library_takes_the_serial_path_and_agrees() {
        let d = scan_dir("parallel-small");
        let paths: Vec<PathBuf> = (0..3u32)
            .map(|n| {
                let path = d.join(format!("{n}.wav"));
                write_wav(&path, 1000 + n);
                path
            })
            .collect();
        let work: Vec<Candidate> = paths.iter().map(|p| (p.clone(), 0, 0, false)).collect();
        let out = read_all(work);
        assert_eq!(
            out.iter().map(|(p, ..)| p.clone()).collect::<Vec<_>>(),
            paths
        );
    }

    /// The wiring, not just the helper: `scan` itself must prune, or every
    /// file under an overlapping root is read twice and counted twice. This is
    /// what actually produced `622 tracks` for a 311-track library (#21).
    #[test]
    fn scanning_overlapping_roots_visits_each_file_once() {
        let d = scan_dir("overlapping");
        // Different lengths, so the two are different files. Written the same
        // once, which made them one row and made this test about duplicates
        // rather than about overlapping roots (#86).
        write_wav(&d.join("Dance/a.wav"), 4410);
        write_wav(&d.join("General/b.wav"), 8820);

        // The stale parent beside its two real children, as the cache had it.
        let roots = vec![d.join("Dance"), d.join("General"), d.clone()];
        let mut store = Holdings::new();
        let report = scan(&mut store, &roots);

        assert_eq!(report.errors, vec![], "the fixture files did not read");
        assert_eq!(store.len(), 2);
        assert_eq!(
            report.total(),
            2,
            "each file was counted more than once: {report}"
        );
        assert_eq!(report.added, 2);
    }

    /// And the incremental path counts once too — the case that reported 622,
    /// where everything was `unchanged`.
    #[test]
    fn rescanning_overlapping_roots_counts_each_file_once() {
        let d = scan_dir("overlapping-rescan");
        write_wav(&d.join("Dance/a.wav"), 4410);
        write_wav(&d.join("General/b.wav"), 4410);
        let roots = vec![d.join("Dance"), d.join("General"), d.clone()];

        let mut store = Holdings::new();
        scan(&mut store, &roots);
        let again = scan(&mut store, &roots);

        assert_eq!(again.unchanged, 2, "{again}");
        assert_eq!(again.total(), 2, "{again}");
        assert_eq!(again.removed, 0);
    }

    /// A root under another root walked every one of its files twice — read
    /// twice on a cold scan, and counted twice in the report. That is how a
    /// 311-track library came to announce 622 (#21).
    #[test]
    fn a_root_under_another_root_is_dropped() {
        let roots = [
            PathBuf::from("/m/Dance"),
            PathBuf::from("/m/General"),
            PathBuf::from("/m"),
        ];
        // The parent arrives last and swallows the two already kept.
        assert_eq!(prune_nested(&roots), vec![PathBuf::from("/m")]);

        // And the other way round: the parent first, children ignored.
        let roots = [
            PathBuf::from("/m"),
            PathBuf::from("/m/Dance"),
            PathBuf::from("/m/General"),
        ];
        assert_eq!(prune_nested(&roots), vec![PathBuf::from("/m")]);
    }

    #[test]
    fn unrelated_roots_all_survive() {
        let roots = [
            PathBuf::from("/m/Dance"),
            PathBuf::from("/m/General"),
            PathBuf::from("/other"),
        ];
        assert_eq!(prune_nested(&roots), roots.to_vec());
    }

    #[test]
    fn an_exact_duplicate_is_dropped() {
        let roots = [PathBuf::from("/m"), PathBuf::from("/m")];
        assert_eq!(prune_nested(&roots), vec![PathBuf::from("/m")]);
    }

    /// Component-wise, not string prefix: `/m/Bach` is not inside `/m/B`.
    #[test]
    fn a_shared_name_prefix_is_not_containment() {
        let roots = [PathBuf::from("/m/B"), PathBuf::from("/m/Bach")];
        assert_eq!(prune_nested(&roots), roots.to_vec());
    }

    /// **A genre tag names several genres**, and which separator depends on
    /// who wrote the file.
    #[test]
    fn a_genre_tag_is_split_into_its_terms() {
        assert_eq!(
            split_genres(Some("Rock; Blues Rock; Alternative Rock")),
            ["Rock", "Blues Rock", "Alternative Rock"]
        );
        assert_eq!(
            split_genres(Some("Electronic/Techno")),
            ["Electronic", "Techno"]
        );
        assert_eq!(split_genres(Some("Folk, Country")), ["Folk", "Country"]);
    }

    /// `Drum & Bass` is one genre, and splitting on `&` would make it two
    /// neither of which exists.
    #[test]
    fn an_ampersand_is_part_of_a_name_and_not_a_separator() {
        assert_eq!(split_genres(Some("Drum & Bass")), ["Drum & Bass"]);
    }

    #[test]
    fn a_blank_term_is_dropped_rather_than_named() {
        assert_eq!(split_genres(Some("Rock;; ; Pop")), ["Rock", "Pop"]);
        assert_eq!(split_genres(Some("   ")), Vec::<String>::new());
        assert_eq!(split_genres(None), Vec::<String>::new());
    }

    #[test]
    fn nothing_in_nothing_out() {
        assert!(prune_nested(&[]).is_empty());
    }

    /// A file, named by its audio.
    ///
    /// **The audio is the fixture's subject now.** These tests used to build a
    /// `Track` with no audio id and let move detection fall back to a *shape*
    /// — size, duration and the tags — which is what made a retag-and-move
    /// break it. Every holding has an id (#86), so what a test says is which
    /// files hold the same bytes.
    fn held(at: &str, audio: u128, title: &str) -> (PathBuf, Scanned) {
        (
            PathBuf::from(at),
            Scanned {
                audio: crate::audio_id::AudioId {
                    scheme: 1,
                    hash: audio,
                },
                title: title.into(),
                artist: "An Artist".into(),
                album: Some("An Album".into()),
                track: Some(3),
                length_ms: 211_000,
                bitrate: 320,
                sample_rate: Some(44_100),
                channels: Some(2),
                ..Default::default()
            },
        )
    }

    fn holdings(files: Vec<(PathBuf, Scanned)>) -> Holdings {
        let mut h = Holdings::new();
        for (at, seen) in files {
            h.take(at, 1000, 5_000_000, seen);
        }
        h
    }

    fn moved(h: &Holdings, from: &str, to: &str) -> Vec<Move> {
        detect_moves(h, &[PathBuf::from(from)], &[PathBuf::from(to)])
    }

    /// The dominant case: a folder rename, so the bytes are identical and only
    /// the path changed.
    #[test]
    fn a_file_at_a_new_path_is_recognised_as_the_same_track() {
        let h = holdings(vec![
            held("/m/old/03 Song.mp3", 1, "Song"),
            held("/m/Singles/03 Song.mp3", 1, "Song"),
        ]);
        assert_eq!(
            moved(&h, "/m/old/03 Song.mp3", "/m/Singles/03 Song.mp3"),
            vec![Move {
                from: PathBuf::from("/m/old/03 Song.mp3"),
                to: PathBuf::from("/m/Singles/03 Song.mp3"),
            }]
        );
    }

    /// mtime is deliberately not consulted: a copy without `-p`, an rsync
    /// without `-a`, and a restore from backup all reset it, and those are
    /// exactly the reorganisations worth surviving.
    #[test]
    fn a_move_that_reset_the_mtime_is_still_a_move() {
        let mut h = holdings(vec![held("/m/old/a.mp3", 1, "Song")]);
        let (at, seen) = held("/m/new/a.mp3", 1, "Song");
        h.take(at, 9_999_999, 5_000_000, seen);
        assert_eq!(moved(&h, "/m/old/a.mp3", "/m/new/a.mp3").len(), 1);
    }

    /// **A retag no longer breaks it, and that is the point of the change.**
    ///
    /// The old signature included the tags, so a pass that renamed *and*
    /// retagged in one go read as a deletion and an unrelated arrival — the
    /// failure that stranded three history entries (#35). An audio id is
    /// computed from the packets and a tagger cannot touch it.
    #[test]
    fn a_retagged_file_that_moved_is_still_a_move() {
        let h = holdings(vec![
            held("/m/old/a.mp3", 1, "Song"),
            held("/m/new/a.mp3", 1, "A Completely Different Title"),
        ]);
        assert_eq!(moved(&h, "/m/old/a.mp3", "/m/new/a.mp3").len(), 1);
    }

    /// And different audio is not a move however alike the tags are.
    #[test]
    fn a_different_recording_with_the_same_tags_is_not_a_move() {
        let h = holdings(vec![
            held("/m/old/a.mp3", 1, "Song"),
            held("/m/new/a.mp3", 2, "Song"),
        ]);
        assert!(moved(&h, "/m/old/a.mp3", "/m/new/a.mp3").is_empty());
    }

    /// The dangerous case. Pairing arbitrarily would attach a rating to the
    /// wrong song, which is worse than the loss this is preventing.
    #[test]
    fn two_identical_departures_are_ambiguous_and_declined() {
        let h = holdings(vec![
            held("/m/one/a.mp3", 1, "Song"),
            held("/m/two/a.mp3", 1, "Song"),
            held("/m/new/a.mp3", 1, "Song"),
        ]);
        assert!(
            detect_moves(
                &h,
                &[PathBuf::from("/m/one/a.mp3"), PathBuf::from("/m/two/a.mp3")],
                &[PathBuf::from("/m/new/a.mp3")]
            )
            .is_empty(),
            "an ambiguous departure was paired anyway"
        );
    }

    #[test]
    fn two_identical_arrivals_are_ambiguous_and_declined() {
        let h = holdings(vec![
            held("/m/old/a.mp3", 1, "Song"),
            held("/m/one/a.mp3", 1, "Song"),
            held("/m/two/a.mp3", 1, "Song"),
        ]);
        assert!(
            detect_moves(
                &h,
                &[PathBuf::from("/m/old/a.mp3")],
                &[PathBuf::from("/m/one/a.mp3"), PathBuf::from("/m/two/a.mp3")]
            )
            .is_empty(),
            "an ambiguous arrival was paired anyway"
        );
    }

    /// A real addition beside a real move: the move is found and the newcomer
    /// is left alone.
    #[test]
    fn a_genuine_addition_alongside_a_move_is_not_swept_up() {
        let h = holdings(vec![
            held("/m/old/a.mp3", 1, "Song"),
            held("/m/new/a.mp3", 1, "Song"),
            held("/m/new/b.mp3", 2, "Another"),
        ]);
        let moves = detect_moves(
            &h,
            &[PathBuf::from("/m/old/a.mp3")],
            &[PathBuf::from("/m/new/a.mp3"), PathBuf::from("/m/new/b.mp3")],
        );
        assert_eq!(moves.len(), 1);
        assert_eq!(moves[0].to, PathBuf::from("/m/new/a.mp3"));
    }

    #[test]
    fn nothing_gone_or_nothing_arrived_means_no_moves() {
        let h = holdings(vec![held("/m/new/a.mp3", 1, "Song")]);
        assert!(detect_moves(&h, &[], &[PathBuf::from("/m/new/a.mp3")]).is_empty());
        assert!(detect_moves(&h, &[PathBuf::from("/m/old/a.mp3")], &[]).is_empty());
    }

    /// A reassociation the listener never sees is as bad as the silent loss it
    /// replaces — worse, if it ever guesses wrong.
    #[test]
    fn the_report_says_when_tracks_moved() {
        let mut r = ScanReport::default();
        // Not `contains("moved")` — "0 removed" contains it.
        assert!(
            !r.to_string().contains('\n'),
            "no moves must add no second line"
        );
        r.moves.push(Move {
            from: PathBuf::from("/a"),
            to: PathBuf::from("/b"),
        });
        assert!(r.to_string().contains("1 track moved"), "{r}");
        r.moves.push(Move {
            from: PathBuf::from("/c"),
            to: PathBuf::from("/d"),
        });
        assert!(r.to_string().contains("2 tracks moved"), "{r}");
    }

    /// **A file whose tags will not parse is still held** (#86, #141).
    ///
    /// It used to assert that such a file still got a title, because the scan
    /// read tags and ingest is total. The scan reads no tags now (#141), so
    /// the promise it can still make is the one a holding is *for*: the audio
    /// read, so the file has an identity and a rescan can skip it. Ingest
    /// being total is asserted where ingest happens.
    #[test]
    fn a_file_whose_tags_will_not_parse_is_still_held() {
        let d = scan_dir("tagless");
        let path = d.join("07 A Real Song.wav");
        write_wav(&path, 4410);

        let mut store = Holdings::new();
        let report = scan(&mut store, &[d]);

        assert_eq!(report.errors, vec![], "readable audio is never an error");
        assert_eq!(store.len(), 1, "it is held");
        let held = store.get(&path).expect("at its own path");
        assert_eq!(
            held.audio,
            crate::audio_id::of(&path).expect("the audio reads"),
            "held under an identity its bytes give it, tags or no tags"
        );
        assert!(held.mtime > 0, "and a rescan can skip it");
    }

    /// The failure the old signature could not survive (#35).
    ///
    /// Retag and move in one pass. The shape key includes title, artist and
    /// album, so it changes at the same moment the path does and the pairing is
    /// lost — which is why this project carried a rule about doing the two
    /// separately, and why a renumber stranded a four-star rating here. The
    /// audio id is computed from the packets, so a tagger cannot reach it.
    #[test]
    fn a_file_that_moves_and_is_retagged_in_one_pass_is_still_recognised() {
        let d = scan_dir("moved-and-retagged");
        let from = d.join("old name.wav");
        write_wav(&from, 4410);
        {
            use lofty::config::WriteOptions;
            use lofty::tag::{Tag, TagExt, TagType};
            let mut tag = Tag::new(TagType::Id3v2);
            tag.insert_text(ItemKey::TrackTitle, "Before".into());
            tag.save_to_path(&from, WriteOptions::default()).unwrap();
        }

        let mut store = Holdings::default();
        let report = scan(&mut store, std::slice::from_ref(&d));
        assert_eq!(report.added, 1);

        // Move it *and* rewrite the tags the old key relied on.
        let to = d.join("new name.wav");
        std::fs::rename(&from, &to).unwrap();
        {
            use lofty::config::WriteOptions;
            use lofty::tag::{Tag, TagExt, TagType};
            let mut tag = Tag::new(TagType::Id3v2);
            tag.insert_text(ItemKey::TrackTitle, "Completely Different".into());
            tag.insert_text(ItemKey::TrackArtist, "Someone Else".into());
            tag.insert_text(ItemKey::AlbumTitle, "Another Album".into());
            tag.save_to_path(&to, WriteOptions::default()).unwrap();
        }

        let report = scan(&mut store, std::slice::from_ref(&d));
        assert_eq!(
            report.moves.len(),
            1,
            "the audio is the same file however it is labelled"
        );
        assert_eq!(report.moves[0].from, from);
        assert_eq!(report.moves[0].to, to);
    }

    /// A label sampler's imprint belongs in `TPUB`, not in `albumartist`
    /// where it reads as an artist. Vorbis spells the same thing
    /// `ORGANIZATION`, and `LABEL` is a common third spelling.
    #[test]
    fn the_label_is_read_from_a_real_file() {
        let d = scan_dir("label");
        // A real WAV, so lofty parses it, with tags written onto it.
        let path = d.join("tagged.wav");
        write_wav(&path, 4410);

        use lofty::config::WriteOptions;
        use lofty::tag::{Tag, TagExt, TagType};
        let mut tag = Tag::new(TagType::Id3v2);
        tag.insert_text(ItemKey::TrackArtist, "ANTIGONE".into());
        tag.insert_text(ItemKey::AlbumArtist, "Various Artists".into());
        // A sampler has an album, and it has to: the album-level credit hangs
        // off a release group, so "Various Artists" with no album names
        // nothing the model can file under (#86).
        tag.insert_text(ItemKey::AlbumTitle, "Trip Recordings Vol. 1".into());
        tag.insert_text(ItemKey::Publisher, "trip recordings".into());
        tag.save_to_path(&path, WriteOptions::default()).unwrap();

        let seen = crate::model::ingest::read(&path).expect("reads");
        assert_eq!(seen.label.as_deref(), Some("trip recordings"));
        assert_eq!(seen.artist, "ANTIGONE");
        assert_eq!(seen.album_artist.as_deref(), Some("Various Artists"));

        // And the label must not leak into what the tree files it under. That
        // is the librarian's answer now rather than a `Track` method, which is
        // the point of there being one place it is decided.
        let l = crate::bridge::imported(
            &{
                let mut h = Holdings::new();
                h.take(path.clone(), 0, 0, seen.clone());
                h
            },
            &std::collections::BTreeSet::new(),
            crate::model::id::Id::derived(b"laptop"),
            100,
        );
        let recording = l.recordings().next().expect("a recording");
        assert_eq!(
            l.row(recording).expect("a row").file_under,
            "Various Artists"
        );
    }

    #[test]
    fn extension_filter_is_case_insensitive() {
        assert!(is_audio(Path::new("/a/b.MP3")));
        assert!(is_audio(Path::new("/a/b.opus")));
        assert!(is_audio(Path::new("/a/b.FLAC")));
        assert!(!is_audio(Path::new("/a/cover.jpg")));
        assert!(!is_audio(Path::new("/a/notes.txt")));
        assert!(!is_audio(Path::new("/a/noext")));
    }
}
