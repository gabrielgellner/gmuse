# Vision and Future — what gmuse is growing into

`gmuse` — Gabriel's MUSical Experience.

**This is a statement of direction, not a plan.** It has no dates, no ordering and no priorities, and nothing in it is
authorised by being written here. [`music-player-tui.md`](music-player-tui.md) remains the brief: it says what gmuse
_is_, and its §1 and its Decision Log are the only places scope actually moves. This document says what gmuse is
_growing toward_, so that the next design decision can be judged against something larger than the feature in front of
it.

It blends four sources:

- the brief's §1 — what the interface is for, local-first, and the out-of-scope list
- [`tree-view-design.md`](tree-view-design.md) and [`tree-navigation-design.md`](tree-navigation-design.md) — query →
  tree, and vim's fold vocabulary over it
- [`library-sync-design.md`](library-sync-design.md) — catalog, local state, events, and the airgap
- the working notes on how the ambition has grown now that the prototype has proven the idea

---

## 1. The one idea

**This is a digital vinyl collection, played from the keyboard.**

Two halves, and both are load-bearing.

**Owned.** Every track is a file I bought — Bandcamp first, another marketplace if not, and an unreleased bootleg last,
fetched by some _other_ tool and entered into the library as an ordinary Opus or FLAC file. There is no account, no
catalogue, and nothing that has an interest in what I play. The brief's 2026-08-29 amendment drew this line where it
belongs: what is out is **services**, not networks. My NAS holding my files is the same music on the other side of a
socket. Spotify is not.

**Played from the keyboard.** The brief already says it: gmuse is a way to interact with music, not a thing to look at.
It is off screen most of the time, and an arguable feature is judged by whether it makes the next action cheaper or only
makes the screen nicer. That rule is why there is no album art and no spectrum display, and it is equally why the remote
socket and session resumption matter more than their size suggests.

Everything below is an elaboration of those two sentences. When something in this document conflicts with them, they
win.

---

## 2. Three ways I use my music

The uses are not phases of a roadmap. They are three postures toward the same library, and they will be interleaved for
as long as the program exists — which is the point of listing them, because a design that serves one and obstructs the
others is the failure mode.

### Listening — the smallest surface, the highest bar

Most hours are this: some navigation, maybe a search, pick a playlist, then play / next / prev / shuffle / repeat /
rate. **This already works**, and the future of it is not features. It is refinement paid for by use — the friction
found in an actual evening of listening, fixed the next morning.

What that posture demands of the design is negative: nothing added for the other two uses may make this one slower or
noisier. The transport must stay one keystroke away and the now-playing line must stay legible when the DJ machinery
below exists but is not in use.

### Librarianship — the part that is really growing

This is where the ambition has moved. It is the work of keeping a collection _mine_: genre tags I actually believe,
playlists built rather than generated, and progressively richer per-track knowledge — BPM, key, cue points, MusicBrainz
identity, sonic embeddings.

The prototype has already shown what this can feel like. Query → tree with the nvim-shaped navigation over it is not a
browser, it is a way of asking a question and standing inside the answer. That is the thing to push on.

Three consequences, stated plainly:

1. **Tag editing has to happen somewhere.** The brief's §1 lists it as out of scope, reading only. That boundary is
   under real pressure and §7 below says how it should move if it moves.
2. **Analysis is not gmuse's job; reading its result is.** The pattern is established twice already — rsgain computes
   ReplayGain and we read the tag; bliss would analyse and we read the output. BPM, key and embeddings arrive the same
   way: an external tool writes, gmuse reads, and a new kind of knowledge becomes a set of _fields_ rather than a
   subsystem. Nothing about this contradicts "no recommendations from anyone else's server" — the computation is mine,
   on my hardware, over my files.
3. **The library lives on several machines and they cannot all talk to each other.** §6.

### The DJ instrument — the ambition, held honestly

