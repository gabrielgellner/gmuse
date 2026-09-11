//! **The librarian thread** — #206, `gmuse-engine-architecture.md` §8.5, I16.
//!
//! The log and the fold move off the thread that renders, and what the rest of
//! the program holds is a pointer to the last thing published.
//!
//! ## What this ends
//!
//! The fold ran on the UI thread, and the borrow checker was what made it
//! safe: `run_tui` owned the `Librarian`, lent it to the draw loop, and the
//! loop had to **return** — with a `Stop::Refold` — before anything could
//! rebuild it, because the borrow outlived every command. That is why a
//! rating, a rescan and a `:view` change all unwound the loop and re-entered
//! it.
//!
//! An [`Arc`] has no such problem. A reader holds one and keeps holding it
//! while the next one is published beside it, so the control path never has to
//! stop to be given a fresh answer. The unwinding was never about the UI; it
//! was about who owned the fold.
//!
//! ## The rule this exists to make true
//!
//! **I16: the control thread never folds.** A full fold is hundreds of
//! milliseconds on this library — the log parse alone is 314ms after
//! compaction — which I6 does not permit on a thread with a deadline.
//!
//! Nothing enforces that by construction: a fold is an ordinary function call
//! and any thread can make one. What makes it hold is that the [`Desk`] hands
//! out `Arc<Held>` and no `&mut` to anything foldable, so the only code that
//! *can* fold is the code on the other side of the channel.
//!
//! ## Why a job queue rather than a lock
//!
//! A `Mutex<Instance>` would be smaller and would put every writer back on the
//! caller's thread — which is the thing being removed. A rating taken while a
//! scan holds the lock would block the render, and that is exactly I6's
//! head-of-line blocking wearing a different hat.
//!
//! So writes are **sent**, not taken: the caller appends to a queue and
//! carries on, and the next publication is how it finds out. [`Desk::settled`]
//! exists for the two places that genuinely must wait — a test, and quitting.

use std::path::PathBuf;
use std::sync::Arc;

use arc_swap::ArcSwap;
use crossbeam_channel::{Sender, unbounded};

use crate::model::entity::Device;
use crate::model::id::Id;
use crate::model::instance::Instance;
use crate::model::seam::Librarian;
use crate::operations::scan::Holdings;
use crate::projection::catalogue::Catalogue;

/// One published answer: everything derived from the log, folded together.
///
/// The two travel as a pair because they are one fold. They were separate
/// fields updated at separate moments — `library_view` rebuilt on `Refold`,
/// `ui.listened` on every appended fact — and a pair that is only sometimes
/// refreshed together is a pair that can disagree about what the log says.
#[derive(Default)]
pub struct Held {
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
    /// Apply this to the log, flush it, and publish.
    ///
    /// Boxed because it crosses a thread: the closure form is what every
    /// caller of `ui::record` already writes, and keeping it means this is a
    /// change of *where* the work happens rather than of how it is expressed.
    Say(Box<dyn FnOnce(&mut Instance) + Send>),
    /// A scan reconciled the disk; these are the holdings now.
    Held(Holdings),
    /// Nothing changed here — fold and publish anyway.
    Advance,
    /// Answer when everything queued before this has been published.
    Settled(Sender<()>),
}

/// Where the rest of the program asks.
///
/// Cheap to clone: a reader wanting the current answer costs one atomic load
/// and an `Arc` bump, which is what lets a draw loop take a fresh one every
/// frame without thinking about it.
pub struct Desk {
    device: Id<Device>,
    current: Arc<ArcSwap<Held>>,
    jobs: Sender<Job>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Desk {
    /// Start the thread and publish the first fold.
    ///
    /// The first fold happens **here, synchronously**, so a caller never sees
    /// an empty library before the thread has caught up. Every fold after it
    /// is on the thread.
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
                                // **Flushed here, and a failure is not fatal.**
                                // The in-memory log is still right, and the
                                // alternative — refusing the fact because the
                                // disk refused it — loses the rating as well
                                // as the file.
                                let _ = log.flush(dir);
                            }
                            None
                        }
                        Job::Held(fresh) => {
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

    /// Which device this is. Held here because the log is, and a caller that
    /// needs it has no other honest source — `Instance` is on the far side of
    /// the channel now.
    pub fn device(&self) -> Id<Device> {
        self.device
    }

    /// The latest published answer. One atomic load.
    pub fn current(&self) -> Arc<Held> {
        self.current.load_full()
    }

    /// Append to the log, off this thread.
    ///
    /// Returns immediately. The caller finds out through the next
    /// [`Desk::current`], which is the same way every other reader does — a
    /// writer is not privileged just because it was the one who wrote.
    pub fn say(&self, say: impl FnOnce(&mut Instance) + Send + 'static) {
        let _ = self.jobs.send(Job::Say(Box::new(say)));
    }

    /// A scan happened. These are the holdings now.
    pub fn holdings(&self, held: Holdings) {
        let _ = self.jobs.send(Job::Held(held));
    }

    /// Publish again without changing anything.
    pub fn advance(&self) {
        let _ = self.jobs.send(Job::Advance);
    }

    /// Block until everything queued so far has been published.
    ///
    /// For the two places that must wait: a test asserting on what a write
    /// did, and quitting, where the last rating has to reach the disk. Not for
    /// ordinary reads — a read that waits has put the fold back on the
    /// caller's thread by another route.
    pub fn settled(&self) {
        let (tx, rx) = unbounded();
        if self.jobs.send(Job::Settled(tx)).is_ok() {
            let _ = rx.recv();
        }
    }
}

impl Drop for Desk {
    /// Close the queue and join, so a rating queued a moment before quit is
    /// not lost to the process exiting.
    fn drop(&mut self) {
        // Dropping the last sender is what ends the thread's `for job in rx`,
        // and it drains first — so a rating queued a moment before quit still
        // reaches the disk.
        drop(std::mem::replace(&mut self.jobs, unbounded().0));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The one fold, and the only place it happens.
fn fold(log: &Instance, held: &Holdings, device: Id<Device>, now: i64) -> Held {
    Held {
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

    /// **A reader holds its answer while the next one is published.** This is
    /// the property the whole change is for: the draw loop used to have to
    /// unwind so the borrow would end before anything could rebuild.
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

    /// **Quitting does not lose what was queued.** A rating is appended and
    /// the program exits a moment later; the write is on another thread by
    /// then, so the only thing standing between it and the disk is `Drop`
    /// draining the queue before it joins.
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
