//! **The control thread, which owns the player** — #230, §4.4, §5.1, §6.
//!
//! ## Why this thread exists, and it is not a preference
//!
//! `PlayerHandle` is `!Send`: it owns the cpal stream, which is not `Send` on
//! every platform, and its own documentation says to create and use it from one
//! thread. [`super::Answers`] is `Send + Sync + 'static`, because a connection
//! worker calls it.
//!
//! **So the player cannot live behind `Answers`.** That is not an obstacle to
//! work around — it is §5.1's control thread arriving because the type system
//! asked for it. §4.4 already says this thread owns control state, must not run
//! queries or do file I/O, and must not block on clients; owning the player is
//! what that row means in practice.
//!
//! ## What a `playback` event is emitted for
//!
//! §7.8 is explicit that there is **never a steady position stream**: an event
//! goes out only when the state, the rate, or the position changes
//! discontinuously. Clients extrapolate locally from `pos_frames` and `at_ns`.
//!
//! That makes the trigger list a closed set rather than a heuristic, which is
//! the nice part: a discontinuity is either something this thread *did* — play,
//! pause, seek, load — or something the player *reported* (`Finished`,
//! `Loaded`, `DeviceLost`), or a stream generation change. Nothing has to guess
//! by watching the position for a jump.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use gmuse_proto::{Event, Topic};

use crate::audio::{PlayerConfig, PlayerEvent, PlayerHandle, Shared, State, null};
use crate::instruction::{Fader, Instruction, ScopeTo};
use gmuse_library::librarian::Desk;
use gmuse_library::model::entity::Recording;
use gmuse_library::model::id::Id;
use gmuse_library::model::rows::Listing;

/// What the connection workers can ask the player for.
pub enum Ask {
    /// A command from the text language, already parsed and already checked
    /// remotable.
    Do(Instruction),
    /// Play this recording.
    ///
    /// **By identity, never by path**, and the gate is deliberate:
    /// `TrackSource::plain` is `#[cfg(test)]` precisely so that "nothing plays
    /// that the library did not describe". The control thread resolves the
    /// recording through the librarian's projection — `Librarian::open` hands
    /// back the encoding this device holds and where it is — which is a snapshot
    /// read rather than file I/O, so §4.4 permits it here.
    ///
    /// Going the other way is what is forbidden: deriving a recording *from* a
    /// path means hashing the file's audio, which is a read this thread may not
    /// do. So a `playback` event names what was asked for, not what was found.
    Load { recording: Id<Recording> },
    /// A queue operation, with somewhere to put the answer.
    ///
    /// **A reply channel, because the state lives on this thread.** `Playing` is
    /// owned by the loop below and a connection worker cannot borrow it, so the
    /// worker asks and waits. That is the engine blocking on *itself*, bounded
    /// by one tick — not I1, which forbids blocking on a **view**.
    Queue {
        op: QueueOp,
        reply: Sender<Result<Answered, super::queue::Stale>>,
    },
}

/// What a client asked of the queue.
pub enum QueueOp {
    /// Replace it whole, for a view that holds a mirror (#233).
    Set {
        ids: Vec<gmuse_library::model::rows::CopyId>,
        base: u64,
    },
    Get,
    Enqueue {
        ids: Vec<gmuse_library::model::rows::CopyId>,
        base: Option<u64>,
    },
    Remove {
        index: usize,
        base: Option<u64>,
    },
    Clear {
        base: Option<u64>,
    },
    SetScope(gmuse_library::next::Scope),
}

/// The queue as it stands after an operation.
pub struct Answered {
    pub version: u64,
    /// The entries, for a read. Absent for a mutation, which answers with the
    /// version and lets the `queue` event carry the rest (§7.8).
    pub entries: Option<Vec<gmuse_library::model::rows::CopyId>>,
    /// **What plays next, for a read** — #233: scope, shuffle, repeat,
    /// repeat_current.
    ///
    /// On the read as well as the event, so a view that attaches mid-stream knows
    /// them without waiting for one to change. Absent for a mutation, for the same
    /// reason `entries` is.
    pub modes: Option<(gmuse_library::next::Scope, bool, bool, bool)>,
}