Two decks, a three-band EQ per channel, a filter, faders with separate gain, and varispeed for beat matching. Not for a
crowd. If a technique removes the skill from a transition, that is a feature, not a cheat — this is a way to _listen_ to
my own collection, not a performance.

And the interesting half is not the mixer. It is the question the working notes actually ask: **what would a DJ setup
designed like an Elektron box or a tracker feel like, rather than like a pair of turntables?** Transitions programmed as
curves instead of performed. Controller movements recorded and replayed. Cueing arbitrary sections of _both_ decks over
the monitor so a transition can be rehearsed before the playing track reaches it — nonsense on a real rig, obviously
right here. Mixes kept as player-piano rolls in metadata, replayable, alongside the printed audio.

This is the use that most challenges the existing design, and §7 says exactly where.

---

## 3. What holds however far this grows

These are the parts that have already been paid for. They are listed together because the temptation, once the ambition
is this large, is to treat them as prototype scaffolding.

- **The audio callback is real-time.** No allocation, no lock, no syscall, no panic. This does not relax because there
  are now two decks; it gets harder and matters more.
- **Local first is not negotiable.** Every feature works with no network at all. A disconnected gmuse is a complete
  music player, not a degraded one. The airgapped work machine is the design target, not the sad path.
- **One store, many projections.** `Store` holds every track; `Library` is a membership rule over it; every view is a
  projection. Catalogs, filters and smart playlists all fell out of this rather than being built. Anything that starts
  computing from the `Store` behind the projections is a mistake, and it is how the model stops being able to absorb the
  next idea for free.
- **Parse at the boundary.** Strings become typed values at the edge — the rc file, the `:` line, the remote socket, and
  in future the editor buffer and the bundle on the drive. Nothing downstream handles a raw string.
- **Threads and channels. No async.**
- **Scope creep is this project's named failure mode.** Said in the brief, and worth repeating in the document whose job
  is to be ambitious.

---

## 4. The substrate: one vocabulary, many surfaces

This is the central design claim of this document, and it is the reason the ambition above is not fantasy.

gmuse has converged, without anyone planning it, on **one expression vocabulary appearing behind every surface**. Field
names are shared between the filter language, the sort keys and the format strings. A catalog is a saved filter. A smart
playlist is a saved filter with an order. A view is a sort plus a list of grouping levels — `ViewSpec` in
`tree-view-design.md` — and specs are data, so adding a view is configuration. The sync policy in
`library-sync-design.md` is a filter with a sort and a limit. `available` is a field.

The consequence to protect: **every new domain enters as fields and expressions, not as a feature.**

| New knowledge         | Enters as                         | Costs                                        |
| --------------------- | --------------------------------- | -------------------------------------------- |
| BPM, key, cue points  | numeric / structured fields       | a reader, a column, three vocabulary entries |
| Sonic embedding       | a distance function over a field  | `similar_to` in filter and sort              |
| Harmonic mixing       | a derived field over `key`        | one function, then `sort: key_distance(...)` |
| Availability          | a three-state field               | a level to group by, a predicate to prune    |
| "These two mix well"  | an event, then an aggregate field | nothing new in the query layer               |
| Play history, ratings | events, aggregated                | already done, and it is the proof this works |

A "mix compatibility view" is then not a feature to build. It is:

```
levels: [ key_wheel_position ]
filter: bpm > 118 & bpm < 126 & rating >= 3
sort:   similarity_to(current) DESC
```

...rendered by the same tree, navigated by the same folds and motions, and reached by the same `/` and `:`. That is the
whole ambition of the librarian and DJ uses, expressed in machinery that already exists.

**So the design rule is this: when a new capability is proposed, first try to express it as a field, a predicate, an
event, or a view spec. Only when all four genuinely fail should it become a subsystem.** Every time this project has
followed that rule the result was small; the ratings, history and catalog work are the evidence.

---

## 5. Buffers, not dialogs

As the interactions get richer, the question of _where_ complex editing happens becomes the important UI question. The
answer emerging from use is the emacs/nvim one: **structured information is edited in a buffer, not in a form.**

