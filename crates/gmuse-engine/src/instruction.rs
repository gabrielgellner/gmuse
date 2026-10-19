//! **What the control thread can be asked to do** — #213, I9, §12.
//!
//! ## Why this exists, which is the one thing the crate split was not supposed to
//! prove
//!
//! §4.3 calls the split "mostly mechanical". It was not, and this is where it
//! stopped being: the control thread matched on `cmd::Command`, the TUI's
//! vocabulary — sixty-three variants including `Up`, `Fold` and `PaneNext`, of
//! which it acted on ten and silently ignored the rest. A crate cannot depend on
//! the crate that depends on it, so drawing the line forced the question rather
//! than letting it stand.
//!
//! ## And it dissolves a bug rather than guarding against one
//!
//! Those two lists had drifted in both directions: `vol`, `rate`, `prev`, `set` and
//! `toggle` were admitted by the old socket's `is_remotable` and dropped by `apply` — each
//! answering `{"accepted": true}` and doing nothing — while `play` was handled and
//! refused at the gate, so unreachable. #233 caught that with a pair of tests that
//! scraped both functions' source and compared the names.
//!
//! A source scrape is a poor guard and it knew it: the first version read
//! `Command::` out of *comments*. What replaces it is a shape in which the drift
//! cannot be expressed — one total translation at the boundary, and an exhaustive
//! match here. A command the engine cannot carry out has no `Instruction`, so the
//! ingress has nothing to send and must say why.

/// One thing the engine can do on its own, with no view and no cursor.
///
/// **Every variant is reachable**, which is the property the two source-scraping
/// tests were approximating. `apply` matches exhaustively and there is no
/// catch-all, so a new variant here does not compile until it is handled.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Instruction {
    /// Pause if running, resume if not — the cmus toggle.
    ///
    /// Named for what it does rather than for the `pause` that spells it. **It is
    /// resolved here rather than by the sender** (#225, D-8): a view computing
    /// `if playing { Pause } else { Play }` races itself, because two presses
    /// arriving faster than the snapshot refresh both read `paused` and both send
    /// `Play`.
    Toggle,
    /// Start, or keep going — **idempotent** (#225, D-7).
    ///
    /// Not the text language's `play`, which starts the *selected* track and needs a
    /// cursor. This is the wire's, and it exists because `Toggle` alone is not enough:
    /// without the kitty keyboard protocol a held key arrives as repeated presses, and
    /// a toggle bound to one flutters where this is a no-op.
    /// Return to the reference point, or set it — **resolved by the engine**
    /// (#229, D-14), for the same reason `Toggle` is.
    Cue,
    /// Play from the reference without moving it — #228. The end of a preview
    /// is `Cue`, not a second instruction.
    CuePreview,
    Play,
    /// Halt, leaving the playhead where it is — **idempotent** (#225, D-7).
    ///
    /// The verb a broadcast needs: "pause everything" must leave everything paused,
    /// where "toggle everything" starts whatever was stopped. Nothing broadcasts yet
    /// — #224 settled that decks come later — but the verb is what makes it
    /// expressible without a loop over toggles.
    Pause,
    /// **Already an audio type.** `SeekSpec` is text-shaped — "+10", "50%" — and
    /// resolving it is the boundary's job, so what arrives here is frames.
    Seek(crate::audio::SeekTo),
    /// Skip forward, which the engine answers from the queue it owns.
    Next,
    /// **And backward, which is the pool's previous.** Not the inverse of `Next`:
    /// the queue is consumed by being played from, so this never reads it.
    Prev,
    Volume(Fader),
    /// Rate what is playing. `None` clears it, which the model distinguishes from
    /// rating it zero.
    Rate(Option<u8>),
    Mode(Mode, Switch),
    Scope(ScopeTo),
    /// What a departed output does to playback — #318. The engine's, because the
    /// engine holds the stream that is rerouted.
    OnDeviceGone(crate::audio::OnDeviceGone),
    /// Reconcile the roots the engine was started with.
    Rescan,
}

/// Where to put the fader.
///
/// **Both arms, because a relative change needs the current position** and only
/// the engine knows it. Resolving `+5` at the boundary would mean the sender
/// guessing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Fader {
    To(f32),
    By(f32),
}

/// A mode that decides what plays next, which §6 makes control state.
///
/// **Three, not eleven.** The `:` line has settings for album headers and gutter
/// numbering too; those are a view's layout and have no meaning in a process with
/// no screen, so they are not expressible here at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Shuffle,
    Repeat,
    RepeatCurrent,
}

/// Set it, or flip whatever it is.
///
/// `Flip` rather than the sender reading and negating, for #225's reason: a toggle
/// resolved by the sender races itself, and a repeating key bound to one flutters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Switch {
    On,
    Off,
    Flip,
}

/// Where to point the scope, or one step round the cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeTo {
    Set(gmuse_programme::Scope),
    Cycle,
}
