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
use crate::cmd::Command;
use crate::librarian::Desk;
use crate::model::entity::Recording;
use crate::model::id::Id;
use gmuse_library::model::rows::Listing;

/// What the connection workers can ask the player for.
pub enum Ask {
    /// A command from the text language, already parsed and already checked
    /// remotable.
    Apply(Command),
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
    SetAaaMode(gmuse_library::next::AaaMode),
}

/// The queue as it stands after an operation.
pub struct Answered {
    pub version: u64,
    /// The entries, for a read. Absent for a mutation, which answers with the
    /// version and lets the `queue` event carry the rest (§7.8).
    pub entries: Option<Vec<gmuse_library::model::rows::CopyId>>,
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
        desk: Arc<Desk>,
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
                    // **Opened on this thread**, because the handle cannot cross
                    // one.
                    let cfg = PlayerConfig {
                        device: None,
                        rate: None,
                        resample: Default::default(),
                        volume: 1.0,
                    };
                    let player = if null_audio {
                        // Wall speed, not `Fast`: a headless engine is still
                        // playing for somebody, and nothing downstream should
                        // be able to tell which backend it is.
                        PlayerHandle::headless(cfg, null::Pace::RealTime)
                    } else {
                        PlayerHandle::new(cfg)
                    };
                    let player = match player {
                        Ok(player) => {
                            let _ = ready_tx.send(Ok(player.device_rate()));
                            player
                        }
                        Err(e) => {
                            let _ = ready_tx.send(Err(e));
                            return;
                        }
                    };

                    // **Both facts before the loop.** The rate was stored here
                    // and the generation only inside the tick, so a `hello`
                    // arriving before the first tick was answered with
                    // generation 0 — a placeholder again, by a different route.
                    facts
                        .sample_rate
                        .store(player.device_rate(), Ordering::Release);
                    facts
                        .stream_generation
                        .store(player.stream_generation(), Ordering::Release);
                    run(&player, &rx, &facts, &desk, &engine, &stopping);
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
fn run(
    player: &PlayerHandle,
    asks: &Receiver<Ask>,
    facts: &Arc<Facts>,
    desk: &Arc<Desk>,
    engine: &Arc<std::sync::OnceLock<Arc<super::Engine>>>,
    stop: &Arc<AtomicBool>,
) {
    let shared = player.shared();
    let mut told: Option<Told> = None;
    let mut playing: Option<String> = None;

    // **The pool, materialised** — #232. §4.4 forbids this thread from running
    // a query, so it holds one instead and rebuilds it when the librarian
    // publishes a change. Answering `NeedNext` is then a lookup.
    let mut queue = super::queue::Playing::new(pool_of(desk));
    // **What was spliced on but has not started yet.**
    //
    // `set_next` hands the decoder a track to join onto the end of the current
    // one; nothing has played it yet, so `playing` must not name it. Promoted
    // when the current track finishes, which is the moment the join happens.
    //
    // Without this the `playback` event kept naming the first recording after a
    // gapless join — the engine had moved on and was telling every view it had
    // not.
    let mut pending: Option<String> = None;
    let mut announced = queue.version();
    let mut folded = desk.current();

    while !stop.load(Ordering::Acquire) {
        // **A timeout, not a blocking receive.** §5.1: this thread must wake on
        // its own, because a `Finished` from the decoder and a stream
        // generation change are both discontinuities nobody sent it a command
        // about.
        let mut moved = match asks.recv_timeout(Duration::from_millis(50)) {
            Ok(Ask::Apply(command)) => apply(player, &command),
            Ok(Ask::Load { recording }) => {
                match source_for(desk, recording) {
                    Some(source) => {
                        playing = Some(recording.to_string());
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
                        let (version, entries) = queue.get();
                        Ok(Answered {
                            version,
                            entries: Some(entries),
                        })
                    }
                    QueueOp::Enqueue { ids, base } => {
                        queue.enqueue(&ids, base).map(|version| Answered {
                            version,
                            entries: None,
                        })
                    }
                    QueueOp::Remove { index, base } => {
                        queue.remove(index, base).map(|version| Answered {
                            version,
                            entries: None,
                        })
                    }
                    QueueOp::Clear { base } => queue.clear(base).map(|version| Answered {
                        version,
                        entries: None,
                    }),
                    QueueOp::SetAaaMode(mode) => Ok(Answered {
                        version: queue.set_aaa_mode(mode),
                        entries: None,
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
        if !Arc::ptr_eq(&folded, &now) {
            queue.refresh(pool_of(desk));
            folded = now;
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
                    match queue
                        .take_next()
                        .and_then(|(id, _source)| recording_of(desk, id).map(|rec| (id, rec)))
                    {
                        Some((id, recording)) => match source_for(desk, recording) {
                            Some(source) => {
                                pending = Some(recording.to_string());
                                queue.now_playing(Some(id));
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
                    if let Some(next) = pending.take() {
                        playing = Some(next);
                    }
                    moved = true;
                }
                PlayerEvent::Loaded { .. } | PlayerEvent::DeviceLost(_) => {
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
        let version = queue.version();
        if version != announced {
            if let Some(engine) = engine.get() {
                engine.publish(&Event::new(
                    Topic::Queue,
                    serde_json::json!({ "version": version }),
                ));
            }
            announced = version;
        }

        if moved || told != Some(now) {
            if let Some(engine) = engine.get() {
                engine.publish(&playback_event(player, &shared, now, playing.as_deref()));
            }
            told = Some(now);
        }
    }
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
fn apply(player: &PlayerHandle, command: &Command) -> bool {
    match command {
        Command::Play => {
            player.play();
            true
        }
        Command::Pause => {
            player.toggle();
            true
        }
        Command::Stop => {
            player.stop();
            true
        }
        Command::Seek(spec) => {
            if let Some(to) = seek_of(spec) {
                player.seek(to);
                return true;
            }
            false
        }
        // **Not a transport discontinuity.** A volume change does not move the
        // playhead or the run state, so §7.8 has nothing to say about it — it
        // belongs to the `params` topic, which has no producer yet.
        Command::Vol(_) => false,
        // Everything else needs a queue, which the engine does not have.
        _ => false,
    }
}

/// Turn the text language's seek into the audio path's.
fn seek_of(spec: &crate::cmd::SeekSpec) -> Option<crate::audio::SeekTo> {
    use crate::audio::SeekTo;
    match spec {
        crate::cmd::SeekSpec::Absolute(secs) => {
            Some(SeekTo::Absolute(Duration::from_secs_f64(secs.max(0.0))))
        }
        crate::cmd::SeekSpec::Relative(delta) => Some(SeekTo::Relative(*delta)),
    }
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
            // Varispeed is a rate change and there is no varispeed yet, so this
            // is the honest constant rather than an absent field a client would
            // have to default.
            "rate": 1.0,
        }),
    )
}
