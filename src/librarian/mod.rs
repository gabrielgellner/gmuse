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

use std::path::PathBuf;
use std::sync::Arc;

use arc_swap::ArcSwap;
use crossbeam_channel::{Sender, unbounded};

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
}

/// A change to the log, or a reason to publish again.
enum Job {
    /// Apply this to the log, flush it, and publish. Boxed because it
    /// crosses a thread.
    Say(Box<dyn FnOnce(&mut Instance) + Send>),
    /// A scan reconciled the disk; these are the holdings now.
    Holdings(Holdings),
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
        let current = Arc::new(ArcSwap::from_pointee(fold(&log, &held, device, now)));
        let (tx, rx) = unbounded::<Job>();
        let published = Arc::clone(&current);

        let thread = std::thread::Builder::new()
            .name("librarian".into())
            .spawn(move || {
                let mut log = log;
                let mut held = held;
                for job in rx {
                    let settled = match job {
                        Job::Say(say) => {
                            say(&mut log);
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
                            held = fresh;
                            None
                        }
                        Job::Advance => None,
                        Job::Settled(reply) => Some(reply),
                    };
                    published.store(Arc::new(fold(&log, &held, device, now)));
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

    /// A scan happened. These are the holdings now.
    pub fn holdings(&self, held: Holdings) {
        let _ = self.jobs.send(Job::Holdings(held));
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
            let (log, _) = Instance::open(data_dir, device, now);
            let folded = Projected::rebuild(log.records());
            crate::cache::save_projection(&at, &print, &folded);
            folded
        }
    }
}

/// The one fold, and the only place it happens.
fn fold(log: &Instance, held: &Holdings, device: Id<Device>, now: i64) -> Answer {
    Answer {
        library: crate::bridge::librarian(held, log.records(), device, now),
        listened: Arc::new(log.catalogue()),
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
