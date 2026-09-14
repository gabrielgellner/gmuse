//! **The engine's handle to the librarian, and what it asks of it** — #257.
//!
//! The last two things the audio crate did to the log directly: a rating, and starting a
//! rescan. Both went through `Desk::appender`, which is the engine holding the library's
//! single appender — the thing §4.4 gives to the librarian and the thing this issue is named
//! after.
//!
//! ## Why the engine's own type and not a shared one
//!
//! `gmuse-programme` is the vocabulary *both* actors need, because both read and write a
//! programme. This is one-way: the engine says what it wants, the wiring does it, and nothing
//! comes back. A one-way vocabulary needs no agreement — the sender defines it and the
//! receiver matches on it, which is what `main` already does for every other engine type.
//!
//! So there is no third crate here, and that is the test of whether the second one was
//! earned: a contract crate is for a vocabulary two peers must *share*, not for every type
//! that crosses between them.
//!
//! ## Why the engine asks at all
//!
//! A rating is about **what is playing**, which is the one subject the engine has without a
//! view: §I9 says an `Instruction` carries no cursor, so "rate what is under the cursor" is a
//! view's business and "rate what is sounding" is the engine's. It knows the subject and may
//! not do the write, so it names the subject and asks.
//!
//! A rescan is here for a different reason: `roots` lives on the engine and only the appender
//! may start one (#237). The engine is not the appender any more, so it forwards.

/// One thing the engine wants written or started.
///
/// **Fire and forget.** Nothing here has an answer, which is why it is a plain `Sender` and
/// not the ask-and-poll pipeline the programme needs: a rating that has landed changes
/// nothing the engine displays, and a rescan reports itself through the `library` topic like
/// any other change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wanted {
    /// Stars for the recording that is sounding. **`None` clears the rating**, which is
    /// distinct from a low one and is what `!rating?` finds.
    ///
    /// **The engine does not stamp it.** `history::now_ms` was called here, which put a
    /// wall-clock read in the audio crate for a fact it had no business writing; whoever
    /// appends decides when, because a stamp is part of the write.
    Rate {
        recording: gmuse_ids::id::Id<gmuse_ids::entity::Recording>,
        stars: Option<u8>,
    },
    /// Reconcile these roots against the disk.
    Rescan { roots: Vec<std::path::PathBuf> },
}

/// **How the engine reaches the librarian: three channels and no `Desk`.**
///
/// It held an `Arc<Desk>` — the librarian itself, with the log, the projection and the
/// appender behind it. That is what made `gmuse-engine` depend on `gmuse-library`, and with it
/// every read path, every fact type and the fold.
///
/// What it actually needed was three much smaller things. Whoever constructs this owns the
/// librarian; the engine cannot name one.
#[derive(Clone)]
pub struct Librarian {
    programme: crossbeam_channel::Sender<(
        gmuse_programme::Ask,
        crossbeam_channel::Sender<gmuse_programme::Reply>,
    )>,
    wants: crossbeam_channel::Sender<Wanted>,
    /// **Bumped when a new fold is published**, so the engine notices by reading a number
    /// rather than comparing `Arc` pointers into the library's own types.
    moved: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl Librarian {
    /// Wire one up. **Called by whoever owns the librarian**, which is not this crate.
    pub fn new(
        programme: crossbeam_channel::Sender<(
            gmuse_programme::Ask,
            crossbeam_channel::Sender<gmuse_programme::Reply>,
        )>,
        wants: crossbeam_channel::Sender<Wanted>,
        moved: std::sync::Arc<std::sync::atomic::AtomicU64>,
    ) -> Self {
        Self {
            programme,
            wants,
            moved,
        }
    }

    /// **Ask, without waiting for the answer.** The receiver is polled on a later tick.
    ///
    /// `None` means the librarian has gone, which for a running engine it has not — and a
    /// caller that gets one has no programme to play and should stop rather than guess.
    pub fn programme_later(
        &self,
        ask: gmuse_programme::Ask,
    ) -> Option<crossbeam_channel::Receiver<gmuse_programme::Reply>> {
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.programme.send((ask, tx)).ok()?;
        Some(rx)
    }

    /// **Say what you want written.** Fire and forget; nothing here has an answer.
    pub fn want(&self, wanted: Wanted) {
        // A closed channel means the librarian is going, which is not a failure the audio
        // path can act on.
        let _ = self.wants.send(wanted);
    }

    /// A counter that moves when the library does. **Compared, never interpreted.**
    pub fn moved(&self) -> u64 {
        self.moved.load(std::sync::atomic::Ordering::Acquire)
    }
}
