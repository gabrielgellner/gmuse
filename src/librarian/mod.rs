//! **The librarian thread.** It owns the log and the projection; everything
//! else holds a pointer to the last thing it published.
//!
//! `gmuse-engine-architecture.md` §8.5 is the design.
//!
//! ## The control thread never folds
//!
//! A full fold is hundreds of milliseconds on this library — the log parse
//! alone is 314ms — which I6 does not permit on a thread with a deadline.
//!
//! Nothing enforces that by construction: a fold is an ordinary function call
//! and any thread can make one. What keeps it true is that [`Desk`] hands out
//! `Arc<Answer>` and no `&mut` to anything foldable, so the only code that *can*
//! fold is on the far side of the channel.
//!
//! ## Writes are sent, not taken
//!
//! A `Mutex<Instance>` would be smaller, and would put every writer back on
//! the caller's thread. A rating taken while a scan held the lock would block
//! the render, which is I6's head-of-line blocking wearing a different hat.
//!
//! So a write goes on a queue and the caller carries on; the next publication
//! is how it finds out. [`Desk::settled`] is for the two places that must
//! wait — a test, and quitting.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use arc_swap::ArcSwap;
use crossbeam_channel::{Sender, unbounded};

use crate::facts::{Blast, Record, Touched};
use crate::model::durable::LogError;
use crate::model::entity::Device;
use crate::model::id::Id;
use crate::model::instance::Instance;
use crate::model::seam::Librarian;
use crate::operations::scan::Holdings;
use crate::projection::Projected;
use crate::projection::catalogue::Catalogue;

/// What the desk hands back when you ask: everything derived from the log,
/// folded together.
///
/// The two travel as a pair because they are one fold. Refreshing them at
/// separate moments would let them disagree about what the log says.
#[derive(Default)]
pub struct Answer {
    /// What the player is allowed to ask (#77's seam).
    pub library: Librarian,
    /// Plays, ratings and views.
    ///
    /// Its own `Arc` inside the published one, so a holder that only wants
    /// this — the UI's rating and play lookups — can keep it without pinning
    /// the whole fold.
    pub listened: Arc<Catalogue>,
    /// What moved to produce this answer, relative to the one before it.
    ///
    /// A reader that keeps its own derived state — a rendered list, a filtered
    /// selection — reads this to find out how much of it to throw away.
    pub changed: Changed,
    /// What this device holds, as the last scan left it.
    ///
    /// Published rather than kept privately because the caller persists it:
    /// the scan runs here, so this is the only place its result exists.
    pub holdings: Arc<Holdings>,
    /// A scan is running. Display only — nothing waits on it.
    pub scanning: bool,
}

/// A change to the log, or a reason to publish again.
enum Job {
    /// Apply this to the log, flush it, and publish. Boxed because it
    /// crosses a thread.
    Say(Box<dyn FnOnce(&mut Instance) + Send>),
    /// A scan reconciled the disk; these are the holdings now.
    Holdings(Holdings),
    /// Reconcile the disk under these roots, here rather than on the caller's
    /// thread.
    Scan {
        roots: Vec<PathBuf>,
        /// Where to persist what it found, so the next launch starts warm.
        /// `None` writes nothing, which is what a test wants.
        cache: Option<PathBuf>,
    },
    /// Nothing changed here — fold and publish anyway.
    Advance,
    /// Reply when everything queued before this has been published.
    Settled(Sender<()>),
}