/// What the handshake needs to report, published by this thread.
///
/// Atomics rather than a channel, because `hello` may arrive on any connection
/// worker at any time and must not wait on the control thread to answer — which
/// is I1 pointed inward.
#[derive(Debug, Default)]
pub struct Facts {
    pub sample_rate: AtomicU32,
    pub stream_generation: AtomicU32,
}

/// A running control thread. Dropping it stops the player.
pub struct Control {
    asks: Sender<Ask>,
    facts: Arc<Facts>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Control {
    /// Open the player and start answering.
    ///
    /// Blocks until the player is open, so that `hello` never reports a
    /// placeholder rate: a client that connected during startup would otherwise
    /// be told 0 Hz and have no way to learn better.
    pub fn start(
        null_audio: bool,
        // **What opened the first stream, and what will open the next** — #241.
        // `preferred_config` matches `--device` by substring, so holding the whole
        // config is what lets a retry honour the intent rather than guess at it.
        cfg: PlayerConfig,
        desk: Arc<Desk>,
        // **What `rescan` reconciles.** Held here rather than asked for per
        // command: the roots are the engine's configuration and a command carries
        // no context (I9).
        roots: Vec<std::path::PathBuf>,
        engine: Arc<std::sync::OnceLock<Arc<super::Engine>>>,
    ) -> Result<Self, crate::audio::AudioError> {
        let (asks, rx) = unbounded::<Ask>();
        let (ready_tx, ready) = bounded::<Result<u32, crate::audio::AudioError>>(1);
        let facts = Arc::new(Facts::default());
        let stop = Arc::new(AtomicBool::new(false));

        let thread = {
            let facts = Arc::clone(&facts);
            let stopping = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("gmuse-engine-control".into())
                .spawn(move || {
                    // **Opened on this thread**, because the handle cannot cross one.
                    let open = |cfg: PlayerConfig| {
                        if null_audio {
                            // Wall speed, not `Fast`: a headless engine is still
                            // playing for somebody, and nothing downstream should be
                            // able to tell which backend it is.
                            PlayerHandle::headless(cfg, null::Pace::RealTime)
                        } else {
                            PlayerHandle::new(cfg)
                        }
                    };
                    let mut player = match open(cfg.clone()) {
                        Ok(player) => {
                            // **Both facts before `ready`, not merely before the loop.**
                            // `Control::start` returns the moment this send lands and a
                            // `hello` can arrive immediately after — so anything stored
                            // later is a placeholder somebody can read. It used to be
                            // stored just below and the window was microseconds; #241
                            // put a librarian fold between the two and the window became
                            // the fold, which `the_engine_plays_a_track_and_says_so`
                            // caught as a rate of 0.
                            facts
                                .sample_rate
                                .store(player.device_rate(), Ordering::Release);
                            facts
                                .stream_generation
                                .store(player.stream_generation(), Ordering::Release);
                            let _ = ready_tx.send(Ok(player.device_rate()));
                            player
                        }
                        // **No device at startup is fatal, and a device *lost* is
                        // not** — #241. The asymmetry is deliberate: `start` blocks
                        // until a player opens and the socket is not bound until it
                        // returns, so an engine waiting here would never answer a
                        // handshake and a view's auto-start (§4.2) would time out
                        // against silence. Failing with "no output device available" is
                        // the useful answer. Once a stream has existed, the socket is
                        // up and clients are connected — so waiting is the useful one.
                        Err(e) => {
                            let _ = ready_tx.send(Err(e));
                            return;
                        }
                    };

                    // **Everything that outlives a stream** (#241), so a cable coming
                    // out does not empty the queue.
                    let mut held = Held {
                        queue: super::queue::Playing::new(pool_of(&desk)),
                        playing: None,
                        pending: None,
                        announced: 0,
                        folded: desk.current(),
                        generation: player.stream_generation(),
                    };

                    loop {
                        // **Again each time round**, because a rebuilt device may run at
                        // another rate and the generation has moved.
                        facts
                            .sample_rate
                            .store(player.device_rate(), Ordering::Release);
                        facts
                            .stream_generation
                            .store(held.generation, Ordering::Release);

                        let wiring = Wiring {
                            asks: &rx,
                            facts: &facts,
                            desk: &desk,
                            roots: &roots,
                            engine: &engine,
                            stop: &stopping,
                        };
                        match run(&player, &mut held, wiring) {
                            Ran::Stopped => return,
                            Ran::DeviceLost => {}
                        }

                        // **Where it was, before the handle that knew goes.**
                        let was = player.position();
                        let resuming = held.playing.clone();
                        drop(player);

                        // **Wait for a device, honouring the intent that opened the
                        // first one** — #241. `preferred_config` matches `--device` by
                        // substring, so a named interface is waited for and nothing
                        // else is taken; with no flag it takes whatever is default now.
                        //
                        // A poll, and not for want of trying: after a `DeviceLost` no
                        // callback runs, so cpal has no `DeviceChanged` to give us.
                        // Enumeration is the only signal there is.
                        let Some(fresh) = wait_for_a_device(&open, &cfg, &stopping) else {
                            return;
                        };
                        player = fresh;
                        // **Monotonic, so a generation never repeats.** A fresh handle
                        // starts its own at zero; passing that through would make the
                        // number go backwards and a `When::Frame` uncheckable (#212).
                        held.generation = held.generation.saturating_add(1);

                        // **Loaded and positioned, and left paused** — #241's decision.
                        // Nothing starts audio nobody asked for: a cable knocked out at
                        // 3am must not start the music again when the device reappears.
                        // The same three commands `restore_session` uses, for the same
                        // reason.
                        if let Some(recording) = resuming
                            .as_deref()
                            .and_then(|r| r.parse::<uuid::Uuid>().ok())
                            .map(gmuse_library::model::id::Id::from_uuid)
                            && let Some(source) = source_for(&desk, recording)
                        {
                            player.load(source);
                            if was > Duration::ZERO {
                                player.seek(crate::audio::SeekTo::Absolute(was));
                            }
                            player.pause();
                        }
                    }
                })
                .map_err(|e| crate::audio::AudioError::Device(e.to_string()))?
        };

        match ready.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(_rate)) => Ok(Self {
                asks,
                facts,
                stop,
                thread: Some(thread),
            }),
            Ok(Err(e)) => Err(e),
            // The thread did not report in. Nothing to do but say so.
            Err(_) => Err(crate::audio::AudioError::Device(
                "the control thread did not open a player".into(),
            )),
        }
    }

    /// Ask the player to do something. Never blocks on the player.
    pub fn ask(&self, ask: Ask) -> bool {
        self.asks.send(ask).is_ok()
    }

    pub fn facts(&self) -> Arc<Facts> {
        Arc::clone(&self.facts)
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// What was last told to the views, so a repeat is not sent.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Told {
    state: State,
    generation: u32,
}

