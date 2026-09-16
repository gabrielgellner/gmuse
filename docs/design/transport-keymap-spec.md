# Transport & Keymap Design Spec

Status: design, **and the transport verbs are built** (#225). Ordered by impact, not by build order.

**What ships of it.** `player.transport` carries `play`, `pause` and `toggle` as three distinct verbs (D-6, D-7), and
the engine resolves toggle where the run flag lives (D-8). `audio::transported` is the rule as a pure function, so D-7's
idempotence is a property test rather than a claim. No `DeckSel` yet: #224 settled its shape and nothing has decks, so a
parameter with one reachable value would be a wire commitment bought with nothing. Acceptance criteria 3 and 4 hold; 5
is deferred by #224's decision that `audio::State` gains `Armed` when something can produce one.

**§4's parameterised jump is decided but not built** (#226). O-3 is settled below, which is what fixes whether `Persist`
crosses the wire at all. Criterion 6 is not met and cannot be yet: `When`, `Marker`, `BeatOffset` and `Persist` appear
nowhere in the tree, there are no decks, and there is no beat grid, so all three axes have exactly one reachable value —
`Now`, a position, `Permanent`. A `Jump` declared today would be a struct with nothing to parameterise, and
`gmuse-proto`'s header already rules that out: inventing the types to hold nothing makes a method name public API before
its payload is designed. It waits on cue points (#229) and a grid, not on anyone's time.

**§6's modes were mostly already built, and criterion 2 is now met** (#227). `Context::Transport` is the one context
that does not fall through to `Common`, and `-- TRANSPORT --` is drawn persistently with a fallback for when the panel
will not fit, so D-15 and D-16 hold and are tested. What was missing was the reporting: a bad binding was **counted**
rather than named, its detail went to an `eprintln!` that `ratatui::init` covers a few lines later, and a chord written
two ways was not detected at all — the binding table is a map, so the loser vanished. A duplicate is now an error that
names both spellings, the first offender is named on a notice that does not expire, and every problem is written to the
audit. It still launches: a typo disables one key, and refusing to start would cost the player.

**§5's cue is built, and `stop` is gone as a verb** (#229). Cue carries both readings — paused marks the reference,
running returns to it and waits — with the reference starting at the top of the track, which is D-14 and is what lets
one verb cover cmus's "back to the start" and a CDJ's cue. The rule is `audio::cued`, resolved at the engine for D-8's
reason: a sender that decided "am I playing? then return" reads a snapshot that may already be wrong. Hold-to-preview is
#228's third of it.

The bottom row is now `z x c v` — prev, play/pause, cue, next — in transport mode and under `<leader>` alike, with `b`
left unbound and reserved. **`v` was stop and is now next**, which the spec calls taste rather than derivation and is
the one keypress a person feels; what makes it affordable is that stop is gone, so the mistake reaches nothing
destructive. Nothing is bound to `play` any more: the row is transport, and starting the row under the cursor is Enter's
job.

**The verb went and the engine's stop stayed**, because they are not the same thing. D-10 says stop is pause plus a
return to the reference, and `PlayerCmd::Stop` is more: it clears the outbox, drops the answer to `NeedNext` and cancels
draining — the **queue-level** condition #224's O-4 named, which a deck has no concept of. The engine still reaches
`Stopped` on its own at the end of a programme and when a device is lost.

**§8's momentary fallback is built, and criterion 7 is no longer vacuous** (#228). It was: "every momentary verb remains
reachable with `hold_capable == false`" was satisfiable by an empty set, because there were none — `bend`, `KeyPhase`
and `while_pressed` existed nowhere, and `slip` in the tree is a row type. Cue preview is the first one, and building it
is what gave the criterion a subject.

`transport::momentary` is the rule, and **the fallback is not a second code path**: a press begins, a release ends, and
on a terminal that cannot report release the release simply never arrives, so the _next press_ ends it. The latch falls
out of the same three lines. `hold_capable` comes from `supports_keyboard_enhancement` at startup, and only
`REPORT_EVENT_TYPES` is pushed — the other flags change how keys are _encoded_, which would rewrite what every existing
binding matches.

A release reaches **only a binding whose command is momentary**; `is_press()` stays the door for everything else, so no
keymap written for presses sees an event that would fire it twice. That also settles where holding works: a chord cannot
be held, because a release names one key and never a sequence, so `P` in transport mode is holdable and `<leader> P`
outside it latches. Both are reachable, which is what the criterion asks. The status line says `[preview]` or
`[preview latched]` so the difference is visible rather than mysterious.

D-17 has a test now — browsing asks the transport for nothing. D-19 (load behind a modifier) is deferred on #226's
grounds: its argument is about clobbering a live deck during a mix, and with one player "play the selection" on a bare
key is what cmus does and what is wanted.

**Where this sits.** It is the missing design for the `deck.*` and `param.*` method families that
[`gmuse-engine-architecture.md`](gmuse-engine-architecture.md) §7.4 names and #215 deliberately did not implement,
because their payloads — `DeckId`, `When`, `Lane`, `BindingTarget` — had no design. This is that design.

**The Rust in this document is illustrative.** The crate paths it names (`crates/engine`, `crates/tui`) are not the ones
that exist: the workspace is `gmuse-ids`, `gmuse-library`, `gmuse-proto`, `gmuse-client` and `gmuse`, with `gmuse-tui`
still to be split out (§4.3; `gmuse-engine` landed in #213). Four of the types it declares collide by name with types
that already ship — see O-5 to O-8, which are integration decisions rather than defects in the reasoning above.

## 0. Scope

This spec covers the **transport command vocabulary** and the **key binding layer** that sits in front of it. It does
not cover audio DSP, deck mixing, or the library query language.

It decides five things:

1. What lives in the `Command` enum versus what lives in the keymap.
2. How a command names the deck it acts on.
3. Whether play and pause are one verb or several.
4. How cue, hot cue, beatjump and loop-roll decompose into a single parameterised jump.
5. How modes and key events are handled given terminal capability variance.

Every normative claim below is marked **D-n** and derived from either prior-art evidence (§8) or an architectural
invariant already fixed for this project. Where a claim rests on taste rather than derivation, it says so.

---

## 1. The command language is the interface — the keymap is data

**Invariant (already fixed):** the engine is driven entirely by a command language with structured input/output; the TUI
is one view, the nvim plugin is another.

**D-1. Nothing in the transport design may assume a keyboard.** The nvim plugin cannot use bare `x` or `c` — both are
taken in normal mode — so it will bind `<leader>` sequences or `<C-...>`. A MIDI controller has no letters at all. Any
design where a verb's identity is tied to a key position fails for two of the three views.

Consequence: the `Command` enum is the durable artifact and the binding table is configuration. Concretely:

```rust
// crates/engine/src/command.rs  — the parse boundary. No key codes here, ever.
pub enum Command {
    Transport(DeckSel, Transport),
    Library(LibraryCmd),
    Mode(ModeCmd),
    // ...
}
```

```rust
// crates/tui/src/keymap.rs — view-layer only.
pub struct Keymap {
    bindings: HashMap<(Mode, KeyChord), Command>,
}
```

**D-2. The keymap is loaded data, not a `match` in the input handler.** Derivation: every surveyed application ships
more than one default keymap and lets users remap freely — rekordbox ships two presets (`Performance 1`,
`Performance 2`) that are different models, not different key choices; VirtualDJ ships four (`Default`,
`Default (Legacy)`, `Rekordbox compatible`, `Serato compatible`); Renoise stores its bindings in `KeyboardMapping.xml`
and states every shortcut is user-definable. A hardcoded match arm per key has to be rewritten to support any of this.

Implementation note: parse the binding file into `Keymap` once at startup and surface errors then (unknown command name,
unparseable chord, duplicate binding within a mode). Do not carry `Result` into the input hot path.

---

## 2. The deck is an operand, not a region of the keyboard

Three models exist in prior art. See §8 for the evidence table.

| Model       | Deck encoded by | Examples                                  |
| ----------- | --------------- | ----------------------------------------- |
| Hand split  | which hand      | Mixxx, VirtualDJ (current), rekordbox P1  |
| Row split   | which row       | Serato, Traktor, rekordbox P2             |
| Target deck | focus state     | VirtualDJ legacy, Traktor "Device Target" |

**D-3. Use the target-deck model.** Derivation: the hand-split and row-split models both halve the available keyspace,
because each verb must be spelled twice. That is affordable when the vocabulary is ~20 verbs. It is not affordable here,
because (a) this player carries a full library/browser vocabulary that DJ software delegates to the mouse, and (b) the
vim-style design wants counts and operators, which consume keyspace faster than flat verb sets do.

The target-deck model is also the only one that composes with counts: `8z` cannot mean "deck 1, jump back 8" and "deck
2, jump back 8" simultaneously under a hand split without a second count prefix.

**D-4. Deck selection is a three-case enum, not a `usize`.**

```rust
pub enum DeckSel {
    Focused,        // the current focus; the common case
    Other,          // the non-focused deck — one-shot, does NOT move focus
    Id(DeckId),     // explicit
    All,            // broadcast; see D-5
}
```

`Other` recovers the one genuine advantage of the hand split: Mixxx's manual justifies its left/right split precisely by
simultaneous cross-deck action ("start playing a track on deck 1 whilst stopping deck 2"). A one-shot prefix gives that
without the keyspace cost — this is vim's `"a` register-prefix pattern, not a mode change.

`All` is required and cannot be emulated by looping `Focused`: "pause everything" must be idempotent (see D-7), and a
loop over toggles is not.

**D-5. Focus is engine state, not view state.** Two views may be attached at once. If the TUI and the nvim plugin each
track their own focus, `DeckSel::Focused` resolves differently depending on which view sent the command, which is a
latent bug with no good failure mode. Focus lives with the transport owner; views render it from the state snapshot.

---

## 3. Transport verbs: toggle is necessary but not sufficient

**D-6. Play/pause is a single toggle in every keymap.** Derivation: unanimous in prior art. Mixxx `D`/`L`, Serato
`W`/`S`, VirtualDJ `Alt+1`/`Alt+7`, rekordbox `Z`/`N` — all are labelled play/pause and all are one key per deck. No
surveyed application binds separate play and pause keys.

**D-7. The `Command` enum nonetheless carries `Play`, `Pause` _and_ `Toggle`.**

```rust
pub enum Transport {
    Play,                  // idempotent: ensure running
    Pause,                 // idempotent: ensure halted
    Toggle,                // resolved by the transport owner — never by a view
    Cue(KeyPhase),
    Jump(Jump),
    // ...
}
```

Three independent derivations, each sufficient on its own:

- **Key repeat.** Without the kitty keyboard protocol, a held key arrives as repeated presses. A toggle bound to a
  repeating key flutters; `Play` sent twice is a no-op. (With enhancement flags active, additionally drop
  `KeyEventKind::Repeat` for toggle bindings.)
- **Broadcast and macro use.** `DeckSel::All` + `Pause` must halt everything. `All` + `Toggle` starts whichever deck was
  stopped. These are different commands and both are wanted.
- **OS media integration.** `MPRemoteCommandCenter` (macOS) and MPRIS (Linux) both expose play, pause and toggle as
  _distinct_ commands. A toggle-only enum forces the integration layer to synthesise state, duplicating D-8's race.

**D-8. `Toggle` is resolved where the state lives, never by the sender.** If a view computes
`if snapshot.playing { Pause } else { Play }`, two presses arriving faster than the snapshot refresh both read `paused`
and both send `Play`. Send `Toggle` across the ring buffer and let the transport owner read its own authoritative state.

**D-9. Two running states, not three — plus a scheduled state once quantize exists.** Winamp/cmus have
stopped/paused/playing. A deck has a playhead position and a run flag; "stopped" is "paused at the cue point".
Collapsing to two makes `Toggle` total.

Quantized start adds a third case that is _not_ a fourth running state but a pending transition:

```rust
pub enum RunState {
    Paused,
    Armed { at: Position },   // scheduled start pending
    Running,
}
```

**Toggle while `Armed` cancels the arm and returns to `Paused`. It does not pause.** This is the second independent
reason `Toggle` cannot be a boolean flip in a view.

**D-10. Stop is derived, not a verb.** `Stop == Pause` then seek to the reference point. Expressed as `Cue` semantics
(§4), not as its own enum variant.

---

## 4. One parameterised jump replaces a dozen verbs

Elektron exposes four pattern-change modes: **sequential** (change at end of current pattern), **direct start**
(immediate, from the beginning), **direct jump** (immediate, preserving position), **temp jump** (immediate, plays once,
then returns).

**D-11. Those four modes are three orthogonal axes, and DJ jump verbs are points in the same space.**

| DJ verb              | when           | where                   | persist     |
| -------------------- | -------------- | ----------------------- | ----------- |
| Hot cue              | `Now`          | `Absolute(cue)`         | `Permanent` |
| Quantized hot cue    | `NextBoundary` | `Absolute(cue)`         | `Permanent` |
| Beatjump             | `Now`          | `Relative(±n)`          | `Permanent` |
| Loop roll / slip cue | `Now`          | `Absolute(cue)`         | `SnapBack`  |
| Censor               | `Now`          | `Relative(0)` + reverse | `SnapBack`  |

```rust
pub struct Jump {
    pub when:    When,
    pub target:  JumpTarget,
    pub persist: Persist,
}

pub enum When   { Now, NextBoundary(Quantize) }
pub enum Persist{ Permanent, SnapBack }

pub enum JumpTarget {
    Absolute(Marker),       // cue point, hot cue slot, track start
    Relative(BeatOffset),   // signed, in the mode's unit
}
```

**D-12. `When` crosses the ring buffer unresolved.** A view must not compute the timestamp of "next bar" — it does not
have sample-accurate beat-grid state, and any value it computes is stale by the time the RT thread reads it. Send the
`Quantize` spec; the RT side resolves it against the grid.

**D-13. Quantize is a mode-level default with a per-command override.** Elektron's pattern (arm a `FUNC`-style modifier,
then act) is the precedent. The default lives in engine state; the keymap may bind a prefix that overrides `When` for
the next command only.

---

## 5. Cue unifies the player and DJ meanings

CDJ cue semantics, which this should follow:

- Pressed while paused → set the cue point at the playhead.
- Pressed while playing → return to the cue point and pause.
- Held → preview from the cue point; snap back on release.

**D-14. Player-mode "return to start" is DJ-mode cue with no cue point set.** If the default cue point is the track
start, one verb covers both modes. This is why `v` (cmus stop) is free — its behaviour is absorbed here, not deleted
arbitrarily.

```rust
pub enum KeyPhase { Press, Release, Latched }
```

---

## 6. Modes

**D-15. Bare letters must be mode-gated, and this is forced by evidence, not preference.** Every surveyed application
collides letter keys against typing and resolves it with a modifier acting as a crude mode:

- VirtualDJ changed its default so all alphanumeric keys fill the search bar and deck actions moved to `ALT+`,
  explicitly to stop accidental deck actions when search lacked focus.
- Serato ships a `Playback Keys Use Shift` preference; without it a letter key jumps to the matching track.
- Ableton requires `Shift` on single-letter shortcuts whenever the Computer MIDI Keyboard is on.

A real mode solves this without burning a modifier: letters are verbs in deck mode, and `/` opens search in library
mode.

**D-16. The active mode must be rendered persistently.** Renoise draws a red border around the pattern editor whenever
edit mode is on. Modal transport without a visible indicator means stopping the wrong deck during a live mix. This is a
blocking UI requirement, not a nicety.

**D-17. Playhead and edit/browse cursor are decoupled.** Renoise's "Pattern Follow" toggle lets you edit one pattern
while others play. The equivalent: browsing the library must never move a deck's playhead, and deck focus must be
independent of browser selection. Load is an explicit command.

---

## 7. Default bindings (TUI view only)

The bottom row, after play/pause collapsed into one verb and stop was absorbed into cue:

| Key | Finger | Player mode     | Deck mode        | Abstract meaning             |
| --- | ------ | --------------- | ---------------- | ---------------------------- |
| `z` | pinky  | previous track  | beatjump back    | backward, in the mode's unit |
| `x` | ring   | play/pause      | play/pause       | toggle run state             |
| `c` | middle | return to start | cue              | return to reference point    |
| `v` | index  | next track      | beatjump forward | forward, in the mode's unit  |
| `b` | —      | _unbound_       | _unbound_        | reserved                     |

Rationale, flagged as **taste, not derivation**: `b` is the weakest key of the five (index stretch off home position)
and carried the highest-frequency verb. Shifting next/forward to `v` puts one meaning per key per finger across both
modes, and `v` is free because stop no longer exists as a verb (D-10, D-14). Known cost: a cmus user pressing `v`
expecting stop gets next-track. Accepted — stop is absent entirely, so no destructive action is reachable by that
mistake.

`b` stays unbound. If anything claims it, eject/unload is the candidate, since that is what actually replaced stop in DJ
terms.

**D-18. Counts prefix commands and are interpreted in the mode's unit.** `3z` = back three tracks in player mode; `8z` =
back eight beats in deck mode. The count is parsed by the keymap layer and lands in `BeatOffset` / track delta, not in
the `Command` enum as a raw string.

**D-19. Load is not a bare bottom-row key in deck mode.** Both Serato (`alt+W`/`alt+Q`) and rekordbox (`alt/option+W`)
put load-next/previous behind a modifier while leaving play and cue bare. Loading clobbers a deck that may be live; it
gets a modifier or a guard regardless of which key it lands on.

---

## 8. Key events and terminal capability

**D-20. Momentary verbs need a tap-to-latch fallback.**

Hold semantics are pervasive in prior art — VirtualDJ's nudge alters speed only while pressed and restores the
pitch-fader value on release; VDJscript has a `while_pressed` suffix for exactly this; Ableton latches a shortcut
momentarily after ~500 ms of hold.

But crossterm only reports `KeyEventKind::Release` after `KeyboardEnhancementFlags::REPORT_EVENT_TYPES` is pushed, which
requires the kitty keyboard protocol. kitty, WezTerm, Ghostty and foot support it; macOS Terminal.app does not.

```rust
// at startup
let hold_capable = crossterm::terminal::supports_keyboard_enhancement()?;
```

Every momentary verb (bend, cue preview, slip) needs both paths:

- capable terminal → `KeyPhase::Press` on down, `KeyPhase::Release` on up;
- otherwise → `KeyPhase::Latched`, toggling on each press.

The engine handles all three phases identically in its state machine; only the view differs. Surface `hold_capable` in
the UI so the behaviour change is visible rather than mysterious.

---

## 9. Prior-art evidence table

Collected from vendor documentation. Included so claims above can be re-checked rather than taken on trust.

| App                 | Model             | Play              | Cue               | Hot cues                     | Notes                                                    |
| ------------------- | ----------------- | ----------------- | ----------------- | ---------------------------- | -------------------------------------------------------- |
| Mixxx               | hand split        | `D` / `L`         | `F` / `;`         | `Z`–`V` / `M`–`/`            | manual justifies split by simultaneous cross-deck action |
| Serato DJ Pro       | row split         | `W` / `S`         | `I` / `K` (temp)  | `1`–`5` / `6`–`0`            | columns align verbs, rows select deck                    |
| Traktor Pro 3       | row split         | `W`/`S` common    | `Q` / `A`         | `1`–`5` / `6`–`0`            | "Device Target" gives per-hotkey target-deck mode        |
| VirtualDJ (current) | hand split, `ALT` | `Alt+1` / `Alt+7` | `Alt+2` / `Alt+8` | pads `Alt+ASDF` / `Alt+JKL;` | bare letters reserved for search                         |
| VirtualDJ (legacy)  | target deck       | —                 | —                 | —                            | `Tab` switches default deck; VDJscript `deck` prefix     |
| rekordbox P1        | hand split        | `Z` / `N`         | `A` / `H`         | `1`–`4` / `6`–`9`            |                                                          |
| rekordbox P2        | row split         | `W` / `S`         | `I` / `K`         |                              | same app ships both models                               |
| Renoise             | modal             | `Space`           | —                 | —                            | `Esc` toggles edit mode; red border indicates it         |
| Elektron            | modal             | —                 | —                 | —                            | four pattern-change modes → D-11 axes                    |

Splitting the number row in half for cue points is the one near-universal convention.

---

## 10. Open questions

- **O-1.** Does `DeckSel::Other` need a visible indicator during its one-shot window, or is the command fast enough that
  it does not matter?
- **O-2.** Should `Quantize` resolution live on the RT thread or on the decoder thread? D-12 says "not the view"; it
  does not decide between the other two.
- **O-3. Decided (#226, Decision Log 2026-09-16): per command, so `Persist` is on the wire.** Serato and Traktor model
  slip as deck state; the axis decomposition models it per command, and per command wins for the reason D-8 gives about
  toggle — a jump whose meaning is read from a flag the sender does not own races that flag, and means something the
  key-press did not ask for. Carrying `persist` in the command makes the intent atomic with the act. The deck-flag feel
  survives as a **keymap** default under D-13's existing mechanism (the mode chooses what the binding sends; the wire
  carries the resolved value), so "per-command may surprise" is answered in the binding rather than by putting a mode in
  the engine. The choice is also cheap to revisit in one direction only: a keymap default can restore deck-flag
  behaviour with no wire change, while moving a deck flag to per-command later would change every jump method, and
  method names are public API (§7.4).
- **O-4. Decided (#224, Decision Log 2026-09-12): the player is a deck plus a queue.** The deck-level transport is
  shared; the queue sits above it. "Stopped" is a queue-level condition — the queue not advancing — which a deck has no
  concept of, and §7.8 already models the two separately as "the main player _and each deck_".

### Integration with what already ships

Four names in this document are already taken. None of these is an argument against the reasoning above; each is a
decision about which declaration wins.

All four are now decided. They are kept here with their reasoning rather than deleted, because the reasoning is what
makes them re-checkable.

- **O-5. Decided: §10.1's declaration stands.** `When` is already defined, and more richly (§10.1). The shipped design
  has six variants — `Now`, `Frame(FrameTime)`, `TrackFrame`, `NextBeat { deck }`, `NextBar { deck }`,
  `Beat { deck, index }` — resolved by the RT callback against a beat grid delivered with the deck buffer, and scoped to
  a `stream_generation` so a stale schedule is refused rather than reinterpreted (#212). This document's
  `When { Now, NextBoundary(Quantize) }` is the same idea one level coarser. **Recommendation: §10.1's declaration
  stands**, and D-13's mode-level quantize default becomes the thing that _chooses_ which §10.1 variant a binding sends,
  rather than a second enum. D-12 is unaffected and is in fact §10.1's stated reason for RT-side resolution.

- **O-6. Decided: two enums, not three.** `Command` is already taken twice. `gmuse::cmd::Command` is the `:` line's
  language — 63 variants, shipping, and already carrying `Play`, `Pause`, `Stop`, `Next`, `Prev`, `Seek`, `Vol`. §7.5
  separately sketches a wire `Command` for the protocol. This document proposes a third. **Recommendation: there are
  two, not three** — the text language (`cmd::Command`, parsed by `cmd::parse`, which I4 requires be the only textual
  parser) and the wire (§7.5's), with this document's `Transport(DeckSel, Transport)` becoming a family inside the
  latter. The keymap binds to the wire form; `cmd.exec_line` parses text into it.

- **O-7. Decided: `audio::State` is not touched yet.** Dropping `Stopped` touches the RT callback. D-9 and D-10 collapse
  to two running states and derive stop from cue. `audio::State` ships as `Stopped | Playing | Paused`, is read by the
  render step every buffer, and appears in §7.8's `playback` event payload. The argument for two states is about a
  _deck_; the shipped enum is about the single player stream, and O-4 is the question of whether those are the same
  thing. **Decide O-4 first** — if player mode is deck mode with one deck, `audio::State` is revised and §7.8's payload
  with it; if not, `RunState` is a new deck type and `audio::State` is untouched.

- **O-8. Decided: `DeckSel` goes in, `DeckId` comes out.** D-5 argues focus is engine state because two views with their
  own focus make `Focused` resolve differently per sender — which is right, and is the same reasoning §6 uses to put
  control state on the control thread. But §7.5's sketch passes an explicit `deck: DeckId` in every deck method, which
  puts the resolution in the view. **The two cannot both stand.** D-5's argument is the stronger one and the protocol
  sketch is explicitly illustrative, but this is a wire-visible choice and method names are public API (§7.4), so it
  wants deciding before `deck.*` is implemented rather than after.

---

## 11. Acceptance criteria

1. `Command` and `Transport` compile with no dependency on `crossterm`. **Already structural**: they will live in
   `gmuse-proto`, whose dependency list is `gmuse-ids`, `serde` and `serde_json` — reaching for crossterm there does not
   fail a review, it fails to build.
2. `Keymap` is deserialised from a config file; an unknown command name fails at startup with the offending line, not at
   keypress.
3. `DeckSel::All` + `Pause` leaves every deck paused regardless of prior state; property test over arbitrary initial
   states.
4. `Toggle` sent twice within one snapshot interval produces exactly one state change; regression test against the D-8
   race.
5. `Toggle` while `Armed` yields `Paused` with no audio started.
6. Every `Jump` verb in the D-11 table is expressible without adding an enum variant.
7. The player runs with `hold_capable == false` and every momentary verb remains reachable.