Tag editing is the first real case, and dired is the model — the rows are the data, editing the text _is_ the edit, and
writing the buffer commits it. Complex filters and, later, scripts are the second case: a small amount of editing at the
`:` line is right, and past that the work belongs in a real editor.

Which means shelling out to `$EDITOR` — realistically nvim — and defining the contract between the two tools. That
contract is the design problem, and it is mostly about where validation and atomicity live:

- **What crosses is a format, not a screen.** gmuse writes a buffer, the editor edits text, gmuse parses it back at the
  boundary and reports every error rather than the first — the rule already used for the config file.
- **The edit is a transaction.** Either the whole buffer applies or none of it does, and it must be undoable after the
  fact. This is where the DuckLake note in `library-sync-design.md` §9 earns its place: a bulk retag committed as one
  snapshot, revertible if the regex was wrong, is better than writing inverse operations for every edit.
- **A plugin on the nvim side is allowed to be part of the design**, but the plain-text path must work without it. A
  format only one plugin can read is a format that cannot be inspected when something goes wrong.
- **The emacs ecosystem has decades of prior art here** and should be read for it, in the same spirit
  `docs/inspiration/` is already read: for interaction and architecture, never for storage or the audio path.

The unifying idea is worth stating on its own: **gmuse is becoming an editor whose buffer happens to be a library.** The
folds, motions, registers and undo are already there. Buffer-based editing is the same thought applied to metadata.

---

## 6. One library, several machines, no reliable link

`library-sync-design.md` is the design; what belongs here is why it is a _vision_ document's problem at all.

The library must be the same library on the laptop, the desktop, the gaming PC and — the hard one — a locked-down work
machine that will never speak to the NAS. That last case is not an edge case to degrade toward. **It is the design
target**, and it is what forces every good decision in the sync document: identity that needs no coordination
(audio-stream hash), history as an append-only event log that union-merges, a bundle on removable media that is
simultaneously the sync unit, the backup and a working library.

Two rules from that document deserve repeating here because violating them is how a personal library quietly loses data:

- **Path is local state, never catalog.** It feels like a property of the track; it is a property of _a device's copy_.
- **`play_count` is never a stored number.** It is an aggregate over events. Counters do not survive disconnection, and
  the loss is silent.

And one consequence for the interface: **availability is three states, not two.** Local, Remote, Absent. A device
holding two percent of the catalog should still show the whole catalog, dimmed — because the librarian work happens
whether or not the audio is on this machine.

---

## 7. The boundaries that will be asked to move

The brief's 2026-08-29 amendment set the standard for this: a boundary may move to a **principled** place, argued and
recorded, and may not erode by increments. Judged by that standard, here is where the pressure is, honestly.

**Tag editing** — the brief says reading only. The librarian use makes editing central, and there is no coherent version
of this project in which correcting a genre requires leaving it. The principled version of the move is: **gmuse edits
tags through a buffer and an event log, and never becomes a tag _guesser_.** No online lookup deciding what a track is,
no automatic rewriting, nothing that changes files because a server said so. What is out stays out; what comes in is
_my_ edit, expressed as data, revertible.

**Two decks and a mixer** — this is the one that touches an INVARIANT. §3's three-thread architecture assumes one
decoder feeding one ring feeding one callback. Two decks with per-channel EQ, filter, faders and varispeed is a
different shape: N decoders, a summing stage, and DSP that must run under exactly the same real-time discipline. It is
not an extension of §3, it is a revision of it, and it should be treated that way when the time comes — designed as a
whole, recorded as a decision, and _measured_, because the CPU target in CLAUDE.md exists for a reason and adding
per-band filters to two streams is where it would quietly be lost. Varispeed in particular is not a knob: it is the
resampler, already the hardest thing in the audio path, now driven continuously.