/// The loop: apply what is asked, and say what changed.
/// **What outlives a stream** — #241.
///
/// A cable coming out must not empty the queue. So everything the control thread
/// remembers between ticks lives here, owned by the thread body rather than by `run`,
/// and a rebuilt player is handed the same `Held` back.
struct Held {
    /// The queue, the modes and the materialised pool (#232).
    queue: super::queue::Playing,
    /// What the `playback` event names.
    playing: Option<String>,
    /// What was spliced on and has not started yet.
    ///
    /// `set_next` hands the decoder a track to join onto the end of the current one;
    /// nothing has played it yet, so `playing` must not name it. Promoted when the
    /// current track finishes, which is the moment the join happens.
    ///
    /// Without this the `playback` event kept naming the first recording after a
    /// gapless join — the engine had moved on and was telling every view it had not.
    pending: Option<String>,
    /// The queue version last published, so an unchanged one publishes nothing.
    announced: u64,
    /// The librarian's last published fold, to notice a new one by pointer.
    folded: std::sync::Arc<gmuse_library::librarian::Answer>,
    /// **The engine's own count of streams**, monotonic across rebuilds.
    ///
    /// Not the handle's: a rebuilt `PlayerHandle` has a fresh `Shared` whose generation
    /// starts at zero, so passing that through would make the number go *backwards*
    /// after a device came back. A generation that can repeat is one a `When::Frame`
    /// cannot be checked against, which is the whole point of #212.
    generation: u32,
}