/// Where the rest of the program asks.
///
/// The metaphor is the literal one: you ask at the desk rather than walking
/// into the stacks yourself. Nothing here hands out the log — you send a
/// request and read what was published — which is the shape I15 and I16
/// describe. (`Librarian` was already the read surface in `model::seam`, so
/// the thing holding the thread needed its own word.)
///
/// Cheap to clone: the current answer costs one atomic load and an `Arc`
/// bump, so a draw loop can take a fresh one every frame.
pub struct Desk {
    device: Id<Device>,
    current: Arc<ArcSwap<Answer>>,
    /// What finished scans had to say. Drained when the caller feels like it;
    /// nothing here waits on a reader.
    pub scans: crossbeam_channel::Receiver<crate::operations::scan::ScanReport>,
    jobs: Sender<Job>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Desk {
    /// Start the thread and publish the first fold.
    ///
    /// The first fold happens here, synchronously, so a caller never sees an
    /// empty library. Every fold after it is on the thread.
    pub fn open(
        log: Instance,
        held: Holdings,
        device: Id<Device>,
        now: i64,
        data_dir: Option<PathBuf>,
    ) -> Desk {
        // The first answer is everything arriving at once, which is a rebuild
        // from the point of view of anyone holding derived state.
        let held = Arc::new(held);
        let current = Arc::new(ArcSwap::from_pointee(fold(
            &log,
            &held,
            device,
            now,
            Changed::Rebuild,
            false,
        )));
        let (tx, rx) = unbounded::<Job>();
        let (reports, scans) = unbounded();
        let published = Arc::clone(&current);

        let thread = std::thread::Builder::new()
            .name("librarian".into())
            .spawn(move || {
                let mut log = log;
                let mut held = held;
                for job in rx {
                    // **Measured against the log before the job**, so what is
                    // published is what this job did rather than what the log
                    // happens to hold. A scan that appends nothing is not a
                    // change, however many files it walked.
                    let before = log.records().clone();
                    let mut moved = Changed::Nothing;
                    let mut scanned = None;
                    let settled = match job {
                        Job::Say(say) => {
                            say(&mut log);
                            moved = Changed::of(log.records().difference(&before));
                            if let Some(dir) = &data_dir {
                                // A failed commit is not fatal: the in-memory
                                // log is still right, and refusing the fact
                                // because the disk refused it would lose the
                                // rating as well as the write.
                                let _ = commit(&mut log, dir);
                            }
                            None
                        }
                        Job::Holdings(fresh) => {
                            // Not a log change, but every locator may have
                            // moved and none of them is named here.
                            moved = Changed::Wide;
                            held = Arc::new(fresh);
                            None
                        }
                        // **The scan runs here, and nothing waits on it.** It
                        // is the one job that can take tens of seconds — a
                        // cold pass re-reads every file — and the caller is a
                        // thread with a deadline.
                        //
                        // Published twice on purpose: once to say it started,
                        // so a view can show it, and once with the result.
                        Job::Scan { roots, cache } => {
                            published.store(Arc::new(fold(
                                &log,
                                &held,
                                device,
                                now,
                                Changed::Nothing,
                                true,
                            )));
                            let mut fresh = (*held).clone();
                            let report = crate::operations::scan::scan(&mut fresh, &roots);
                            // **Persisted here, because the holdings are
                            // produced here.** Without it every launch is a
                            // cold pass: the read cache is what turns 24s into
                            // 0.3s, and it only exists if somebody wrote it.
                            //
                            // Never an empty one over a good one — that is how
                            // a working library becomes an empty one after a
                            // format bump, taking the roots with it.
                            if let Some(path) = &cache
                                && !(fresh.is_empty() && roots.is_empty())
                                && let Err(e) = crate::cache::save(path, &fresh, &roots, None)
                            {
                                eprintln!("cache: {e}");
                            }
                            scanned = Some(report);
                            moved = Changed::Wide;
                            held = Arc::new(fresh);
                            None
                        }
                        Job::Advance => None,
                        Job::Settled(reply) => Some(reply),
                    };
                    // **Only when something moved.** A job that changed
                    // nothing — a settle, an advance that was already caught
                    // up — must not republish, because `changed` describes the
                    // publication it rides on and republishing `Nothing` would
                    // erase what the job before it had to say. A reader that
                    // looked between the two would be told nothing happened.
                    if moved != Changed::Nothing {
                        published.store(Arc::new(fold(&log, &held, device, now, moved, false)));
                    }
                    if let Some(report) = scanned {
                        let _ = reports.send(report);
                    }
                    if let Some(reply) = settled {
                        // Dropped receiver means the waiter gave up, which is
                        // not this thread's problem.
                        let _ = reply.send(());
                    }
                }
            })
            .expect("librarian thread");

        Desk {
            device,
            current,
            scans,
            jobs: tx,
            thread: Some(thread),
        }
    }

    /// A desk over an empty log that writes nothing, for tests.
    pub fn detached(device: Id<Device>) -> Desk {
        Desk::open(Instance::new(device), Holdings::default(), device, 0, None)
    }

    /// Which device this is. Held here because the log is, and `Instance` is
    /// on the far side of the channel.
    pub fn device(&self) -> Id<Device> {
        self.device
    }

    /// The latest published answer. One atomic load.
    pub fn current(&self) -> Arc<Answer> {
        self.current.load_full()
    }

    /// Append to the log, off this thread.
    ///
    /// Returns immediately. The caller learns the result through the next
    /// [`Desk::current`], like every other reader: a writer is not privileged
    /// just because it was the one who wrote.
    pub fn say(&self, say: impl FnOnce(&mut Instance) + Send + 'static) {
        let _ = self.jobs.send(Job::Say(Box::new(say)));
    }

    /// A scan happened elsewhere. These are the holdings now.
    pub fn holdings(&self, held: Holdings) {
        let _ = self.jobs.send(Job::Holdings(held));
    }

    /// Reconcile the disk, off this thread.
    ///
    /// Returns immediately, which is the point: a cold pass re-reads every
    /// file. The caller learns it started from [`Answer::scanning`] and learns
    /// what it found from [`Desk::scans`].
    pub fn scan(&self, roots: Vec<PathBuf>, cache: Option<PathBuf>) {
        let _ = self.jobs.send(Job::Scan { roots, cache });
    }

    /// Publish again without changing anything.
    pub fn advance(&self) {
        let _ = self.jobs.send(Job::Advance);
    }

    /// Block until everything queued so far has been published.
    ///
    /// For a test asserting on what a write did, and for quitting, where the
    /// last rating has to reach the disk. Not for ordinary reads: a read that
    /// waits has put the fold back on the caller's thread by another route.
    pub fn settled(&self) {
        let (tx, rx) = unbounded();
        if self.jobs.send(Job::Settled(tx)).is_ok() {
            let _ = rx.recv();
        }
    }
}

impl Drop for Desk {
    fn drop(&mut self) {
        // Dropping the last sender ends the thread's `for job in rx`, and it
        // drains first — so a rating queued a moment before quit still reaches
        // the disk. The join is what makes that ordering observable.
        drop(std::mem::replace(&mut self.jobs, unbounded().0));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// What moved, at the coarsest grain that is still true.
///
/// A notification saying only "something changed" makes every reader re-read
/// everything, which at 7,271 rows is the same as saying nothing. `Fact::blast`
/// already classifies each fact; this is that classification carried to
/// whoever is watching.
///
/// **A lattice**, which is what makes coalescing well-defined: merging two
/// notifications can only ever move up it, so a reader that missed some is
/// told something weaker than the truth and never something stronger.
///
/// ```text
/// Nothing  <  These(ids)  <  Wide  <  Rebuild
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Changed {
    /// Nothing arrived.
    #[default]
    Nothing,
    /// Exactly these entries, and nothing else.
    These(BTreeSet<Touched>),
    /// Something arrived whose reach is not derivable from the facts. Re-query
    /// whatever you display.
    Wide,
    /// Start again.
    Rebuild,
}

impl Changed {
    /// What a batch of records moved.
    pub fn of<'a>(records: impl IntoIterator<Item = &'a Record>) -> Changed {
        let mut out = Changed::Nothing;
        for record in records {
            out = out.and(match record.fact.blast() {
                Blast::Narrow => Changed::These(record.fact.touches().into_iter().collect()),
                Blast::Wide => Changed::Wide,
                Blast::Rebuild => Changed::Rebuild,
            });
        }
        out
    }

    /// Join, which is the coalescing rule: up the lattice, never down.
    ///
    /// Two sets union. Anything meeting `Wide` is `Wide`, because a reader
    /// cannot act on a partial list once part of the change is unlocatable —
    /// telling it the ids it *could* see would be worse than telling it to
    /// re-query, since it would stop looking.
    #[must_use]
    pub fn and(self, other: Changed) -> Changed {
        match (self, other) {
            (Changed::Rebuild, _) | (_, Changed::Rebuild) => Changed::Rebuild,
            (Changed::Wide, _) | (_, Changed::Wide) => Changed::Wide,
            (Changed::Nothing, one) | (one, Changed::Nothing) => one,
            (Changed::These(mut a), Changed::These(b)) => {
                a.extend(b);
                Changed::These(a)
            }
        }
    }
}

/// Persist what a named operation appended. **The one door.**
///
/// [`Instance::flush`] is crate-private, so this is the only way facts reach
/// disk: the binary crate cannot write at all except by calling this, which
/// makes "one appender" a property of the build rather than of everyone's
/// good intentions.
///
/// It deliberately does **not** refresh the projection snapshot, though that
/// is where such a thing would belong. The snapshot lives at one fixed path
/// under the cache directory while the log is named by a parameter, so a
/// write against any other data directory — every test that applies a
/// proposal — would overwrite the snapshot of the real library with its own.
/// Refreshing here needs the snapshot keyed by the log it describes first.
pub fn commit(log: &mut Instance, data_dir: &std::path::Path) -> Result<usize, LogError> {
    log.flush(data_dir)
}

/// The projection as it stands, for a read.
///
/// **Snapshot first.** A print of the log files is one `stat` each and decides
/// on its own whether the fold is current, so a caught-up read never opens the
/// log — parsing is the expensive half and caching only the fold would cache
/// the cheap one.
///
/// **It never scans.** A fold is a derivation and a read may do it; a scan is
/// a discovery and a read may not (`access-layers.md` §3a). A read that walks
/// the disk answers with what the tags say rather than what the library
/// recorded, which is the one thing the library is for.
pub fn reading(data_dir: &std::path::Path, device: Id<Device>, now: i64) -> Projected {
    let at = crate::cache::projection_path();
    let print = crate::cache::LogPrint::of(data_dir);
    match crate::cache::current_projection(&at, &print) {
        Some(ready) => ready,
        None => {
            // **Through `advance`, not a bare fold.** It is the only entry
            // point, so the caught-up case costs one comparison per device
            // rather than being a fold nobody checked the need for.
            let (log, _) = Instance::open(data_dir, device, now);
            let mut folded = Projected::default();
            folded.advance(log.records());
            crate::cache::save_projection(&at, &print, &folded);
            folded
        }
    }
}

/// The one fold, and the only place it happens.
fn fold(
    log: &Instance,
    held: &Arc<Holdings>,
    device: Id<Device>,
    now: i64,
    changed: Changed,
    scanning: bool,
) -> Answer {
    Answer {
        library: crate::bridge::librarian(held, log.records(), device, now),
        listened: Arc::new(log.catalogue()),
        changed,
        holdings: Arc::clone(held),
        scanning,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::Fact;
    use crate::model::entity::{Artist, Recording};

    fn device() -> Id<Device> {
        Id::derived(b"laptop")
    }

    fn desk() -> Desk {
        Desk::detached(device())
    }

    /// **A job that changed nothing does not erase what the last one said.**
    ///
    /// `changed` rides on the publication it describes, so a settle or a
    /// caught-up advance republishing `Nothing` would tell a reader that
    /// looked between them that nothing had happened. Found by a test that
    /// asserted the scan's `Wide` and got `Nothing`, because `settled` had
    /// published over it.
    #[test]
    fn a_no_op_job_does_not_overwrite_what_moved() {
        let desk = desk();
        let song = Id::<Recording>::from_uuid(uuid::Uuid::from_u128(11));
        desk.say(move |log| {
            log.append_fact(
                1_000,
                Fact::RecordingRated {
                    recording: song,
                    stars: Some(5),
                },
            );
        });
        desk.settled();

        // Several no-ops, any one of which used to wipe it.
        desk.advance();
        desk.settled();
        desk.advance();
        desk.settled();

        assert_eq!(
            desk.current().changed,
            Changed::These([Touched::Recording(song)].into_iter().collect()),
            "a no-op job republished over the rating"
        );
    }

    /// A scan runs on the librarian thread and reports back through a
    /// channel, leaving its result in the published holdings.
    ///
    /// **That it does not block is structural, not asserted here.** `scan`
    /// returns `()` and the report arrives on `scans`, so there is nothing a
    /// caller could wait on by accident. A test that tried to prove the
    /// timing would either race the thread or need a root big enough to be
    /// slow, and a racy test is worse than a stated one.
    #[test]
    fn a_scan_reports_through_the_channel_and_lands_in_the_holdings() {
        let dir = std::env::temp_dir().join("gmuse-librarian-scan");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let desk = desk();
        assert!(desk.scans.try_recv().is_err(), "nothing has scanned yet");

        desk.scan(vec![dir.clone()], None);
        desk.settled();

        assert!(
            desk.scans.try_recv().is_ok(),
            "the scan never reported, so it never ran"
        );
        let answer = desk.current();
        assert!(!answer.scanning, "it finished, so it should say so");
        assert_eq!(
            answer.changed,
            Changed::Wide,
            "a scan may move any locator and names none of them"
        );
    }

    /// **A scan with nowhere to write leaves no cache.** The guard that
    /// matters is the other one — never an empty cache over a good one — but
    /// this is the half a test can reach without a real library.
    #[test]
    fn a_scan_with_no_cache_path_writes_nothing() {
        let dir = std::env::temp_dir().join("gmuse-librarian-nocache");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let desk = desk();
        desk.scan(vec![dir.clone()], None);
        desk.settled();

        let wrote: Vec<_> = std::fs::read_dir(&dir)
            .expect("readable")
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert!(
            wrote.is_empty(),
            "a scan wrote {wrote:?} with no cache path"
        );
    }

    /// **One appender, and the compiler holds most of it.** `Instance::flush`
    /// is crate-private, so `src/main.rs` — a separate crate — cannot write at
    /// all except through `commit`. What the compiler cannot hold is the
    /// inside of this crate, where `flush` is still reachable, so that half is
    /// asserted here.
    ///
    /// Against the source, because no type can carry it: `flush` is an
    /// ordinary method and any module could call one.
    #[test]
    fn nothing_but_commit_puts_facts_on_disk() {
        let mut offenders = Vec::new();
        let mut stack = vec![std::path::PathBuf::from("src")];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("src is readable").flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") {
                    continue;
                }
                // The log's own module defines `flush`; this one is the door.
                let name = path.to_string_lossy().replace('\\', "/");
                if name.ends_with("model/instance.rs") || name.ends_with("librarian/mod.rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).expect("readable");
                let body = match text.find("#[cfg(test)]") {
                    Some(at) => &text[..at],
                    None => &text[..],
                };
                for (n, line) in body.lines().enumerate() {
                    // `flush` is also a resampler method and a writer method;
                    // only the log's takes a data directory.
                    if line.contains(".flush(data_dir")
                        || line.contains(".flush(&loaded.data_dir")
                        || line.contains(".flush(&dir)")
                    {
                        offenders.push(format!("{name}:{}", n + 1));
                    }
                }
            }
        }

        assert!(
            offenders.is_empty(),
            "these write to the log without going through `commit`: {offenders:?}"
        );
    }