**A mix is data** — the tracker-shaped half is where this design gets interesting rather than merely large. A recorded
transition is a sequence of timestamped control changes; that is an event log, which this project already knows how to
store, merge and replay. "These two mix well" is likewise an event that aggregates into a field. Approached this way,
most of the DJ ambition lands in machinery §4 already describes, and only the audio graph is genuinely new.

**Waveforms** — the notes name this friction directly, and the tension is real: "no visualizers" is a rule about not
building something to stare at, and a waveform overview during a transition is a navigational aid. The interesting
answer is not to relax the rule but to take the Elektron thought seriously and ask what a keyboard-shaped, non-pictorial
representation of "where am I in this track and what is coming" would be. If that genuinely fails, the move is arguable
— with the same standard applied, and recorded.

**Scripting** — a small expression language at the `:` line is already here and is right. The line to hold is that gmuse
does not grow an embedded programming language; past a certain complexity the work moves to §5's editor and comes back
as data.

**Streaming services, album art, lyrics, podcasts** — unchanged, permanently. The line drawn in 2026-08-29 was between
_my files_ and _somebody's service_, and none of these cross it in the right direction.

---

## 8. How we will know it is working

The project has an unusually strong testing culture already, and the reason to write it down in a vision document is
that the new domains each need their own version of it. The lessons that have actually been paid for:

- **Measure inside the running player, not in a throughput loop.** The resampler benchmark understated real cost by
  3–5×.
- **Listen for an hour.** M5 and M7 were signed off by listening, with an underrun count and a log to check afterwards.
- **A passing test over a degenerate fixture is indistinguishable from a passing test over a correct one.** Learned
  twice; the mutation runs found six survivors that were all the same fixture mistake.
- **A test whose expectation is built from the code under test proves nothing.**
- **The log records what happened, not what was asked for.**

Applied forward, each ambition has an acid test that is not a unit test:

| Ambition          | The test that actually decides it                                                                                |
| ----------------- | ---------------------------------------------------------------------------------------------------------------- |
| Listening         | An evening of real use with nothing to write down afterwards                                                     |
| Tag editing       | A bulk retag, applied from a buffer, reverted cleanly, with the event log explaining both                        |
| Sync              | A week on the airgapped machine, then a bundle home, and every play, rating and edit present exactly once        |
| Discovery / views | A question I could not previously ask, answered by a view spec and no new code                                   |
| DJ                | A recorded transition replayed from metadata that sounds like the one I performed — and the CPU target still met |

And the standing measurement targets do not move: steady-state playback under ~1% of one core, at or below `afplay`,
with zero underruns, verified against `ffprobe` durations.

---

## 9. The decisions that are hard to reverse

Everything in this document is negotiable except the things that invalidate stored data or invalidate the model. Those
should be got right before there is much to lose:

1. **`track_id` is the audio-stream hash.** Every event ever recorded is keyed by it. Changing it later throws away the
   history.
2. **Events carry `event_id`, `kind` and `ts_seq` from the first one written**, even while only `played` exists.
   `kind = tagged` must be reserved now, or §7's tag editing is a redesign rather than a projection.
3. **Local state is a separate table, joined — never a column on the catalog.** Path in the catalog is what makes
   bundles non-portable.
4. **`Availability` is three states in the expression language**, not a boolean.
5. **The projection model holds.** `Store` → membership → view. Anything that computes pools or lists behind it breaks
   catalogs, filters and next-track resolution at once.

None of the five names a storage engine, a library, or a UI. That is deliberate, and it is the shape every decision in
this document should aim for: **choose the thing that is expensive to change, and leave the rest cheap.**

---

## 10. The shape of the thing, in one paragraph

An owned collection of files, indexed once and projected many ways; a keyboard interface that treats the library as a
buffer to be navigated, folded, filtered and edited with the vim vocabulary; knowledge about the music accumulating as
fields and events rather than as features; the whole thing converging across machines that cannot talk to each other, by
a merge that needs no coordination; and — eventually — an instrument on top of it that treats mixing as something
programmed and replayed rather than performed. Off screen most of the time. Under one percent of a core. Mine.