/// Why the loop gave up the stream.
enum Ran {
    /// Asked to stop. Nothing is rebuilt.
    Stopped,
    /// The device went. The caller rebuilds and calls again (#241).
    DeviceLost,
}

/// **What outlives every stream, as opposed to every tick** — #241.
///
/// [`Held`] is the state a rebuild carries forward; this is the wiring it carries it
/// through. Bundled because a rebuild loop means `run` is called more than once, and
/// eight arguments at a call site say nothing about which of them could change between
/// calls. None of these can.
#[derive(Clone, Copy)]
struct Wiring<'a> {
    asks: &'a Receiver<Ask>,
    facts: &'a Arc<Facts>,
    desk: &'a Arc<Desk>,
    roots: &'a [std::path::PathBuf],
    engine: &'a Arc<std::sync::OnceLock<Arc<super::Engine>>>,
    stop: &'a Arc<AtomicBool>,
}

fn run(player: &PlayerHandle, held: &mut Held, w: Wiring<'_>) -> Ran {
    let Wiring {
        asks,
        facts,
        desk,
        roots,
        engine,
        stop,
    } = w;
    let shared = player.shared();
    let mut told: Option<Told> = None;
    let mut lost = false;

    while !stop.load(Ordering::Acquire) {
        // **A timeout, not a blocking receive.** §5.1: this thread must wake on
        // its own, because a `Finished` from the decoder and a stream
        // generation change are both discontinuities nobody sent it a command
        // about.
        let mut moved = match asks.recv_timeout(Duration::from_millis(50)) {
            Ok(Ask::Do(instruction)) => apply(
                player,
                instruction,
                &mut held.queue,
                desk,
                roots,
                &mut held.pending,
                &mut held.playing,
            ),
            Ok(Ask::Load { recording }) => {
                match source_for(desk, recording) {
                    Some(source) => {
                        held.playing = Some(recording.to_string());
                        player.load(source);
                        true
                    }
                    // Nothing this device holds. Not an error the player can
                    // act on, and the `playback` event that does not follow is
                    // the honest report.
                    None => false,
                }
            }
            Ok(Ask::Queue { op, reply }) => {
                let answered = match op {
                    QueueOp::Get => {
                        let (version, entries) = held.queue.get();
                        Ok(Answered {
                            version,
                            entries: Some(entries),
                            modes: Some(held.queue.modes()),
                        })
                    }
                    QueueOp::Enqueue { ids, base } => {
                        held.queue.enqueue(&ids, base).map(|version| Answered {
                            version,
                            entries: None,
                            modes: None,
                        })
                    }
                    QueueOp::Set { ids, base } => {
                        held.queue.set(&ids, base).map(|version| Answered {
                            version,
                            entries: None,
                            modes: None,
                        })
                    }
                    QueueOp::Remove { index, base } => {
                        held.queue.remove(index, base).map(|version| Answered {
                            version,
                            entries: None,
                            modes: None,
                        })
                    }
                    QueueOp::Clear { base } => held.queue.clear(base).map(|version| Answered {
                        version,
                        entries: None,
                        modes: None,
                    }),
                    QueueOp::SetScope(mode) => Ok(Answered {
                        version: held.queue.set_scope(mode),
                        entries: None,
                        modes: None,
                    }),
                };
                // A dropped receiver means the client gave up, which is not this
                // thread's problem.
                let _ = reply.send(answered);
                false
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => false,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        };

        // **A new fold means a new pool.** A track imported while something is
        // playing should be playable next, and the cheapest way to know is the
        // pointer the librarian swapped.
        let now = desk.current();
        if !Arc::ptr_eq(&held.folded, &now) {
            held.queue.refresh(pool_of(desk));
            held.folded = now;
        }

        // What the player has to say for itself. Draining rather than taking
        // one: a track change can arrive as `Finished` then `Loaded` in the
        // same tick, and only the last state matters.
        while let Ok(event) = player.events().try_recv() {
            match event {
                // **The engine answers this, and that is the whole of #232.**
                // The decoder asks while the current track is still playing and
                // needs a reply before it ends; a detached view cannot serve a
                // deadline, so nothing is asked of one.
                PlayerEvent::NeedNext => {
                    match held
                        .queue
                        .take_next()
                        .and_then(|(id, _source)| recording_of(desk, id).map(|rec| (id, rec)))
                    {
                        Some((id, recording)) => match source_for(desk, recording) {
                            Some(source) => {
                                held.pending = Some(recording.to_string());
                                held.queue.now_playing(Some(id));
                                player.set_next(Some(source));
                            }
                            // Named and not held: a row the library has and this
                            // device does not is an ordinary thing to reach.
                            None => player.set_next(None),
                        },
                        // End of pool. Letting playback stop is the answer, and
                        // it has to be *an* answer or the decoder waits.
                        None => player.set_next(None),
                    }
                }
                PlayerEvent::Finished => {
                    // The join happened: what was spliced on is what is playing.
                    if let Some(next) = held.pending.take() {
                        held.playing = Some(next);
                    }
                    moved = true;
                }
                PlayerEvent::Loaded { .. } => {
                    moved = true;
                }
                // **Remembered, not just announced** — #212. The render step has
                // stopped, so the position is frozen and every client needs to be
                // told to stop extrapolating it. There is no restart path yet
                // (#241), so this stays set: the stream really is gone.
                PlayerEvent::DeviceLost(_) => {
                    lost = true;
                    moved = true;
                }
                // Informational: the OS rerouted us and playback continues, so
                // it is not a transport discontinuity.
                PlayerEvent::DeviceChanged | PlayerEvent::Error(_) => {}
            }
        }

        let now = Told {
            state: player.state(),
            generation: shared.generation(),
        };
        facts
            .stream_generation
            .store(now.generation, Ordering::Release);

        // **Only on a discontinuity** — §7.8. A tick that published every 50 ms
        // would be a steady position stream, which the design forbids and which
        // §7.10's coalescing exists to avoid needing.
        // **The queue says only its version** — §7.8. A view re-reads with
        // `queue.get`; sending the entries would make every reorder carry the
        // whole list, and §7.10's coalescing already means a view that fell
        // behind sees one event rather than each.
        let version = held.queue.version();
        if version != held.announced {
            if let Some(engine) = engine.get() {
                // **The modes ride along** — #233. A list plus `shuffle` plus
                // `repeat` is the answer to "what plays next"; a view drawing the
                // list while owning the modes would draw a queue that does not
                // describe what will happen. They are small, they change on the
                // same events, and `set_scope`/`set_mode` already move the version
                // — so there is nothing to add but the fields.
                let (scope, shuffle, repeat, repeat_current) = held.queue.modes();
                engine.publish(&Event::new(
                    Topic::Queue,
                    serde_json::json!({
                        "version": version,
                        "scope": scope.label(),
                        "shuffle": shuffle,
                        "repeat": repeat,
                        "repeat_current": repeat_current,
                    }),
                ));
            }
            held.announced = version;
        }

        if moved || told != Some(now) {
            if let Some(engine) = engine.get() {
                engine.publish(&playback_event(
                    player,
                    &shared,
                    now,
                    held.playing.as_deref(),
                    lost,
                ));
            }
            told = Some(now);
        }

        // **After the event, so every client knows before the stream changes.**
        // A view should learn that nothing is sounding before it learns there is a
        // new stream, or it spends the rebuild extrapolating a dead one.
        if lost {
            return Ran::DeviceLost;
        }
    }
    Ran::Stopped
}

/// **Wait for a device to come back, honouring the config that opened the first** —
/// #241.
///
/// `None` means the engine was asked to stop while waiting, which is the one way out
/// other than succeeding.
///
/// **A poll, and not for want of trying.** After a `DeviceLost` no callback runs, so
/// cpal has no `DeviceChanged` to give us — enumeration is the only signal there is.
/// So this asks, waits, and asks again.
///
/// Backed off to a second, which is the right order for a person plugging a cable in:
/// fast enough that it feels immediate, slow enough that an engine with no device for
/// a week is not spinning a host enumeration a hundred times a second.
fn wait_for_a_device(
    open: &impl Fn(PlayerConfig) -> Result<PlayerHandle, crate::audio::AudioError>,
    cfg: &PlayerConfig,
    stop: &Arc<AtomicBool>,
) -> Option<PlayerHandle> {
    let mut said = false;
    while !stop.load(Ordering::Acquire) {
        match open(cfg.clone()) {
            Ok(player) => {
                if said {
                    eprintln!(
                        "gmuse engine: a device again at {} Hz",
                        player.device_rate()
                    );
                }
                return Some(player);
            }
            Err(e) => {
                // **Once, not every second.** A device that is gone for an hour would
                // otherwise write three and a half thousand identical lines.
                if !said {
                    eprintln!(
                        "gmuse engine: no output device ({e}); waiting{}",
                        match cfg.device.as_deref() {
                            Some(want) => format!(" for one matching {want:?}"),
                            None => String::new(),
                        }
                    );
                    said = true;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
    None
}

/// Every row the library holds, as the pool to pick from.
///
/// **A projection read, not a query** (§4.4). `Listing::rows` walks what the
/// librarian already folded, and `Shelf::from_members` groups it — the same call
/// the TUI makes for its own list, which is what keeps the two from disagreeing
/// about what "the library" contains.
fn pool_of(desk: &Arc<Desk>) -> gmuse_library::shelf::Shelf {
    let answer = desk.current();
    let rows: std::collections::HashSet<gmuse_library::model::rows::CopyId> =
        Listing::rows(&answer.library).into_iter().collect();
    gmuse_library::shelf::Shelf::from_members(&answer.library, rows, true)
}

/// The recording a row draws, so a pick can be resolved to a locator.
fn recording_of(desk: &Arc<Desk>, id: gmuse_library::model::rows::CopyId) -> Option<Id<Recording>> {
    let answer = desk.current();
    let rows: &dyn Listing = &answer.library;
    rows.recording_of(id)
}

/// Build a playable source from a recording, through the projection.
///
/// **Unity gain for now.** `source_for` in the UI applies replay gain from the
/// encoding's levels and the user's gain preference — and that preference is
/// control state the engine does not carry yet. Playing at unity is wrong by a
/// few dB and is not wrong about *what* plays; a silent guess at the preference
/// would be the worse error.
fn source_for(desk: &Arc<Desk>, recording: Id<Recording>) -> Option<crate::audio::TrackSource> {
    let answer = desk.current();
    let (_encoding, locator) = answer.library.open(recording, &[desk.device()], &[])?;
    Some(crate::audio::TrackSource {
        path: std::path::PathBuf::from(locator.to_string()),
        gain: 1.0,
    })
}

/// Apply one text-language command. Returns whether it was a discontinuity.
fn apply(
    player: &PlayerHandle,
    instruction: Instruction,
    queue: &mut super::queue::Playing,
    desk: &Arc<Desk>,
    // Where `Rescan` reconciles, which only the engine knows.
    roots: &[std::path::PathBuf],
    pending: &mut Option<String>,
    // **What the `playback` event names.** A load starts immediately, unlike a
    // splice, so this is set here rather than promoted on `Finished` — and
    // forgetting it is why the first version of `next` skipped the audio while
    // telling every view the old track was still playing.
    playing: &mut Option<String>,
) -> bool {
    // **Exhaustive, with no catch-all** — #213. Every `Instruction` is something
    // the engine can do, because that is the only way one can be constructed: the
    // boundary's translation from `Command` either produces one or refuses with a
    // reason. So a new variant does not compile until it is handled here, which is
    // the guarantee two source-scraping tests used to approximate.
    match instruction {
        Instruction::Toggle => {
            player.toggle();
            true
        }
        Instruction::Stop => {
            player.stop();
            true
        }
        Instruction::Seek(to) => {
            player.seek(to);
            true
        }
        // **Skip to the next track, which the engine answers** — #232 gave it the
        // queue, so `next` no longer needs a view to resolve it.
        //
        // Unlike `NeedNext` this is a *load*, not a splice: the current track is
        // being abandoned rather than played out, so there is nothing to join onto
        // and `pending` is not the right slot.
        Instruction::Next => {
            let picked = queue.take_next().map(|(id, _)| id);
            load_picked(player, queue, desk, pending, playing, picked)
        }
        // **`prev` is the pool's previous, and always was.** An earlier note here
        // refused it, on the grounds that going back is not the inverse of
        // `take_next` because the queue is consumed by being played from. True, and
        // not what `prev` means: `next::prev` walks the *pool* backwards and never
        // reads the queue, so a view's `prev` has always been "the previous row in
        // the pool's order".
        //
        // A play *history* is still a design question, and a different feature.
        Instruction::Prev => {
            let picked = queue.take_prev();
            load_picked(player, queue, desk, pending, playing, picked)
        }

        // **A volume change is not a transport discontinuity**, so §7.8 has nothing
        // to say about it and this returns `false` — it still acts, which before
        // #233 it did not.
        Instruction::Volume(fader) => {
            let to = match fader {
                Fader::To(v) => v,
                Fader::By(d) => player.volume() + d,
            };
            player.set_volume(to);
            false
        }

        // **A rating is about what is playing**, which is the one subject the engine
        // has without a view. The view's fallback — rate whatever is under the
        // cursor — is a view's, and an `Instruction` carries no cursor by I9.
        Instruction::Rate(stars) => {
            let Some(recording) = playing
                .as_deref()
                .and_then(|r| r.parse::<uuid::Uuid>().ok())
                .map(gmuse_library::model::id::Id::from_uuid)
            else {
                return false;
            };
            let now = gmuse_library::history::now_ms();
            if let Some(appender) = desk.appender() {
                appender.say(move |log| log.rate(now, recording, stars));
            }
            false
        }

        // **What plays next** — §6 assigns these to the control thread.
        Instruction::Mode(which, to) => {
            queue.set_mode(which, to);
            false
        }
        Instruction::Scope(to) => {
            let mode = match to {
                ScopeTo::Set(mode) => mode,
                ScopeTo::Cycle => queue.scope().cycle(),
            };
            queue.set_scope(mode);
            false
        }

        // **A scan is the librarian's job.** Routed rather than duplicated: `roots`
        // lives on the engine and the appender is the only thing that may start one
        // (#237).
        Instruction::Rescan => {
            if let Some(appender) = desk.appender() {
                appender.scan(roots.to_vec(), None);
            }
            false
        }
    }
}

/// Load whatever the queue picked, or do nothing if it picked nothing.
///
/// Shared by `Next` and `Prev`, which differ only in how the pick is made — and
/// having them differ in anything else is how the first version of `next` came to
/// tell every view the old track was still playing.
fn load_picked(
    player: &PlayerHandle,
    queue: &mut super::queue::Playing,
    desk: &Arc<Desk>,
    pending: &mut Option<String>,
    playing: &mut Option<String>,
    picked: Option<gmuse_library::model::rows::CopyId>,
) -> bool {
    let Some((id, recording)) = picked.and_then(|id| recording_of(desk, id).map(|r| (id, r)))
    else {
        // End of pool, or a row the library has and this device does not.
        return false;
    };
    let Some(source) = source_for(desk, recording) else {
        return false;
    };
    queue.now_playing(Some(id));
    *pending = None;
    *playing = Some(recording.to_string());
    player.load(source);
    true
}

/// §7.8's payload for the main player.
///
/// **No `deck` field.** §7.8 says the topic carries "transport state for the
/// main player *and* each deck"; this is the main player, and #224 settled that
/// the player is a deck plus a queue rather than a deck. A `deck` key here would
/// claim a deck exists.
fn playback_event(
    player: &PlayerHandle,
    shared: &Arc<Shared>,
    told: Told,
    recording: Option<&str>,
    // Whether the device has gone, so the position is not advancing (#212).
    lost: bool,
) -> Event {
    let sounding = shared.sounding();
    Event::new(
        Topic::Playback,
        serde_json::json!({
            "state": match told.state {
                State::Playing => "playing",
                State::Paused => "paused",
                State::Stopped => "stopped",
            },
            "recording": recording,
            "pos_frames": sounding.map(|s| s.frame),
            "stream_generation": told.generation,
            "sample_rate": player.device_rate(),
            "at_ns": sounding.map(|s| s.at_nanos),
            // **Zero when nothing is sounding** — #212, I13.
            //
            // A client extrapolates position locally between discontinuities
            // (§7.8), so it needs to know when *not* to. A lost device stops the
            // render step, which freezes `position_frames` — so the local
            // `PlayerHandle::position` is honest by construction and a remote one
            // was not: it took the frozen position, reset its own clock, and
            // advanced forever while nothing played.
            //
            // `rate` is the field that already means this. It was the constant 1.0
            // with a note that varispeed does not exist yet; a stalled stream is
            // rate 0, which is the same quantity honestly measured rather than a
            // fourth run state invented for the case.
            "rate": if lost { 0.0 } else { 1.0 },
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The retry honours the config that opened the first stream, and gives up only
    /// when asked to stop** — #241.
    ///
    /// This is where the policy lives, so this is what is worth testing: that it keeps
    /// asking, that it hands back the first device it gets, and that a stop reaches it
    /// while it is waiting. A poll rather than an event because after a `DeviceLost` no
    /// callback runs and cpal has no `DeviceChanged` to give — so "keeps asking" is the
    /// whole mechanism.
    #[test]
    fn waiting_for_a_device_keeps_asking_until_one_answers() {
        let tries = std::sync::atomic::AtomicUsize::new(0);
        let cfg = PlayerConfig {
            device: Some("an interface".into()),
            rate: None,
            resample: Default::default(),
            volume: 0.25,
        };
        let open = |asked: PlayerConfig| {
            // **The config is passed through unchanged**, which is the decision: a
            // named device is waited for and nothing else is taken.
            assert_eq!(asked.device.as_deref(), Some("an interface"));
            assert_eq!(
                asked.volume, 0.25,
                "and the fader position survives a rebuild"
            );
            let n = tries.fetch_add(1, Ordering::Relaxed);
            if n < 2 {
                Err(crate::audio::AudioError::Device("not yet".into()))
            } else {
                PlayerHandle::headless(asked, null::Pace::Fast)
            }
        };

        let stop = Arc::new(AtomicBool::new(false));
        let player = wait_for_a_device(&open, &cfg, &stop);
        assert!(player.is_some(), "it gave up while a device was coming");
        assert_eq!(
            tries.load(Ordering::Relaxed),
            3,
            "it asked twice more after being refused"
        );
    }

    /// **A stop reaches it while it is waiting**, or an engine with no device could not
    /// be shut down — #241.
    #[test]
    fn waiting_for_a_device_stops_when_asked() {
        let stop = Arc::new(AtomicBool::new(false));
        {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(50));
                stop.store(true, Ordering::Release);
            });
        }
        let cfg = PlayerConfig {
            device: None,
            rate: None,
            resample: Default::default(),
            volume: 1.0,
        };
        let never = |_: PlayerConfig| Err(crate::audio::AudioError::Device("gone".into()));
        let waited = std::time::Instant::now();
        assert!(
            wait_for_a_device(&never, &cfg, &stop).is_none(),
            "it produced a device from a closure that never yields one"
        );
        assert!(
            waited.elapsed() < Duration::from_secs(5),
            "it did not notice the stop for {:?}",
            waited.elapsed()
        );
    }

    /// **A lost device reaches the control thread as an event** — #241, #212.
    ///
    /// The trigger, at the level cpal uses: its error callback sets `Shared::device_lost`
    /// and the decoder turns that into the one event the loop acts on. Tested here
    /// because the rebuild itself lives inside a thread that owns its player, and
    /// reaching into that from a test would mean a door nothing else uses.
    #[test]
    fn a_lost_device_surfaces_as_one_event() {
        let player = PlayerHandle::headless(
            PlayerConfig {
                device: None,
                rate: None,
                resample: Default::default(),
                volume: 1.0,
            },
            null::Pace::Fast,
        )
        .expect("a headless player");

        player.shared().device_lost.store(true, Ordering::Release);

        let mut heard = 0;
        let until = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < until {
            if let Ok(PlayerEvent::DeviceLost(_)) =
                player.events().recv_timeout(Duration::from_millis(50))
            {
                heard += 1;
                break;
            }
        }
        assert_eq!(heard, 1, "the loss never reached the control thread");

        // **Once, not once per tick.** The decoder swaps the flag rather than reading
        // it, so a single failure is a single event — which is what lets the loop treat
        // it as a discontinuity.
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            !matches!(player.events().try_recv(), Ok(PlayerEvent::DeviceLost(_))),
            "the loss was reported twice"
        );
    }
}