    /// A reader holds its answer while the next one is published beside it.
    /// This is what lets the draw loop take a fold and keep drawing from it.
    #[test]
    fn an_answer_already_taken_survives_the_next_publication() {
        let desk = desk();
        let before = desk.current();

        desk.say(|log| {
            log.append_fact(
                1_000,
                Fact::RecordingCatalogued {
                    recording: Id::<Recording>::from_uuid(uuid::Uuid::from_u128(1)),
                    title: "Xtal".into(),
                    artist: Id::<Artist>::derived(b"aphex twin"),
                },
            );
        });
        desk.settled();
        let after = desk.current();

        assert!(
            before.listened.recordings.is_empty(),
            "the answer taken first was rewritten underneath its holder"
        );
        assert_eq!(after.listened.recordings.len(), 1, "the new one has it");
        assert!(
            !Arc::ptr_eq(&before, &after),
            "the same allocation was mutated rather than replaced"
        );
    }

    /// Writes arrive in the order they were sent, which is what makes a queue
    /// an acceptable substitute for a lock: two ratings on one recording must
    /// not land backwards.
    #[test]
    fn what_is_said_first_is_applied_first() {
        let desk = desk();
        let rec = Id::<Recording>::from_uuid(uuid::Uuid::from_u128(2));
        for title in ["first", "second", "third"] {
            desk.say(move |log| {
                log.append_fact(
                    1_000,
                    Fact::RecordingCatalogued {
                        recording: rec,
                        title: title.into(),
                        artist: Id::<Artist>::derived(b"someone"),
                    },
                );
            });
        }
        desk.settled();

        assert_eq!(
            desk.current()
                .listened
                .recordings
                .get(&rec)
                .map(|r| &*r.title),
            Some("third"),
            "last writer did not win, so the queue reordered"
        );
    }

