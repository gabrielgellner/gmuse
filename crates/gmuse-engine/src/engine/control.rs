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

use super::librarian::{Librarian, Wanted};
use crate::audio::{PlayerConfig, PlayerEvent, PlayerHandle, Shared, State, null};
use crate::instruction::{Fader, Instruction, ScopeTo};
use gmuse_ids::entity::Recording;
use gmuse_ids::id::Id;

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
    Load {
        recording: Id<Recording>,
        /// The rows of the view the play came from, resolved by the socket handler; the
        /// programme is built inside them. `None` is the whole library.
        within: Option<std::sync::Arc<std::collections::HashSet<gmuse_ids::copy::CopyId>>>,
        /// The playlist the play came from, in its order — #329. When present, the
        /// programme is these rows and `within` is not consulted.
        order: Option<Vec<gmuse_ids::copy::CopyId>>,
    },
    /// **What is playing, as one answer** — #243, #248.
    ///
    /// Asked rather than derived from the event stream, because the events are
    /// discontinuities (§7.8) and a client that arrives between two of them has
    /// nothing to go on. The same reply channel as `Queue` and for the same
    /// reason: `Held.playing` is owned by the loop below, so a connection worker
    /// asks and waits one tick.
    Playing { reply: Sender<Sounded> },
    /// **The programme changed under us; take a fresh window** — #257.
    ///
    /// The queue itself is no longer asked of this thread: a view's `queue.*` goes to the
    /// librarian, which owns the programme. But the window here was filled from the old
    /// one, so the mutation has to say so or the change would not take effect until the
    /// window ran dry two tracks later.
    Resync,
    /// **The programme was edited, not replaced; refill from where we are** — #301.
    ///
    /// The distinction [`Resync`] does not draw. An enqueue, a removal, a reorder or a mode
    /// change leaves the sounding entry exactly where it was, so asking for the head throws
    /// away a position that is perfectly good — and, worse, replaces the window with the
    /// programme's first track, which is what made a track queued with `E` never play.
    ///
    /// [`Resync`]: Ask::Resync
    ResyncHere,
}

