//! **The thread that owns the librarian and answers the engine** — #257.
//!
//! The audio engine holds three channels instead of an `Arc<Desk>`: one to ask the programme,
//! one to say what it wants written, and a counter that moves when the library does. This is
//! the other end of all three.
//!
//! ## Why a relay and not a shared `Desk`
//!
//! The engine may not name a `Desk`, because naming one is naming the log, the projection, the
//! fold and every fact type behind it — which is the whole of what #257 removes. What it needs
//! is smaller than a librarian by a long way, and saying so in channels makes the difference
//! visible: two message types and a `u64`.
//!
//! **The cost is one thread and one hop.** A programme ask crosses to here, then to the
//! librarian thread, and the reply comes back the same way. Both hops are unix-pipe-free
//! in-process channel sends measured in microseconds, against an ask the engine never waits
//! on anyway (#259) — so the hop buys the boundary for nothing that can be heard.
//!
//! ## Why the wants are fire-and-forget
//!
//! A rating and a rescan have no answer the engine displays: a landed rating changes nothing
//! it draws, and a scan reports itself through the `library` topic like any other change. So
//! they are a plain `Sender` rather than the ask-and-poll pipeline the programme needs.

use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use gmuse_engine::engine::librarian::{Librarian, Wanted};
use gmuse_library::librarian::Desk;
use gmuse_programme::{Ask, Reply};

/// The engine's handle, and the thread that serves it.
///
/// The handle is returned; the thread runs until both channels close, which happens when the
/// engine drops its handle.
pub struct Relay {
    pub librarian: Librarian,
    /// The counter the engine watches. **Bumped by the announcer**, which is already watching
    /// the desk for the `library` topic — one watcher, not two.
    pub moved: Arc<AtomicU64>,
}

impl Relay {
    /// Start serving `desk` to an engine.
    pub fn start(desk: Arc<Desk>) -> Relay {
        let (ask_tx, ask_rx) =
            crossbeam_channel::unbounded::<(Ask, crossbeam_channel::Sender<Reply>)>();
        let (want_tx, want_rx) = crossbeam_channel::unbounded::<Wanted>();
        let moved = Arc::new(AtomicU64::new(0));

        // **Not joined, and not for want of tidiness.** The first version held the join handle
        // and joined on `Drop`, which deadlocked shutdown: the engine holds a clone of the
        // senders, so dropping this one leaves the channels open and the loop below waiting
        // for a message that will never come. `a_graceful_stop_removes_its_socket` caught it.
        //
        // There is nothing to join *for*. The thread owns its own `Arc<Desk>`, so it cannot
        // outlive what it borrows — it has nothing borrowed — and it ends on its own when the
        // last handle goes.
        let _thread = std::thread::Builder::new()
            .name("gmuse-relay".into())
            .spawn(move || {
                loop {
                    crossbeam_channel::select! {
                        recv(ask_rx) -> got => match got {
                            // **Blocking here is right and nowhere else.** This thread exists
                            // to wait; the engine's control thread never does (#259), and the
                            // librarian answers in its own time.
                            Ok((ask, reply)) => {
                                if let Some(answer) = desk.programme(ask) {
                                    let _ = reply.send(answer);
                                }
                            }
                            Err(_) => break,
                        },
                        recv(want_rx) -> got => match got {
                            Ok(wanted) => apply(&desk, wanted),
                            Err(_) => break,
                        },
                    }
                }
            })
            .expect("the relay thread starts");

        Relay {
            librarian: Librarian::new(ask_tx, want_tx, Arc::clone(&moved)),
            moved,
        }
    }
}

/// Do one thing the engine asked for.
///
/// **Exhaustive with no catch-all**, so a new `Wanted` does not compile until somebody has
/// decided what it does — which is the same guarantee `Instruction`'s match gives one layer
/// down.
fn apply(desk: &Arc<Desk>, wanted: Wanted) {
    let Some(appender) = desk.appender() else {
        // A reading desk cannot write, and an engine's is opened appending — so this is a
        // construction error rather than a state to handle. Silent because the audio path has
        // already moved on and there is nobody left to tell.
        return;
    };
    match wanted {
        Wanted::Rate { recording, stars } => {
            // **Stamped here**, because whoever appends decides when. The engine used to read
            // `history::now_ms` for this, which put a wall-clock read in the audio crate for a
            // fact it had no business writing.
            let now = gmuse_library::history::now_ms();
            appender.say(move |log| log.rate(now, recording, stars));
        }
        Wanted::Rescan { roots } => appender.scan(roots, None),
        Wanted::Played {
            recording,
            encoding,
        } => {
            // **`play_maybe`, which is what the TUI called**, so the fact and its
            // supersession rules are unchanged — only who decides has moved. Stamped here for
            // the same reason a rating is.
            let now = gmuse_library::history::now_ms();
            appender.say(move |log| log.play_maybe(now, recording, Some(encoding)));
        }
    }
}