    /// Quitting does not lose what was queued. A rating appended a moment
    /// before exit is still on the librarian thread, so `Drop` draining the
    /// queue before it joins is the only thing between it and the disk.
    #[test]
    fn dropping_the_desk_flushes_what_was_still_queued() {
        let dir = std::env::temp_dir().join("gmuse-librarian-drop");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let rec = Id::<Recording>::from_uuid(uuid::Uuid::from_u128(3));
        {
            let desk = Desk::open(
                Instance::new(device()),
                Holdings::default(),
                device(),
                1_000,
                Some(dir.clone()),
            );
            desk.say(move |log| {
                log.append_fact(
                    1_000,
                    Fact::RecordingCatalogued {
                        recording: rec,
                        title: "Aisatsana".into(),
                        artist: Id::<Artist>::derived(b"aphex twin"),
                    },
                );
            });
            // Deliberately no `settled()`. Drop is the thing under test.
        }

        let (back, damage) = Instance::open(&dir, device(), 2_000);
        assert!(damage.is_empty(), "the log came back damaged: {damage:?}");
        assert!(
            back.records().iter().any(|r| matches!(
                &r.fact,
                Fact::RecordingCatalogued { recording, .. } if *recording == rec
            )),
            "a fact queued at quit never reached the disk"
        );
    }
}

#[cfg(test)]
mod change_tests {
    //! The lattice, and what a fact says it reached.