/// What is sounding, for a client that asked instead of waiting.
///
/// **The same fields the `playback` event carries**, which is the point: a snapshot
/// and an event that disagreed about shape would make a client hold two parsers for
/// one fact. `playback_event` and this are built from the same three sources.
pub struct Sounded {
    pub state: State,
    pub recording: Option<String>,
    /// Frames into the *current track* — see `playback_event` on why the base is
    /// subtracted here rather than sent alongside.
    pub pos_frames: Option<u64>,
    pub generation: u32,
    pub sample_rate: u32,
    /// Zero when the device has gone, so a client knows not to extrapolate (#212).
    pub rate: f64,
    /// **Which programme entry is sounding** — #257.
    ///
    /// Opaque: this side mints nothing and reads nothing from it. It is here because the
    /// programme is the librarian's and *where you are in it* is this one's, so a view asking
    /// for "up next" has to get the anchor from here and the page from there.
    ///
    /// Not the recording, which a programme may hold twice.
    pub entry: Option<u64>,
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
        desk: Librarian,
        // **What `rescan` reconciles.** Held here rather than asked for per
        // command: the roots are the engine's configuration and a command carries
        // no context (I9).
        roots: Vec<std::path::PathBuf>,
        engine: Arc<std::sync::OnceLock<std::sync::Weak<super::Engine>>>,
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
                        queue: super::queue::Playing::new(),
                        playing: None,
                        pending: None,
                        next_cue: None,
                        sounding: None,
                        announced: 0,
                        folded: desk.moved(),
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
                        // **A setting outlives the stream it was set on** — #318. The
                        // fresh handle opens from `cfg`, which is what the command line
                        // said, not what a `set` said since.
                        let on_device_gone = player.on_device_gone();
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
                        player.set_on_device_gone(on_device_gone);
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
                            .map(gmuse_ids::id::Id::from_uuid)
                            && let Some(cue) =
                                super::queue::Playing::cue_for_recovery(&desk, recording)
                        {
                            player.load(source_of(&cue, 1.0));
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
    /// The window, and the door to the programme behind it (#257).
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
    /// **What was spliced and has not started yet**, beside `pending`.
    ///
    /// A splice becomes current at `Loaded`, not when it is handed over — the same reason
    /// `pending` exists. Without it a play would be counted against the track that was still
    /// playing when the next one was cued.
    next_cue: Option<super::window::Cue>,
    /// **What is sounding, and whether it has been counted** — #257.
    ///
    /// A play is earned partway through, which is not an event the player has, so it is
    /// polled — the same reason the underrun count is. Cleared on every load, so a track
    /// played twice counts twice.
    sounding: Option<(super::window::Cue, bool)>,
    /// **The library's last-seen version**, to notice a change by a number.
    ///
    /// It was an `Arc<Answer>` compared by pointer — the librarian's own fold, held here so
    /// the engine could tell when it moved. A counter says the same thing and names nothing
    /// (#257).
    folded: u64,
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
    desk: &'a Librarian,
    roots: &'a [std::path::PathBuf],
    engine: &'a Arc<std::sync::OnceLock<std::sync::Weak<super::Engine>>>,
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
            Ok(Ask::Do(instruction)) => apply(player, instruction, desk, roots, held),
            // **Queued rather than waited for** — #259. The librarian may be reconciling
            // 7,267 files, and a play gesture that timed out answered `accepted: true` while
            // doing nothing. It is loaded on the tick its answer arrives, so the `playback`
            // event follows the sound rather than the request — which is what §7.8 says an
            // event is for.
            Ok(Ask::Load {
                recording,
                within,
                order,
            }) => {
                held.queue.start(recording, within, order);
                false
            }
            // **Answered from the same three sources the event is built from**, and
            // deliberately not from a cached copy of the last event: a snapshot that
            // could disagree with the stream is worse than no snapshot, because a
            // client cannot tell which one is stale.
            Ok(Ask::Playing { reply }) => {
                let _ = reply.send(sounded(
                    player,
                    &shared,
                    held.playing.as_deref(),
                    held.sounding.as_ref().map(|(cue, _)| cue.token),
                    lost,
                ));
                false
            }
            // **The programme moved, so the window did too** — #257.
            //
            // Whoever mutated it did so through the librarian, which is the owner; this
            // side only has to notice. A resync takes a window from the top of the
            // programme as it now stands, which is what makes a `queue.set` audible at the
            // next track rather than two tracks later.
            Ok(Ask::Resync) => {
                held.queue.resync();
                false
            }
            Ok(Ask::ResyncHere) => {
                held.queue.resync_here();
                false
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => false,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        };

        // **A new fold may have moved a file, so re-cue what is waiting** — #257.
        //
        // There is no pool to refresh any more; the programme is the librarian's and it
        // rebuilt itself when the fold did. What can go stale here is a *locator*: a relink
        // or a rescan changes where a row's audio is, and the window holds paths. Taking a
        // fresh window is cheap and is the only thing that could be wrong.
        let now = desk.moved();
        if held.folded != now {
            // **From where we are, not from the top.** The programme did not change — a
            // locator did — so the position is still good and asking for the head would
            // discard it. `resync` is for a view replacing the programme.
            held.queue.resync_here();
            held.folded = now;
        }

        // **A play is earned by sound** — #257. Polled rather than triggered, because the
        // interesting moment is halfway through and the player has no event for it.
        if let Some((cue, counted)) = held.sounding.as_mut()
            && !*counted
            && gmuse_programme::counts_as_played(player.position(), cue.length)
        {
            *counted = true;
            desk.want(Wanted::Played {
                recording: cue.recording,
                encoding: cue.encoding,
            });
        }

        // **One tick of the programme pipeline, and nothing waits** — #259. Absorbs a reply
        // if one arrived, sends the next ask if the line is free, tops the window up if it is
        // short. A gesture whose answer is here gets loaded now.
        if let Some(cue) = held.queue.tick(desk) {
            moved |= load_picked(player, held, Some(cue));
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
                    // **A `pop_front`, and no ask at all** — #257. The librarian resolved
                    // the path when it filled the window, so nothing here queries anything
                    // on the deadline. A cue that reached the window is one this device can
                    // open: an unopenable row was dropped librarian-side, which shortens the
                    // window rather than handing the decoder a path that would underrun.
                    match held.queue.take_next() {
                        Some(cue) => {
                            held.pending = Some(cue.recording.to_string());
                            player.set_next(Some(source_of(&cue, player.volume())));
                            held.spliced(cue);
                        }
                        // The programme ran out. Letting playback stop is the answer, and
                        // it has to be *an* answer or the decoder waits.
                        None => player.set_next(None),
                    }
                }
                // **The join happened here, and nowhere else.**
                //
                // This promotion used to hang off `Finished`, on the reasoning that
                // finishing a track is when the next one takes over. It is not, and
                // the audio path asserts so: `a spliced track must not report
                // Finished` — a splice is deliberately continuous and `Finished` is
                // emitted once, when the whole chain runs out. So `playing` caught up
                // to `pending` only as everything stopped, and every event published
                // in between named the track that had already ended.
                //
                // `Loaded` is the boundary reaching the output, and the decoder holds
                // it back until the playhead is actually there (`announce_boundaries`)
                // — which is the same reason it stores `track_base` first. An initial
                // load arrives here too and needs nothing: `load_picked` set `playing`
                // when it was asked for, so `pending` is `None` and this is a no-op.
                PlayerEvent::Loaded { .. } => {
                    if let Some(next) = held.pending.take() {
                        held.playing = Some(next);
                        held.joined();
                    }
                    moved = true;
                }
                PlayerEvent::Finished => {
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
                // **The output we were on left** — #318. When the decoder paused for
                // it, that is a discontinuity like any pause, and every client hears
                // of it on `playback`. The reason goes on `engine`, which §7.8 gives
                // status; a view that wants to say *why* it paused reads it there.
                PlayerEvent::DeviceGone { left, paused } => {
                    moved |= paused;
                    if let Some(engine) = engine.get().and_then(|weak| weak.upgrade()) {
                        engine.publish(&Event::new(
                            Topic::Engine,
                            serde_json::json!({ "device_gone": left, "paused": paused }),
                        ));
                    }
                }
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
            if let Some(engine) = engine.get().and_then(|weak| weak.upgrade()) {
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
            if let Some(engine) = engine.get().and_then(|weak| weak.upgrade()) {
                engine.publish(&playback_event(
                    player,
                    &shared,
                    now,
                    held.playing.as_deref(),
                    held.sounding.as_ref().map(|(cue, _)| cue.token),
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

impl Held {
    /// **Spliced, and not heard yet.** `set_next` hands the decoder a track to join onto the
    /// end of the current one; nothing has played it.
    fn spliced(&mut self, cue: super::window::Cue) {
        self.next_cue = Some(cue);
    }

    /// **The join reached the output, so what was next is now sounding.**
    ///
    /// The counting starts here rather than at [`spliced`], and the difference is real: a
    /// splice handed over two minutes ago has not been heard, and counting it there would
    /// credit the play to whichever track was still playing when the next one was cued.
    ///
    /// [`spliced`]: Held::spliced
    fn joined(&mut self) {
        self.sounding = self.next_cue.take().map(|cue| (cue, false));
    }

    /// **A load sounds now**, unlike a splice — and it abandons anything spliced behind it.
    fn loaded(&mut self, cue: super::window::Cue) {
        self.next_cue = None;
        self.sounding = Some((cue, false));
    }
}

/// **A cue, as a thing to decode — and the gain applied here** (#257).
///
/// `source_for` played everything at unity and said why: the ReplayGain tags are the
/// library's and the person's gain preference is this side's, and a cue had no way to carry
/// the first. It does now, so the arithmetic happens where both halves are.
///
/// Track gain before album gain, because a queue is not an album: playing one track of a
/// record next to one of another is exactly when per-track normalisation is what a listener
/// wants. Falls back to unity when neither tag is there, which is the honest answer for a
/// file nothing measured.
fn source_of(cue: &super::window::Cue, _fader: f32) -> crate::audio::TrackSource {
    let db = cue.track_db.or(cue.album_db);
    crate::audio::TrackSource {
        path: cue.path.clone(),
        // **Clamped, and the peak is why.** A +6 dB correction on a track that already peaks
        // at 0.99 clips, and clipping is audible where being 6 dB quiet is merely wrong.
        gain: match db {
            Some(db) => {
                let linear = 10f32.powf(db / 20.0);
                let ceiling = cue
                    .track_peak
                    .or(cue.album_peak)
                    .filter(|p| *p > 0.0)
                    .map(|p| 1.0 / p)
                    .unwrap_or(1.0);
                linear.min(ceiling).clamp(0.05, 4.0)
            }
            None => 1.0,
        },
    }
}

/// Apply one text-language command. Returns whether it was a discontinuity.
fn apply(
    player: &PlayerHandle,
    instruction: Instruction,
    desk: &Librarian,
    // Where `Rescan` reconciles, which only the engine knows.
    roots: &[std::path::PathBuf],
    // **Everything it may change, as one borrow.** The queue, what the `playback` event
    // names, what was spliced, and what is being counted. Separate out-parameters said
    // nothing about how they relate, and adding what is sounding pushed the signature past
    // clippy's limit — they are all one thing, which is what `Held` is.
    //
    // `playing` is set on a load rather than promoted on `Finished`, and forgetting that is
    // why the first version of `next` skipped the audio while telling every view the old
    // track was still playing.
    held: &mut Held,
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
        // **One command each, and the decoder already resolves them.** `Play` stores
        // `Playing` only where a track is loaded and `Pause` stores `Paused` only from
        // `Playing`, so each is a single atomic store and neither is a read-modify-write
        // the render callback could observe mid-flight (#225).
        Instruction::Play => {
            player.play();
            true
        }
        Instruction::Pause => {
            player.pause();
            true
        }
        Instruction::Cue => {
            player.cue();
            true
        }
        Instruction::CuePreview => {
            player.cue_preview();
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
            // **Instant when the window holds it**, which is the common case: a `next` that
            // had to ask would feel like a key that sometimes works.
            let picked = held.queue.skip_next();
            load_picked(player, held, picked)
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
            // **Always asked**, because what precedes the position is never in the window —
            // a window looks forwards. Loaded on the tick the answer arrives.
            held.queue.skip_prev();
            false
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
            let Some(recording) = held
                .playing
                .as_deref()
                .and_then(|r| r.parse::<uuid::Uuid>().ok())
                .map(gmuse_ids::id::Id::from_uuid)
            else {
                return false;
            };
            // **Named, not written** — #257. `history::now_ms` was read here for a fact the
            // audio crate had no business appending; whoever appends decides when, because a
            // stamp is part of the write.
            desk.want(Wanted::Rate { recording, stars });
            false
        }

        // **What plays next** — §6 assigns these to the control thread.
        Instruction::Mode(which, to) => {
            held.queue.set_mode(which, to);
            false
        }
        Instruction::Scope(to) => {
            let mode = match to {
                ScopeTo::Set(mode) => mode,
                ScopeTo::Cycle => held.queue.scope().cycle(),
            };
            held.queue.set_scope(mode);
            false
        }

        // **A scan is the librarian's job.** Routed rather than duplicated: `roots`
        // lives on the engine and the appender is the only thing that may start one
        // (#237).
        // **Not a transport discontinuity**: it changes what a later reroute does,
        // and nothing now.
        Instruction::OnDeviceGone(to) => {
            player.set_on_device_gone(to);
            false
        }

        Instruction::Rescan => {
            desk.want(Wanted::Rescan {
                roots: roots.to_vec(),
            });
            false
        }
    }
}

/// Load whatever the queue picked, or do nothing if it picked nothing.
///
/// Shared by `Next` and `Prev`, which differ only in how the pick is made — and
/// having them differ in anything else is how the first version of `next` came to
/// tell every view the old track was still playing.
fn load_picked(player: &PlayerHandle, held: &mut Held, picked: Option<super::window::Cue>) -> bool {
    // **Resolves nothing** — #257. The cue arrived with a path the librarian chose and a
    // recording to name, so there is no library question left to ask here. That this
    // function used to take a `Desk` is the whole of what moved.
    let Some(cue) = picked else {
        // The programme ran out, or its next row is not held on this device.
        return false;
    };
    held.pending = None;
    held.playing = Some(cue.recording.to_string());
    player.load(source_of(&cue, player.volume()));
    held.loaded(cue);
    true
}

/// §7.8's payload for the main player.
///
/// **No `deck` field.** §7.8 says the topic carries "transport state for the
/// main player *and* each deck"; this is the main player, and #224 settled that
/// the player is a deck plus a queue rather than a deck. A `deck` key here would
/// claim a deck exists.
/// What is sounding, from the player, the shared atomics and the loop's own idea of
/// which recording it is.
///
/// **One function, two callers** — `playback_event` frames it as an event and
/// `Ask::Playing` hands it back as a reply. They were written separately first and
/// the position base was subtracted in only one of them, which is exactly the kind
/// of disagreement a snapshot is supposed to resolve rather than add to.
fn sounded(
    player: &PlayerHandle,
    shared: &Arc<Shared>,
    recording: Option<&str>,
    // **Which programme entry**, so "up next" has an anchor. Passed rather than read off a
    // handle, because what is sounding is the control thread's and this function is shared
    // with `playback_event`, which has the same three sources and no more.
    entry: Option<u64>,
    lost: bool,
) -> Sounded {
    let sounding = shared.sounding();
    // **Into this track, not into the stream** — I13, I17, #246.
    //
    // A gapless transition deliberately does not flush, so the frame counter runs
    // straight across the join and `Shared::track_base` is what makes it mean
    // anything to a person; `PlayerHandle::position` has always subtracted it. The
    // wire carried the raw stream offset, so an attached view's clock never reset at
    // a join and ran past the track's own duration — honest locally, silently wrong
    // one process away, which is #212's shape exactly.
    //
    // Subtracted here rather than sent as a second field, because a stream offset is
    // not a user-facing grain (I17) and nothing asks for one. It also makes a track
    // change the position discontinuity §7.8 already lists it as.
    let base = shared.track_base.load(std::sync::atomic::Ordering::Acquire);
    Sounded {
        entry,
        state: player.state(),
        recording: recording.map(str::to_owned),
        pos_frames: sounding.map(|s| s.frame.saturating_sub(base)),
        generation: shared.generation(),
        sample_rate: player.device_rate(),
        // **Zero when nothing is sounding** — #212, I13.
        //
        // A client extrapolates position locally between discontinuities (§7.8), so
        // it needs to know when *not* to. A lost device stops the render step, which
        // freezes `position_frames` — so the local `PlayerHandle::position` is honest
        // by construction and a remote one was not: it took the frozen position, reset
        // its own clock, and advanced forever while nothing played.
        //
        // `rate` is the field that already means this. It was the constant 1.0 with a
        // note that varispeed does not exist yet; a stalled stream is rate 0, which is
        // the same quantity honestly measured rather than a fourth run state invented
        // for the case.
        rate: if lost { 0.0 } else { 1.0 },
    }
}

fn playback_event(
    player: &PlayerHandle,
    shared: &Arc<Shared>,
    told: Told,
    recording: Option<&str>,
    // Which programme entry is sounding — the anchor a view pages "up next" from.
    entry: Option<u64>,
    // Whether the device has gone, so the position is not advancing (#212).
    lost: bool,
) -> Event {
    // **`told` rather than a fresh read, for the two fields that name the stream.**
    //
    // The loop decided this event was worth publishing by comparing `told` to what it
    // last announced; re-reading the state here could describe a *different* moment
    // than the one that triggered the publish. `sounded` supplies everything that is
    // a measurement rather than a decision.
    let now = sounded(player, shared, recording, entry, lost);
    let at_ns = shared.sounding().map(|s| s.at_nanos);
    Event::new(
        Topic::Playback,
        serde_json::json!({
            "state": match told.state {
                State::Playing => "playing",
                State::Paused => "paused",
                State::Stopped => "stopped",
            },
            "recording": now.recording,
            "pos_frames": now.pos_frames,
            "stream_generation": told.generation,
            "sample_rate": now.sample_rate,
            "at_ns": at_ns,
            "rate": now.rate,
        }),
    )
}

#[cfg(test)]
mod tests {
    /// **A splice is counted when it joins, not when it is handed over** — #257.
    ///
    /// `set_next` gives the decoder a track to join onto the end of the current one, which can
    /// be two minutes before anybody hears it. Counting there would credit the play to
    /// whichever track was still playing — the same confusion that made the `playback` event
    /// name the wrong recording until `pending` was added.
    #[test]
    fn a_spliced_track_is_not_counted_until_it_joins() {
        let mut held = Held {
            queue: super::super::queue::Playing::new(),
            playing: None,
            pending: None,
            next_cue: None,
            sounding: None,
            announced: 0,
            folded: 0,
            generation: 0,
        };

        let first = a_cue(1);
        held.loaded(first.clone());
        assert_eq!(
            held.sounding.as_ref().map(|(c, _)| c.token),
            Some(1),
            "a load did not start sounding"
        );

        // Spliced while the first is still playing.
        held.spliced(a_cue(2));
        assert_eq!(
            held.sounding.as_ref().map(|(c, _)| c.token),
            Some(1),
            "the splice was counted against the track still playing"
        );

        // The join reaches the output.
        held.joined();
        assert_eq!(
            held.sounding.as_ref().map(|(c, _)| c.token),
            Some(2),
            "the join did not promote what was spliced"
        );
        assert!(
            !held.sounding.as_ref().unwrap().1,
            "it started already counted"
        );
        assert!(held.next_cue.is_none(), "the splice was promoted and kept");
    }

    /// **A load abandons what was spliced behind it.** Pressing next mid-track means the
    /// track that was queued to join never sounds, so it must not be promoted later.
    #[test]
    fn a_load_discards_a_splice_that_will_never_sound() {
        let mut held = Held {
            queue: super::super::queue::Playing::new(),
            playing: None,
            pending: None,
            next_cue: None,
            sounding: None,
            announced: 0,
            folded: 0,
            generation: 0,
        };
        held.loaded(a_cue(1));
        held.spliced(a_cue(2));
        held.loaded(a_cue(3));

        assert!(
            held.next_cue.is_none(),
            "an abandoned splice survived a load"
        );
        held.joined();
        assert_eq!(
            held.sounding.as_ref().map(|(c, _)| c.token),
            None,
            "a join after a load promoted nothing, so it cleared what was sounding"
        );
    }

    fn a_cue(token: u64) -> super::super::window::Cue {
        super::super::window::Cue {
            token,
            recording: gmuse_ids::id::Id::derived(&token.to_le_bytes()),
            encoding: gmuse_ids::id::Id::derived(&token.to_be_bytes()),
            length: std::time::Duration::from_secs(200),
            path: std::path::PathBuf::from("/nowhere.flac"),
            track_db: None,
            track_peak: None,
            album_db: None,
            album_peak: None,
        }
    }

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
            on_device_gone: Default::default(),
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
            on_device_gone: Default::default(),
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

    /// **The reported position is into the track, not into the stream** — I13, I17, #246.
    ///
    /// A gapless transition deliberately does not flush, so `position_frames` runs
    /// straight across the join and `Shared::track_base` is what makes it a position
    /// rather than an offset. `PlayerHandle::position` has always subtracted it; the
    /// wire did not, so an attached view's clock never reset at a join and climbed
    /// past the duration of the track it was naming.
    ///
    /// **A unit test rather than a socket one, because the natural join is a clock.**
    /// The first version waited for a real gapless advance under the null renderer and
    /// passed alone and failed inside `just ci` — thirty-four other tests starving the
    /// renderer meant the 1-second track had not reached its join inside a 25-second
    /// window, and it reported `[1024, 1632, 1760]`, three positions from the first
    /// track. A test whose verdict depends on machine load is worse than no test.
    ///
    /// So the join is *stated* instead of waited for: a mapping at a known frame and a
    /// `track_base` under it is exactly the state the decoder leaves behind, and the
    /// subtraction is the whole claim.
    #[test]
    fn the_reported_position_is_relative_to_the_current_track() {
        let player = PlayerHandle::headless(
            PlayerConfig {
                device: None,
                rate: None,
                resample: Default::default(),
                volume: 1.0,
                on_device_gone: Default::default(),
            },
            null::Pace::Fast,
        )
        .expect("a headless player");
        // **Not the player's own `Shared`.** `playback_event` takes the two separately,
        // and the headless renderer at `Pace::Fast` republishes its mapping in a tight
        // loop — a first version wrote a frame into the live one and read back the
        // renderer's next value, reporting 0 for a reason that had nothing to do with
        // the subtraction under test. So the player supplies only the device rate, and
        // the state being asserted about is stated here.
        let shared = std::sync::Arc::new(crate::audio::Shared::new());
        let generation = 1;
        shared
            .stream_generation
            .store(generation, Ordering::Release);

        // The state a join leaves: the stream is 48,000 frames in, and the track
        // playing started at 47,616 of them. `announce_boundaries` stores the base
        // before it sends the `Loaded`, so this ordering is the real one.
        shared.map_frame.store(48_000, Ordering::Relaxed);
        shared.map_at_nanos.store(1, Ordering::Relaxed);
        shared.map_generation.store(generation, Ordering::Relaxed);
        shared.map_latency_nanos.store(0, Ordering::Relaxed);
        // Even, so the mapping reads as complete rather than mid-write.
        shared.map_seq.store(2, Ordering::Release);
        shared.track_base.store(47_616, Ordering::Release);

        // **The instrument is attached**, and this is the assertion that proves it:
        // `sounding()` refuses a torn or stale mapping by returning `None`, and
        // `pos_frames` would then be `null` — which is not zero and not 384, but would
        // sail past a check that only looked for "not 48,000".
        assert!(
            shared.sounding().is_some(),
            "the mapping was refused, so the event below has no position to report"
        );

        let told = Told {
            state: crate::audio::State::Playing,
            generation,
        };
        let event = playback_event(&player, &shared, told, Some("a-recording"), None, false);

        assert_eq!(
            event.body["pos_frames"].as_u64(),
            Some(384),
            "the wire carried the stream offset rather than the position in the track"
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
                on_device_gone: Default::default(),
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
