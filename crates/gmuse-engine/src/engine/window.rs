//! **The next two or three cues, and nothing about how they were chosen** — #257.
//!
//! `docs/design/playback-programme.md` is the decision. The librarian owns the ordered
//! programme; this holds a window of it and no ordering policy at all.
//!
//! ## What this replaced, and why it is smaller as well as better
//!
//! `queue.rs` held a **materialised pool** — the whole library, ordered, rebuilt on every
//! library change — because §4.4 forbids the control thread from running a query and
//! `NeedNext` has a real deadline. That worked and cost a fold in the engine.
//!
//! A window answers the same problem with two entries: `NeedNext` is a `pop_front` with no
//! ask at all, and the refill happens afterwards, off the deadline and **without
//! blocking** — the reply arrives on a later tick of the control loop.
//!
//! ## Why it holds no mode
//!
//! Every playback mode is a rule the librarian answers, so a refill under repeat-current
//! simply returns the same entry again and this cannot tell. There is nothing here to set,
//! which is the decision working rather than a gap: an engine that could read the mode is
//! an engine that could act on it.
//!
//! ## Why a token and not a row
//!
//! A programme can hold one recording twice, so a position is named by the entry rather
//! than by what occupies it. **Opaque here**: it is stored, handed back in the next ask,
//! and never interpreted — which is what lets this file name no library type.

use std::collections::VecDeque;
use std::path::PathBuf;

/// One thing to play, as the librarian described it.
#[derive(Debug, Clone, PartialEq)]
pub struct Cue {
    /// Which programme entry. **Opaque** — hand it back, never read it.
    pub token: u64,
    /// **What is sounding, for the `playback` event to name.**
    ///
    /// The one library identity a cue carries, and it is unavoidable: a view identifies the
    /// playing track by recording — `gp` jumps to it, the info panel follows it — and this
    /// is the only actor that knows what is playing. Not the row: two rows can hold one
    /// recording, and what a listener hears is the recording.
    pub recording: gmuse_ids::id::Id<gmuse_ids::entity::Recording>,
    pub path: PathBuf,
    /// What the tags say, in dB, with nothing done to them.
    ///
    /// **How loud to play is this side's question.** The library says what the file
    /// claims; the gain preference is control state here, so the arithmetic is here too.
    pub track_db: Option<f32>,
    pub track_peak: Option<f32>,
    pub album_db: Option<f32>,
    pub album_peak: Option<f32>,
}

/// How short the window may get before it is worth asking for more.
///
/// **Two, so `NeedNext` is answered from memory and the refill is never on the deadline.**
/// One would mean asking at the moment the decoder asks, which is the shape this replaces.
pub const KEEP: usize = 3;

/// The cues in hand, and whether an ask is outstanding.
#[derive(Debug, Default)]
pub struct Window {
    cues: VecDeque<Cue>,
    /// The last token handed to the decoder, which is what a refill is addressed from.
    ///
    /// **Not an index.** The programme may be reordered under us and a token still
    /// resolves, where an index would silently name something else.
    from: Option<u64>,
    /// The programme version the last window came from, for reporting only.
    version: u64,
    /// True while a refill has been asked for and not yet absorbed, so the control loop
    /// does not ask again every tick.
    asked: bool,
}

impl Window {
    pub fn new() -> Self {
        Self::default()
    }

    /// **Take the next cue**, which is the whole answer to `NeedNext`.
    ///
    /// `None` means playback stops. That is an answer and has to be one — the decoder
    /// waits for a reply, and `NEXT_REPLY_TIMEOUT` is what happens if none comes.
    pub fn take(&mut self) -> Option<Cue> {
        let cue = self.cues.pop_front()?;
        self.from = Some(cue.token);
        Some(cue)
    }

    /// What is waiting, for a status surface. Does not consume.
    pub fn peek(&self) -> Option<&Cue> {
        self.cues.front()
    }

    pub fn len(&self) -> usize {
        self.cues.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cues.is_empty()
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    /// The last token handed to the decoder — **the playing position**, not the refill point.
    pub fn from(&self) -> Option<u64> {
        self.from
    }

    /// **The token a refill continues after** — the last one *held*, not the last one taken.
    ///
    /// The distinction is a bug that reached the real library and no unit test caught. With
    /// `[B, C]` in hand and `from = A`, asking what follows *A* answers `B`, which appends to
    /// give `[B, C, B]` — so five skips walked A, B, A, B, C, alternating between two
    /// adjacent tracks for ever.
    ///
    /// A refill extends the **end** of the queue, so it is addressed from the end. `from` is
    /// still the right anchor when nothing is held, because then the end *is* the position.
    pub fn tail(&self) -> Option<u64> {
        self.cues.back().map(|c| c.token).or(self.from)
    }

    /// **Whether to ask for more.** False while an ask is outstanding.
    ///
    /// Guarded so a short window does not queue an ask on every tick of the control
    /// loop — which, with the librarian answering in FIFO order behind a scan, would put
    /// thousands of identical jobs in the channel.
    pub fn wants_more(&self) -> bool {
        !self.asked && self.cues.len() < KEEP
    }

    /// How many to ask for.
    pub fn short_by(&self) -> usize {
        KEEP.saturating_sub(self.cues.len())
    }

    /// Note that an ask is outstanding.
    pub fn asked(&mut self) {
        self.asked = true;
    }

    /// **Add cues to the end**, which is a refill.
    pub fn extend(&mut self, version: u64, cues: impl IntoIterator<Item = Cue>) {
        self.version = version;
        self.cues.extend(cues);
        self.asked = false;
    }

    /// **Throw everything away and start from these** — a new programme, or a resync.
    ///
    /// `from` is cleared too: the token it held is from a programme that no longer holds
    /// it, and addressing a refill from it would get `Stale` forever.
    pub fn replace(&mut self, version: u64, cues: impl IntoIterator<Item = Cue>) {
        self.version = version;
        self.cues = cues.into_iter().collect();
        self.from = None;
        self.asked = false;
    }

    /// **Keep the position after a replace**, when the position did not actually move.
    ///
    /// A mode change refills from where we are, so the cues are new but the entry that is
    /// sounding is the same. Without this, `replace` clearing `from` would make the next
    /// refill have nothing to address itself from.
    pub fn resume_at(&mut self, token: u64) {
        self.from = Some(token);
    }

    /// An ask came back with nothing usable. Stop waiting, so the next tick may ask again.
    pub fn answered(&mut self) {
        self.asked = false;
    }
}