    use super::*;
    use crate::facts::Fact;
    use crate::model::entity::{Artist, Recording};

    fn rec(n: u128) -> Touched {
        Touched::Recording(Id::from_uuid(uuid::Uuid::from_u128(n)))
    }

    fn these(ids: impl IntoIterator<Item = Touched>) -> Changed {
        Changed::These(ids.into_iter().collect())
    }

    /// One edge per case, because the join is what makes coalescing safe and
    /// a wrong edge is silent: a reader told `These` when the truth was `Wide`
    /// stops looking for what it was not told about.
    #[test]
    fn the_join_only_ever_moves_up_the_lattice() {
        assert_eq!(Changed::Nothing.and(Changed::Nothing), Changed::Nothing);
        assert_eq!(Changed::Nothing.and(these([rec(1)])), these([rec(1)]));
        assert_eq!(these([rec(1)]).and(Changed::Nothing), these([rec(1)]));
        assert_eq!(
            these([rec(1)]).and(these([rec(2)])),
            these([rec(1), rec(2)]),
            "two sets union"
        );
        assert_eq!(these([rec(1)]).and(Changed::Wide), Changed::Wide);
        assert_eq!(Changed::Wide.and(these([rec(1)])), Changed::Wide);
        assert_eq!(Changed::Wide.and(Changed::Wide), Changed::Wide);
        assert_eq!(Changed::Wide.and(Changed::Rebuild), Changed::Rebuild);
        assert_eq!(these([rec(1)]).and(Changed::Rebuild), Changed::Rebuild);
        assert_eq!(Changed::Rebuild.and(Changed::Nothing), Changed::Rebuild);
    }

