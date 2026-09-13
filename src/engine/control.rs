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
                    run(&player, &rx, &facts, &desk, &roots, &engine, &stopping);
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
    roots: &[std::path::PathBuf],
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
            Ok(Ask::Apply(command)) => apply(
                player,
                &command,
                &mut queue,
                desk,
                roots,
                &mut pending,
                &mut playing,
            ),
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
                            modes: Some(queue.modes()),
                        })
                    }
                    QueueOp::Enqueue { ids, base } => {
                        queue.enqueue(&ids, base).map(|version| Answered {
                            version,
                            entries: None,
                            modes: None,
                        })
                    }
                    QueueOp::Set { ids, base } => queue.set(&ids, base).map(|version| Answered {
                        version,
                        entries: None,
                        modes: None,
                    }),
                    QueueOp::Remove { index, base } => {
                        queue.remove(index, base).map(|version| Answered {
                            version,
                            entries: None,
                            modes: None,
                        })
                    }
                    QueueOp::Clear { base } => queue.clear(base).map(|version| Answered {
                        version,
                        entries: None,
                        modes: None,
                    }),
                    QueueOp::SetScope(mode) => Ok(Answered {
                        version: queue.set_scope(mode),
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
                // **The modes ride along** — #233. A list plus `shuffle` plus
                // `repeat` is the answer to "what plays next"; a view drawing the
                // list while owning the modes would draw a queue that does not
                // describe what will happen. They are small, they change on the
                // same events, and `set_scope`/`set_mode` already move the version
                // — so there is nothing to add but the fields.
                let (scope, shuffle, repeat, repeat_current) = queue.modes();
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
fn apply(
    player: &PlayerHandle,
    command: &Command,
    queue: &mut super::queue::Playing,
    desk: &Arc<Desk>,
    // Where `rescan` reconciles, which only the engine knows.
    roots: &[std::path::PathBuf],
    pending: &mut Option<String>,
    // **What the `playback` event names.** A load starts immediately, unlike a
    // splice, so this is set here rather than promoted on `Finished` — and
    // forgetting it is why the first version of `next` skipped the audio while
    // telling every view the old track was still playing.
    playing: &mut Option<String>,
) -> bool {
    match command {
        // **No `Command::Play` arm, and that is not an omission.** `play` starts
        // the *selected* track, so it needs a cursor — `remote::is_remotable`
        // refuses it and `play_is_refused_and_says_what_to_send_instead` records
        // the reason. This arm existed and was unreachable, which the two drift
        // tests below now make impossible in either direction.
        //
        // Resuming is `pause`, which toggles. #225 is the argument for the
        // protocol carrying three distinct verbs so a sender does not have to
        // resolve one against the run state.
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
        // **Skip to the next track, which the engine can now answer** — #232
        // gave it the queue, so `next` no longer needs a view to resolve it.
        //
        // Unlike `NeedNext` this is a *load*, not a splice: the current track is
        // being abandoned rather than played out, so there is nothing to join
        // onto and `pending` is not the right slot.
        Command::Next => match queue
            .take_next()
            .and_then(|(id, _)| recording_of(desk, id).map(|rec| (id, rec)))
        {
            Some((id, recording)) => match source_for(desk, recording) {
                Some(source) => {
                    queue.now_playing(Some(id));
                    *pending = None;
                    *playing = Some(recording.to_string());
                    player.load(source);
                    true
                }
                None => false,
            },
            None => false,
        },
        // **`prev` is the pool's previous, and always was** — #233.
        //
        // An earlier note here refused this, on the grounds that going back is
        // not the inverse of `take_next` because the queue is consumed by being
        // played from. That is true and it is not what `prev` means: `next::prev`
        // walks the *pool* backwards and never reads the queue, so a view's
        // `prev` has always been "the previous row in the pool's order". The
        // engine doing the same thing is faithful rather than inventive.
        //
        // A play *history* is still a design question, and a different feature.
        Command::Prev => match queue
            .take_prev()
            .and_then(|id| recording_of(desk, id).map(|rec| (id, rec)))
        {
            Some((id, recording)) => match source_for(desk, recording) {
                Some(source) => {
                    queue.now_playing(Some(id));
                    *pending = None;
                    *playing = Some(recording.to_string());
                    player.load(source);
                    true
                }
                None => false,
            },
            None => false,
        },

        // **A volume change is not a transport discontinuity**, so §7.8 has
        // nothing to say about it and this returns `false` — but it *acts*, which
        // it did not before. `is_remotable` admitted `vol` and this arm dropped
        // it, so `gmuse ctl vol 50` answered `{"accepted": true}` and did nothing.
        //
        // The same arithmetic the view does: absolute sets, relative adds to what
        // the fader is at now.
        Command::Vol(spec) => {
            let to = match spec {
                crate::cmd::VolSpec::Absolute(v) => *v,
                crate::cmd::VolSpec::Relative(d) => player.volume() + d,
            };
            player.set_volume(to);
            false
        }

        // **A rating is about what is playing**, which is the one subject the
        // engine has without a view. The view's fallback — rate whatever is under
        // the cursor — is a view's, and `Ask::Apply` has no cursor by I9.
        Command::Rate(spec) => {
            let Some(recording) = playing
                .as_deref()
                .and_then(|r| r.parse::<uuid::Uuid>().ok())
                .map(gmuse_library::model::id::Id::from_uuid)
            else {
                return false;
            };
            let stars = match spec {
                crate::cmd::RateSpec::Set(n) => Some(*n),
                _ => None,
            };
            let now = gmuse_library::history::now_ms();
            if let Some(appender) = desk.appender() {
                appender.say(move |log| log.rate(now, recording, stars));
            }
            false
        }

        // **What plays next, set through the same keys the `:` line uses** —
        // §7.12, #233. `SettingKey::not_the_engines` is what decides which keys
        // reach here, and `is_remotable` refuses the rest before this.
        Command::Set { key, value } => {
            match value {
                crate::cmd::SettingValue::Scope(mode) => {
                    queue.set_scope(*mode);
                }
                crate::cmd::SettingValue::Bool(on) => {
                    queue.set_mode(*key, *on);
                }
                // A setting the engine owns whose value is not a bool or a scope
                // is one `not_the_engines` refuses, so there is nothing to do.
                _ => {}
            }
            false
        }
        Command::Toggle(key) => {
            if *key == crate::cmd::SettingKey::Scope {
                queue.set_scope(queue.scope().cycle());
            } else {
                let on = !queue.mode(*key);
                queue.set_mode(*key, on);
            }
            false
        }

        // **A scan is the librarian's job, and `library.scan` is its front door.**
        // Routed rather than duplicated: `roots` lives on the engine and the
        // appender is the only thing that may start one (#237).
        Command::Rescan => {
            if let Some(appender) = desk.appender() {
                appender.scan(roots.to_vec(), None);
            }
            false
        }

        // **Everything else needs a view** — a cursor, a selection, a pane — and
        // `remote::is_remotable` refuses it at the ingress. Reaching here means
        // the gate and this match have drifted, which is what
        // `every_remotable_command_is_one_the_engine_carries_out` exists to
        // prevent: the previous drift made `set`, `prev`, `vol` and `rate` all
        // answer `{"accepted": true}` and do nothing.
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

#[cfg(test)]
mod tests {
    /// **Every command the socket admits is one this thread carries out** — #233.
    ///
    /// `remote::is_remotable` is the gate and `apply` is the doing, and they were
    /// two independent lists. They drifted in both directions: `set`, `prev`,
    /// `vol` and `rate` were admitted and silently ignored — `{"accepted": true}`
    /// and nothing happened — while `play` was handled here and refused at the
    /// gate.
    ///
    /// A source comparison rather than a call, because `apply` needs a
    /// `PlayerHandle` and that needs a device. What it proves is the thing that
    /// actually went wrong: a name on one list and not the other.
    #[test]
    fn every_remotable_command_is_one_the_engine_carries_out() {
        let gate = body_of(
            "src/remote.rs",
            "pub fn is_remotable(cmd: &Command) -> bool {",
        );
        let doing = body_of("src/engine/control.rs", "fn apply(");

        // Every `Command::X` the gate admits. `Set` and `Toggle` are named there
        // too, and their arm delegates to `SettingKey::not_the_engines`.
        let admitted: Vec<String> = names_in(&gate);
        assert!(admitted.len() >= 9, "the gate was not parsed: {admitted:?}");
        let handled = names_in(&doing);
        let missing: Vec<&String> = admitted.iter().filter(|c| !handled.contains(c)).collect();
        assert!(
            missing.is_empty(),
            "the socket admits these and `apply` does not name them, so they would \
             be accepted and silently ignored: {missing:?}"
        );
    }

    /// The other direction: nothing is handled here that the gate refuses.
    ///
    /// Not a symmetry for its own sake — a command `apply` acts on and the gate
    /// rejects is a feature nobody can reach, which is how `play` sat unreachable
    /// over `--remote` while working in process.
    #[test]
    fn nothing_the_engine_carries_out_is_refused_at_the_gate() {
        let gate = names_in(&body_of(
            "src/remote.rs",
            "pub fn is_remotable(cmd: &Command) -> bool {",
        ));
        let doing = names_in(&body_of("src/engine/control.rs", "fn apply("));
        let stranded: Vec<&String> = doing.iter().filter(|c| !gate.contains(c)).collect();
        assert!(
            stranded.is_empty(),
            "`apply` acts on these and the socket refuses them, so nothing can \
             reach them: {stranded:?}"
        );
    }

    /// The `Command::X` names mentioned in a chunk of source, deduplicated.
    ///
    /// **Comments stripped first.** Both functions explain in prose which commands
    /// they leave out and why, and a scrape that counted those would report a name
    /// as handled because somebody wrote a sentence about not handling it. Learned
    /// the hard way on a different scrape in this repo, which counted string
    /// literals inside a forbidden-list.
    fn names_in(text: &str) -> Vec<String> {
        let code: String = text
            .lines()
            .map(|line| match line.find("//") {
                Some(at) => &line[..at],
                None => line,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let text = code.as_str();
        let mut out = Vec::new();
        for (at, _) in text.match_indices("Command::") {
            let name: String = text[at + "Command::".len()..]
                .chars()
                .take_while(|c| c.is_alphanumeric())
                .collect();
            if !name.is_empty() && !out.contains(&name) {
                out.push(name);
            }
        }
        out.sort();
        out
    }

    /// The body of a function, read from source by its signature line.
    ///
    /// Paths are relative to the crate root, which is where `cargo test` runs.
    fn body_of(file: &str, signature: &str) -> String {
        let text = std::fs::read_to_string(file).unwrap_or_else(|e| panic!("{file}: {e}"));
        let start = text
            .find(signature)
            .unwrap_or_else(|| panic!("{file}: no {signature}"));
        let rest = &text[start..];
        let end = rest.find("\n}\n").expect("the end of the function");
        rest[..end].to_string()
    }
}
