//! **The engine's librarian, and what it tells the views** — #222.
//!
//! §4.4: the librarian owns the fact log and the projection, talks to the
//! control thread and the disk, and must never run on the control thread (I16)
//! or be bypassed for a write (I15).
//!
//! `librarian::Desk` is that thread and already exists — #206 built it, the TUI
//! has used it since. What is here is the part that is the engine's: holding
//! one, routing §8.4's named operations to it, and turning each thing it
//! publishes into the one `library` event a view needs.
//!
//! ## Why an announcer rather than a callback
//!
//! The `Desk` publishes by atomic swap and hands out `Arc<Answer>`; it has no
//! subscription mechanism, deliberately — a reader takes a fresh answer when it
//! wants one, and nothing on the librarian thread waits for anybody. Adding a
//! callback would put a view's reachability on the appending path, which is I1
//! read from the other end.
//!
//! So a thread watches. It is cheap — an atomic load and a pointer comparison —
//! and it is the only shape that keeps the librarian unaware of whether anyone
//! is listening.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use gmuse_library::librarian::{Answer, Desk};

use gmuse_engine::engine::Engine;

/// Watches the desk and turns each publish into a `library` event.
pub struct Announcer {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// The engine's log position — **a scalar, because the engine is the single
    /// appender** (§8.7).
    ///
    /// Deliberately not `projection::Position`, which is a per-device map of
    /// stamps: that is a vector clock, and §8.7 says in as many words that sync
    /// identity is not what goes on this wire.
    version: Arc<AtomicU64>,
}

impl Announcer {
    /// Start watching. Every answer after the current one becomes an event.
    ///
    /// **From the current one, not from the first.** The answer standing when
    /// the engine starts is not news — a client that connects later asks for
    /// what it needs, and announcing the startup fold would tell every view to
    /// re-query the moment it arrived.
    pub fn start(desk: Arc<Desk>, engine: std::sync::Weak<Engine>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let version = Arc::new(AtomicU64::new(0));

        let stopping = Arc::clone(&stop);
        let counter = Arc::clone(&version);
        let thread = std::thread::Builder::new()
            .name("gmuse-engine-announcer".into())
            .spawn(move || {
                let mut last: Arc<Answer> = desk.current();
                while !stopping.load(Ordering::Acquire) {
                    let now = desk.current();
                    if !Arc::ptr_eq(&last, &now) {
                        let version = counter.fetch_add(1, Ordering::Relaxed) + 1;
                        if let Some(event) = super::change::change_for(&now.changed, version)
                            // **`None` means the engine is going**, which is not a
                            // change worth announcing to nobody — see `Engine`'s
                            // `Drop` on why this is a `Weak`.
                            && let Some(engine) = engine.upgrade()
                        {
                            engine.publish(&event);
                        }
                        last = now;
                    }
                    // **Polled, and the interval is a display latency not a
                    // correctness one.** Events coalesce, so a slower poll
                    // sends fewer and larger events rather than wrong ones; a
                    // faster one costs an atomic load. Twenty-five milliseconds
                    // is under a frame at any refresh rate a person notices.
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
            })
            .ok();

        Self {
            stop,
            thread,
            version,
        }
    }

    /// The engine's log position, as last announced.
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Relaxed)
    }
}

impl Drop for Announcer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