    /// **A narrow fact names what it reached**, which is the definition of
    /// narrow rather than a property of it.
    #[test]
    fn a_narrow_fact_names_exactly_what_it_touched() {
        let song = Id::<Recording>::from_uuid(uuid::Uuid::from_u128(7));
        let fact = Fact::RecordingRated {
            recording: song,
            stars: Some(4),
        };
        assert_eq!(fact.blast(), Blast::Narrow);
        assert_eq!(fact.touches(), vec![Touched::Recording(song)]);
    }

    /// **And a wide one does not claim to.** `ArtistMerged` hands over two ids
    /// and leaves "every row crediting the loser" to be found, so a
    /// notification built from it must send the reader back to the library
    /// rather than handing it a list that looks complete.
    #[test]
    fn a_wide_fact_is_reported_wide_however_many_ids_it_mentions() {
        let fact = Fact::ArtistMerged {
            loser: Id::<Artist>::derived(b"afx"),
            winner: Id::<Artist>::derived(b"aphex twin"),
        };
        assert_eq!(fact.blast(), Blast::Wide);
        assert!(
            !fact.touches().is_empty(),
            "it does mention ids — that is the trap"
        );

        let clock = crate::model::clock::HybridClock::new(Id::derived(b"laptop"));
        let mut clock = clock;
        let records: BTreeSet<Record> = [fact]
            .into_iter()
            .map(|fact| Record {
                stamp: clock.stamp(1_000),
                fact,
            })
            .collect();
        assert_eq!(
            Changed::of(&records),
            Changed::Wide,
            "the ids it mentions were reported as the ids it changed"
        );
    }

    /// The largest fact in the log is narrow, and this is why: it names every
    /// encoding it touches.
    #[test]
    fn holdings_names_every_encoding_it_asserts() {
        let one = Id::<crate::model::entity::Encoding>::derived(b"one");
        let two = Id::<crate::model::entity::Encoding>::derived(b"two");
        let fact = Fact::HoldingsAsserted {
            device: Id::derived(b"laptop"),
            held: [(one, "/a".to_string()), (two, "/b".to_string())]
                .into_iter()
                .collect(),
        };
        assert_eq!(fact.blast(), Blast::Narrow);
        let touched = fact.touches();
        assert!(touched.contains(&Touched::Encoding(one)));
        assert!(touched.contains(&Touched::Encoding(two)));
    }
}
