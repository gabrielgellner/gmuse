# DESIGN.md — a terminal music player and library manager in Rust

Working title: `gmuse`.

This document is the project brief. It is written to be read by an implementing agent at the start of a session and to
be kept up to date as decisions are made. Sections marked **INVARIANT** are not to be revised without an explicit
decision recorded in the Decision Log at the bottom.

---

## 1. Goal and scope

Build a terminal music player for a local library that reproduces the cmus _interaction model_ — its views, its
next-track semantics, its command and keybinding layer — in idiomatic Rust, with a pure-Rust audio path.

This is a **reimplementation, not a port**. Do not translate cmus's C. Its event loop, hand-rolled hash tables, and UI
layer do not map onto Rust idiom and translating them will produce worse code than designing fresh against the same
behavioural spec. Read cmus's source to answer "what does it do in this case," never to answer "how should this be
structured."

### What the interface is for

**gmuse is a way to interact with music, not a thing to look at.** It is driven from the keyboard, it is off screen most
of the time, and its job is to make the next action cheap — not to fill a terminal with something worth watching.

This is a scope rule, not a mood. It is why there are no visualizers and no album art, and it is also why a remote
socket, media keys and session resumption matter more than they look: the interface has to be usable when it is not in
front of you. When a feature is arguable, the question is whether it makes acting faster or only makes looking nicer.

### In scope

- Local filesystem library: scan, tag-index, persist, incremental update
- The four cmus views (library tree, sorted library, playlist, queue) plus filters and the guide
- Gapless playback of MP3, Ogg Vorbis, FLAC, WAV, AAC/M4A, ALAC
- Sample-accurate seeking
- cmus's next-track semantics: queue precedence, `aaa_mode`, shuffle, repeat, continue
- The filter mini-language and the display format-string language
- Configurable keybindings via an rc file, `:bind` at runtime
- A remote control socket (`cmus-remote` equivalent) for media keys and status bars

### Decided out — revisit deliberately, not by accident

- Album art in any form (no sixel, no ueberzug, no kitty graphics, no `image`)
- Visualizers, spectrum displays
- Lyrics, synced or otherwise
- Podcasts, RSS
- Commercial and scraped streaming services: no Spotify, no YouTube, no yt-dlp
- Any async runtime. No `tokio`, no `async-std`. Threads and channels.

These are decisions, not an oversight, and they are the reason comparable projects carry 40+ dependencies. **A decision
can be revisited.** What this list asks for is that reopening one is an explicit act with a Decision Log entry behind it
— not something that happens because a crate made it easy one afternoon.

The async rule is different in kind from the rest: it is a **code-style invariant** (§12), not a feature decision, and
it does not move.

### Where the scope is meant to grow — decided 2026-09-06

**This is a library manager as much as a player, and its scope is explorative.** An earlier version of this document
named scope creep as the project's failure mode and told every reader to refuse on sight. That rule cost more than it
saved: it turned "does this belong here" into a reflex rather than a question, and the questions it was suppressing —
what is a claim, what is a copy, what is derived from what — turned out to be the substance of the project.

The named directions, so they are on the record rather than argued from scratch each time:

- **Musical key on a recording**, alongside `bpm`. The tags already carry it — 94% of the dance library — and it is a
  fact about the music like any other.
- **Harmonic and tempo-aware recommendation** — circle-of-fifths compatibility, BPM-adjacent suggestion. Wanted.
- **Embeddings and audio fingerprinting**, feeding a suggestion engine, and **held by the library model** rather than
  bolted beside it. The model is the thing being hardened so that this can rest on it.

Two constraints survive and they are the ones that were ever load-bearing:

1. **Local first, always.** These are computed here, from files you hold. §1's "no recommendations from anyone else's
   server" is untouched — the objection was ever to a service deciding what you hear, never to your own library being
   able to answer the question.
2. **Every dependency is recorded in the Decision Log with a rationale** (§12), which is the mechanism that actually
   kept the tree small. That did the work the slogan was taking credit for.

### Local first, and one's own server — amended 2026-08-29

The original rule was "no network sources", full stop. It has been **narrowed**: what stays out is _services_ — things
with accounts, catalogues, and an interest in what you play. What is now allowed is **your own library on your own
hardware**, reached over the network: browsing, streaming and syncing against a personal media server (Plex being the
case in hand).

The distinction is the point. A service decides what you hear; a NAS holds what you already own. The second is the same
music this player was built for, on the other side of a socket.

Three things hold regardless, and they are what keep this from being the slope §1 exists to prevent:

1. **Local first is not negotiable.** Every feature must work with no network at all. Remote sources are additive; a
   disconnected gmuse is a complete music player, not a degraded one.
2. **No account, no catalogue, no recommendations from anyone else's server.**
3. **Anything networked is opt-in and behind a feature flag**, so the default build stays what it is today, and an HTTP
   client is a dependency like any other — recorded in the Decision Log with a rationale.

### Tag editing — admitted 2026-09-01

The original rule was "tag _editing_ (reading only)", and it is **withdrawn**. Writing tags is in scope.

The rule was written when the open question was whether a terminal player was worth building at all. Under that
question, every excluded feature was cheap to exclude, because the thing being protected was a proof of concept with no
demonstrated shape. That is no longer the question. The shape is settled, and a scope rule that was load-bearing for a
POC is not automatically load-bearing for the project it became — the focus has already been paid for and the return
collected.

What it is _not_ is an argument that scope creep stopped being the failure mode. §1 keeps its other exclusions, and the
2026-08-29 standard still governs: **a boundary may move to a principled place, and may not erode by increments.** So:

1. **The library is still read at scan time.** Editing is a distinct, explicit act on a selection — never a side effect
   of playing, scanning or importing.
2. **Bulk retagging belongs to `tools/`, not the player.** The genre pipeline is the pattern: a script proposes, a
   person reviews a file, a second pass writes. A player that could rewrite a library in a keystroke is a different
   program.
3. **An edit is undoable.** Whether that is #41's `kind = tagged` event or a snapshot is still open, but "no undo" is
   not an acceptable answer for an operation that mutates files the player does not own.

The immediate case is multi-artist credits (#60): 367 tracks whose `artist` reads as joined, of which the tags already
settle 332 and **three strings** need a person. That case argues for `tools/` normalisation rather than for an editor —
which is why this entry admits tag editing to scope without also committing to a tag editor in the UI. #41 remains the
place where that is decided.

See the Decision Log entry of 2026-08-29 for the reasoning, and the tracking issues for the work.

---

## 2. Dependencies

Rust edition 2024, MSRV 1.85 (set by Symphonia 0.6).

```toml
[dependencies]
ratatui           = "0.29"   # UI
crossterm         = "0.28"   # terminal backend, input events

cpal              = "0.15"   # audio device + callback
symphonia         = { version = "0.6", features = ["mp3", "vorbis", "flac", "isomp4", "aac", "alac"] }
rubato            = "0.16"   # resampling to device rate
rtrb              = "0.3"    # lock-free SPSC ring buffer, decoder -> callback

lofty             = "0.22"   # tag reading, uniform across ID3v2/Vorbis/MP4/APE
walkdir           = "2"      # library scan (swap for jwalk if scan is slow)
nucleo-matcher    = "0.3"    # fuzzy search — in the tree since 2026-09-05, for the picker (#54)

serde             = { version = "1", features = ["derive"] }
postcard          = "1"      # library cache serialization
crossbeam-channel = "0.5"    # control messages between threads
thiserror         = "2"
```

Verify current versions with `cargo add` rather than trusting these pins.

### Dependency notes

- **Symphonia's `mp3` feature is not on by default.** Neither are `isomp4`, `aac`, or `alac`. `vorbis`, `flac`, `wav`,
  `ogg`, and `pcm` are. Enable explicitly as above rather than using `all` — `all` pulls in demuxers we have no use for.
- **Opus is not supported natively by Symphonia.** If `.opus` support is wanted later, add `symphonia-adapter-libopus`
  and register its decoder in a custom `CodecRegistry`. This introduces a C dependency (libopus); it is the only one in
  the project. Treat it as an opt-in cargo feature, off by default.
- **Do not add `rodio`.** It wraps cpal in a `Source` iterator abstraction that is convenient for "play this file" and
  actively obstructive for gapless transitions, sample-accurate seeking, and controlling buffer depth — all of which are
  core requirements here. We drive cpal directly.
- **Do not add a component framework over ratatui** (tui-realm or similar). Use ratatui's widgets directly and write the
  one custom widget we need (the library tree).
- Symphonia's SIMD optimizations pull in `rustfft`. Leave them off initially; measure before enabling.

---

## 3. Thread architecture — **INVARIANT**

Three threads. The split is not negotiable; it is what determines whether playback is glitch-free.

```
  UI thread                 decoder thread                audio callback (cpal, RT)
  ─────────                 ──────────────                ─────────────────────────
  ratatui render      cmd   symphonia demux+decode        drain rtrb consumer
  crossterm input    ───▶   rubato resample         ───▶  write to device buffer
  owns Store/views          push frames to rtrb           update atomic frame counter
  reads atomics      ◀───   pre-decode next track   ◀───  (nothing else, ever)
                     evt
```

### A fourth thread for the socket — amended 2026-09-01

The rule above says three, and M9 adds a fourth: a listener that blocks on `accept` and posts typed commands to the UI
thread. The amendment is narrow and the wording of the rule is what makes it necessary — **"the split is not negotiable;
it is what determines whether playback is glitch-free"** names its own scope. The three threads that carry audio are
unchanged: UI to decoder to callback, the same channels, the same ring buffer.

What is added is _off_ that path entirely. The listener never touches the ring, the decoder or the callback. It reads a
line, parses it, and either posts a `Command` to the UI thread or answers from a snapshot the UI publishes. It cannot
delay a decode and it cannot delay the callback, because it shares nothing with either.

Blocking on `accept` is precisely the work that must not live on a thread with a deadline — putting it on the UI thread
would mean a non-blocking accept, partial-read state, and a client able to interleave with rendering. So the fourth
thread exists to _protect_ the three rather than to dilute them.

The callback rules below are untouched and remain absolute.

### The count is retired, the property is not — revised 2026-09-11

The rule above says three, the amendment above says four, and
[`gmuse-engine-architecture.md`](gmuse-engine-architecture.md) says roughly thirteen roles across two processes. The UI
thread leaves the process and becomes the engine's **control thread**; the TUI attaches over a socket like every other
view. Recorded in §13 (2026-09-11), which is what authorises it.

What the count was shorthand for is unchanged and now stated directly: **nothing with a deadline shares a thread with
work that can stall.** Every audio-carrying path still has the shape it has here — something decodes off the RT thread,
hands frames over an SPSC ring, the callback drains it — and **ordinary playback still streams, one song at a time,
exactly as described above.** Full-decode decks are an additional path for DJ mode, not a replacement for this one.

The trade is that three threads could be verified by reading the code and a property cannot, so the tests named in that
document are the enforcement. The callback rules below are untouched and remain absolute.

### Rules for the audio callback — **INVARIANT**

The cpal callback runs on a real-time thread. Inside it, the following are forbidden without exception:

- Heap allocation or deallocation (no `Vec::push`, no `Box`, no `String`)
- Locking a `Mutex` or `RwLock`
- Any syscall: no file I/O, no logging, no `println!`
- Panicking (a panic across the FFI boundary is UB)
- Unbounded loops

The callback may only: read from the `rtrb::Consumer`, write samples to the output slice, and update
`AtomicU64`/`AtomicBool` state. If the ring buffer underruns, write silence and set an atomic underrun flag — do not
block, do not report from inside the callback.

Violating this produces the clicks, pops, and dropouts that distinguish an amateur player from a usable one. It is the
single most common failure mode in this class of project and it will not show up in tests — only in listening.

### Communication

- UI → decoder: `crossbeam_channel::Sender<PlayerCmd>`, bounded, non-blocking send from the UI (drop or coalesce on full
  rather than blocking the render loop)
- decoder → UI: `Sender<PlayerEvent>` for track-finished, decode-error, device-lost
- decoder → callback: `rtrb` SPSC ring buffer of interleaved `f32` frames, sized for ~500–1000ms of audio at device rate
- callback → everyone: atomics only (`frames_played: AtomicU64`, `underruns: AtomicU32`, `state: AtomicU8`)

Position display is derived from `frames_played` plus a seek epoch, not from a timer. Bump an epoch counter on seek so
the UI can discard stale positions.

---

## 4. Data model — **INVARIANT**

One track store; every view is a projection over it. Track identity is an integer index into an arena, never a pointer
or `Rc`.

```rust
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct TrackId(u32);

pub struct Track {
    pub path: PathBuf,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub albumartist: Option<String>,
    pub title: Option<String>,
    pub tracknumber: Option<u32>,
    pub discnumber: Option<u32>,
    pub date: Option<i32>,
    pub genre: Option<String>,
    pub duration: Duration,
    pub bitrate: Option<u32>,
    pub samplerate: Option<u32>,
    pub channels: Option<u8>,
    pub mtime: i64,          // for incremental rescan
    pub replaygain: Option<ReplayGain>,
}

pub struct Store {
    tracks: Vec<Track>,                    // TrackId(n) == tracks[n]
    by_path: HashMap<PathBuf, TrackId>,
}
```

`TrackId` indices keep the whole model `Send`, make views cheap to rebuild, and let the same track appear in the
library, several playlists, and the queue simultaneously as the same `u32`.

Views:

```rust
pub struct Library {                       // views 1 and 2, same membership
    members: HashSet<TrackId>,
    tree: BTreeMap<ArtistKey, BTreeMap<AlbumKey, Vec<TrackId>>>,  // view 1
    sorted: Vec<TrackId>,                                          // view 2
}

pub struct Playlist { entries: Vec<TrackId> }        // ordered, duplicates allowed
pub struct Queue    { entries: VecDeque<TrackId> }   // consumed on pop
```

`ArtistKey`/`AlbumKey` are normalised sort keys (case-folded, leading article stripped per a `smart_artist_sort`
setting), not the raw display strings. Keep display strings on the `Track`.

### Catalogs — planned, and why the model already allows it

Several separate bodies of music (general, electronica, classical, jazz), with one, a subset, or all of them active at a
time. **Implemented 2026-08-28** — `src/catalog.rs`.

This is recorded here rather than only in the issue because it constrains what §4 and §5 may become, and both are
INVARIANT.

`members` above is the whole reason it fits: `Store` holds every track ever scanned and `Library` is a projection over a
_subset_ of it. A catalog is therefore a membership rule, and:

- **`next()` needs no change.** §5 builds its pool from the library, so confining the library confines shuffle,
  `aaa_mode` and repeat with it. Anything that starts computing pools from the `Store` instead would break this, and
  should be treated as a mistake.
- **Playlists and the queue cut across catalogs for free**, because they hold `TrackId`s straight into the `Store`. They
  must **not** be filtered by the active set — a playlist is an explicit choice, and crossing themes is the point.
- **One scan, one cache.** Catalogs partition what is _shown_, never what is indexed.

**Membership is `collection` and `where`** (§7), settled 2026-08-30 and narrowed 2026-09-05, either of which may be
absent and which combine with **and** when both are given; a catalog with neither holds everything. A catalog _is_ a
named, persistent filter, which is what cmus's `fset` already is.

**No membership decision reads a path** — INVARIANT, see the 2026-09-05 entry in §13. `roots` was the third key and is
gone: membership by folder meant that moving a file changed what it belonged to, and that the same config selected
nothing on a second machine. Where the files are is `library`, which is a different question and has one answer.

Active catalogs **union**; a transient filter **intersects**:

```text
members = (union of the active catalogs) ∩ (the transient filter, if any)
```

Two combining rules, each following from what the thing is for — "which bodies of music am I in" is a selection, "what
am I looking for" is a restriction. This is also what `:catalog folk pop` already does, and it must keep doing it.

Removal from the library is a tombstone or a rebuild, never an index shift — shifting invalidates every `TrackId` held
elsewhere.

---

## 5. Next-track resolution — **INVARIANT, and the highest-value tests in the project**

This function is what makes cmus feel correct and it is where every "why did it play _that_" bug lives. Implement it as
a pure function over explicit state. No I/O, no mutation, no reaching into the player.

```rust
pub struct NextInput<'a> {
    pub queue: &'a Queue,
    pub active: ActiveSource,        // Library | Playlist
    pub library: &'a Library,
    pub playlist: &'a Playlist,
    pub current: Option<TrackId>,
    pub aaa_mode: AaaMode,           // All | Artist | Album
    pub shuffle: bool,
    pub repeat: bool,
    pub repeat_current: bool,
    pub rng_state: u64,
}

pub fn next(input: NextInput<'_>) -> Option<TrackId>;
pub fn prev(input: NextInput<'_>) -> Option<TrackId>;
```

Resolution order:

1. `repeat_current` → return `current`
2. Queue non-empty → pop front. **The queue wins unconditionally**, regardless of `aaa_mode`, shuffle, or active source.
   Playing a queued track does not change the library cursor.
3. Otherwise build the candidate pool from the active source, restricted by `aaa_mode` when the source is the library:
   `All` = whole library, `Artist` = tracks by the current track's artist, `Album` = tracks on the current track's
   album.
4. Within the pool: if `shuffle`, pick unplayed-at-random (track a played-set per pool so shuffle doesn't repeat until
   exhausted); else take the successor in pool order.
5. Pool exhausted: if `repeat`, wrap to pool start (and clear the shuffle played-set); else `None` → stop.

Table-test this exhaustively. Roughly 150 lines of implementation and it should have several hundred lines of tests
covering: empty library, single track, queue-during-shuffle, `aaa_mode` transitions when the current track changes
artist, shuffle exhaustion with and without repeat, prev at pool start.

---

## 6. Command and keybinding layer

cmus's `:bind <context> <key> <command>`, its rc file, and its `:` command line are one mechanism. Model them as one
typed enum and parse everything into it. This is the "parse, don't validate" boundary of the application: strings become
`Command` at the edge and nothing downstream ever handles a raw string.

```rust
pub enum Command {
    // navigation
    WinUp, WinDown, WinPageUp, WinPageDown, WinTop, WinBottom,
    WinActivate, WinToggle, WinRemove, WinUpdate,
    // playback
    PlayerPlay, PlayerPause, PlayerStop, PlayerNext, PlayerPrev,
    Seek(SeekSpec),                       // relative or absolute, +/- seconds
    Vol(VolSpec),
    // library
    Add { target: AddTarget, path: PathBuf },
    Save { path: Option<PathBuf> },
    Load { path: PathBuf },
    Update,
    // state
    Set { key: SettingKey, value: SettingValue },
    Toggle(SettingKey),
    Bind { ctx: Context, key: KeyEvent, cmd: Box<Command> },
    Unbind { ctx: Context, key: KeyEvent },
    // views
    View(ViewId),
    Filter(Option<FilterExpr>),
    Search { pattern: String, dir: SearchDir },
    Quit,
}

pub enum Context { Common, Library, SortedLibrary, Playlist, Queue, Filters, Guide, Transport }

pub type Bindings = HashMap<(Context, KeyEvent), Command>;
```

Lookup falls back: try `(current_context, key)`, then `(Context::Common, key)`. Execution happens in one
`fn execute(&mut self, cmd: Command)` — a single place where all state mutation lives. View 7 is then just a rendering
of `Bindings` and the settings map; it costs nothing extra.

Default bindings to ship (matching cmus): `1`–`7` views; `j`/`k`/arrows, `g`/`G`, `Ctrl-f`/`Ctrl-b` navigation; `Tab`
pane switch; `Space` expand; `x`/`c`/`v`/`b`/`z` transport; `←`/`→` ±10s, `<`/`>` ±60s; `-`/`+` volume; `s`/`r`/`C`/`m`
mode toggles; `a`/`y`/`e`/`E` add-to targets; `D` remove; `u` update; `/`,`?`,`n`,`N` search; `:` command line; `q`
quit.

---

## 7. The two mini-languages

Both are small. Hand-rolled recursive descent or `winnow` — do not pull in a parser-generator.

### Filter expressions

Settled 2026-08-30 — see the Decision Log for the reasoning and the evidence.

```text
genre="jazz"|genre="blues"
artist="*ferrell*"&year>2020
genre="*electronic*"&!genre="*christmas*"&bitrate>256
year>=1990&year<2000&(genre="folk"|genre="country")
filename="*.opus"
any="*live*"
```

**Terse, deliberately.** This is a pointed language typed at a `:` prompt, not prose. `&` `|` `!` and parentheses; `=`
`!=` for strings; `=` `!=` `<` `<=` `>` `>=` for numbers.

**Strings match by glob, and `*` is the only metacharacter.** One operator covers every positional case, and the
wildcard is visible where it applies rather than spread across four keywords:

| pattern              | matches      |
| -------------------- | ------------ |
| `artist="Ferrell"`   | exactly that |
| `artist="Ferrell*"`  | starts with  |
| `artist="*Ferrell"`  | ends with    |
| `artist="*Ferrell*"` | contains     |

`?` is **not** a metacharacter — glob's "exactly one character" is never what anyone wants from a music search, while a
question mark in a title is real. `\*` is a literal asterisk.

**Smart-case**: insensitive unless the pattern contains a capital, as in vim and ripgrep. `genre="jazz"` finds "Jazz";
`artist="Burial"` is precise.

A trailing `?` on a field asks whether the track has it at all: `genre?` has one, `!genre?` has none. An empty or
whitespace tag counts as missing.

**No regex**, and not by omission — see the Decision Log. It can arrive later as a separate opt-in operator (`=~`) if
globs prove insufficient, so the hazardous behaviour would be something asked for rather than the default.

**No bare words.** `ferrell` alone is an error; `any="*ferrell*"` is how you search every text field. `any` is a real
field that composes with everything else, so the convenience costs no implicitness.

Fields are **the same names the format language uses** (§7 below, `format::Field`), with a type attached — one
vocabulary for both mini-languages, and one place to add `play_count` when it exists. Strings: `artist`, `albumartist`,
`album`, `title`, `genre`, `filename`, `path`, `any`. Numbers: `track`, `disc`, `year`, `duration`, `bitrate`,
`samplerate`, `channels`.

```rust
pub enum FilterExpr {
    And(Box<FilterExpr>, Box<FilterExpr>),
    Or(Box<FilterExpr>, Box<FilterExpr>),
    Not(Box<FilterExpr>),
    Cmp { field: Field, op: CmpOp, value: Value },
}

impl FilterExpr {
    pub fn compile(&self) -> impl Fn(&Track) -> bool + '_;
}
```

**Compile once to a closure**, do not re-interpret the AST per track. Measured: a whole-library pass is 0.92ms at 20k
tracks in the worst case, so there is no index to be won — see the Decision Log entry on SQLite.

**A filter confines playback, not merely display.** §4 makes `Library` a projection and §5 builds pools from it, so
anything that narrows the library narrows what shuffle picks from. That falls out rather than being built, and the rule
is "you hear what you can see". Playlists and the queue are **not** filtered: they hold `TrackId`s straight into the
`Store`, and crossing boundaries is the point of them.

### Sort keys and smart playlists

Settled 2026-08-30 — see the Decision Log.

A **smart playlist** is a named filter expression with an order, and it is a **play source**, not a narrowing:

```toml
[playlists.fresh]
where = "days_since_added<30"
sort = "-added"
```

`:playlist <name>` fills view 3 from the rule; `:freeze` turns the loaded one into an ordinary, editable playlist.

The distinction that makes this coherent is _not_ persistence — catalogs are saved too. It is exclusivity. A catalog
hides everything outside itself; a smart playlist sits alongside the library and is one more place to play from. A saved
expression that narrows already exists and is spelled `[catalogs.x] where = "…"`, so making smart playlists narrow would
be two mechanisms for one idea. Membership is evaluated over the `Store`, not the active `Library`, because §4 already
says a playlist must not be filtered by the active set.

It is a **live query**, re-evaluated when anything it depends on moves — a play, a rating, a rescan. That is what makes
it not editable: an exception list bolted onto a rule is how these become unexplainable, so `:freeze` drops the rule
instead.

**Sort** is a comma-separated list of field names, applied left to right, `-` for descending, from the same vocabulary
as the filter and format languages plus `random`. Not foobar2000's arbitrary format pattern, which sorts numbers
lexically unless padded by hand, and not iTunes' fixed menu, which does not compose. Missing values sort **last in both
directions**; ties break by path, so the order is total and stable.

Dates are the one place the two languages diverge: a filter says `days_since_added`, a sort says `added`. `days_since`
inverts in an order — ascending would mean newest first — so `-days_since_added` written for "newest" gives the oldest
**silently**, with a plausible-looking list and no error. Each language takes the spelling that reads in it and rejects
the other by name.

### Format strings

`{artist} — {title}`, with `{field:spec}` sizing and `[…]` groups that vanish when a field inside them is missing. A
spec is `[>|0]? min? ( '.' max )?` — `{track:02}`, `{artist:20}`, `{duration:>7}`, `{title:.40}`, `{title:8.8}`. A
minimum pads and a maximum truncates; the value is padded first and cut second, so **the maximum always wins**. Parse to
a `Vec<Atom>` at config-load time and render against a `&Track`. Never re-parse per frame — the now-playing line renders
at whatever the UI tick rate is.

Named placeholders rather than the `%a` this section originally specified — see the Decision Log entry of 2026-08-30. A
literal delimiter is backslash-escaped (`\[`, `\{`), not doubled.

---

## 8. Module layout

```
src/
  main.rs           argument parsing, terminal setup/teardown, signal handling
  app.rs            App state, execute(Command), the one mutation point
  store.rs          Track, TrackId, Store
  library.rs        Library, tree/sorted projections, sort keys
  playlist.rs       Playlist, Queue, .pl load/save
  next.rs           next()/prev() — pure, heavily tested
  scan.rs           walkdir + lofty, incremental rescan by mtime
  cache.rs          postcard serialization of Store + Library
  filter/
    mod.rs          FilterExpr, compile()
    parse.rs
  format/
    mod.rs          FormatAtom, render()
    parse.rs
  cmd/
    mod.rs          Command enum
    parse.rs        rc file + ':' line -> Command
    bind.rs         Bindings, default table, lookup with Common fallback
  audio/
    mod.rs          PlayerHandle: the public API the UI talks to
    device.rs       cpal device selection, stream build, hotplug recovery
    decode.rs       decoder thread: symphonia -> rubato -> rtrb
    ring.rs         rtrb wiring, buffer sizing
    gapless.rs      next-track pre-decode, transition
  ui/
    mod.rs          top-level layout, view dispatch, event loop
    tree.rs         custom widget: the artist/album tree (view 1)
    list.rs         shared track-list rendering
    statusline.rs   format-string rendering, mode indicators
    cmdline.rs      ':' input, search input
    settings.rs     view 7 — renders Bindings + settings
  remote.rs         unix socket server + `--remote` client; one line in, one line out
```

---

## 9. Milestones

Build in this order. Each is independently verifiable; do not start the next until the current one works.

**M1 — sound out of the speakers.** ✅ _done 2026-08-27 —_ _verified: `01 Dance With Me.mp3` played 31.686s vs ffprobe's
31.686009s, 0 underruns._ Hardcode a file path, decode with Symphonia, push through rtrb to a cpal callback, play to
completion. No UI, no library. Verify: no clicks, no dropouts, over a full album, with the underrun counter at zero.

**M2 — the audio API.** ✅ _done 2026-08-27._ Wrap M1 in `PlayerHandle` with play/pause/stop/seek/ volume and a
`PlayerEvent` stream. Drive it from a `main` that reads stdin lines. Verify: seek is sample-accurate and the position
display doesn't jump on seek.

**M3 — store and scan.** ✅ _done 2026-08-27._ `walkdir` + `lofty` into `Store`, postcard cache, incremental rescan by
mtime. No UI. Verify against a real library — measure scan time and memory on ~20k tracks.

**M4 — the views.** ✅ _done 2026-08-27._ ratatui, views 1–5, navigation, `Tab`, `Space` expand. Read-only, no playback
wired in. Verify: tree renders correctly for compilations, multi-disc albums, and missing tags.

**M5 — next() and playback wiring.** ✅ _done 2026-08-28._ Implement `next.rs` with its full test table, wire views to
`PlayerHandle`. Verify: every row in the test table, then listen for an hour. The hour was logged — see the Decision Log
entry for 2026-08-28.

**M6 — command layer.** ✅ _done 2026-08-27._ `Command`, rc parsing, `:` line, `Bindings`, view 7. Verify: every default
binding, plus rebinding round-trips through the rc file.

**M7 — gapless.** ✅ _done 2026-08-28._ Pre-decode next track into a second buffer, transition without draining to
silence. Verify on a continuous-mix album — this is the acid test. Verified on Anderson .Paak's _Malibu_; see the
Decision Log.

**M8 — filters and format strings.** ✅ _done 2026-08-30._ Format strings, the filter language, `:filter`, catalogs
defined by expressions, and view 6.

_Note: the resampler was pulled forward out of milestone order (done 2026-08-27), because Opus playback is impossible
without it on a 44.1k-only output device._

**M9 — remote socket, MPRIS if wanted.** ✅ _socket done 2026-09-01._ `remote.rs`, `gmuse --remote`, and a fourth thread
amended into §3. MPRIS not done and not currently wanted.

---

## 10. Testing strategy

- `next.rs`: exhaustive table tests. This is where the bugs are.
- Filter and format parsers: round-trip and error-case tests.
- Scan: fixture directory with deliberately awful tags — missing artist, unicode, embedded nulls, multi-value fields,
  wildly wrong durations.
- Audio: assert the callback never allocates. A `#[global_allocator]` wrapper that panics when a thread-local "in
  callback" flag is set catches this in integration tests and is worth the 30 lines.
- Underrun counter exposed as a debug field in the UI during development.
- Golden-file tests for view rendering via `ratatui::backend::TestBackend`.

---

## 11. Known hard parts

Ranked by how much time they will actually take:

1. **Gapless.** ✅ Done 2026-08-28. The encoder delay/padding half turned out to be free — Symphonia 0.6 applies the
   trim inside the decoder, and the packet's `trim_start`/`trim_end` are informational (see the Decision Log). What was
   left was the splice itself, and the fiddly part was not the transition but the _resampler_: the filter has to be fed
   continuously across the join rather than flushed and rebuilt.
2. **Device hotplug.** USB DAC disappears, the cpal stream errors, and the app must rebuild the stream at the same
   playback position without dropping state. Test by physically unplugging.
3. **Seek coherence.** Symphonia's `seek()` is the easy half. Flushing the ring buffer, resetting resampler state,
   invalidating in-flight decoded frames, and keeping the UI position honest across all of it is the other half. Use a
   seek epoch counter.
4. **The tree view for real libraries.** Compilations, `albumartist` vs `artist`, multi-disc, "Various Artists", tracks
   with no tags at all. Decide the `albumartist` fallback rule early and write it down here.
5. **Resampling policy.** Device runs at 48k, half the library is 44.1k. Decide: always resample to device rate, or
   reconfigure the device per track (better quality, causes an audible gap — conflicts with gapless). Recommend:
   resample, with a `resample_quality` setting.

---

## 12. Style constraints for the implementing agent

- Prefer explicit typed layers over abstraction. If a trait has one implementor, it is a struct.
- Parse at the boundary; downstream code takes parsed types, never strings.
- No `unsafe` outside `audio/`, and inside it only where cpal's API requires it.
- No async. Threads and channels.
- Errors: `thiserror` enums per module. No `anyhow` in library code; `main` may use it.
- Do not add a dependency without recording it in the Decision Log below with a one-line rationale.
- Do not implement anything from the "out of scope" list in §1, even if it seems easy or a crate makes it trivial. Scope
  creep is the failure mode for this project.

---

## 13. Decision log

Append here as decisions are made. Format: date, decision, rationale.

- **(initial)** Reimplement rather than port cmus. Its C structure does not map onto Rust idiom.
- **(initial)** cpal + Symphonia directly, not rodio. Rodio's `Source` abstraction blocks gapless, precise seek, and
  buffer control.
- **(initial)** `TrackId(u32)` arena indices rather than `Rc<Track>`. Keeps the model `Send`, makes views cheap.
- **(initial)** Opus deferred; requires a C dependency (libopus) via `symphonia-adapter-libopus`. Opt-in cargo feature
  if added.
- **(initial)** No album art, visualizers, lyrics, podcasts, or network sources.

- **2026-08-27** Crate named `gmuse` (the `rcmus` working title is retired).
- **2026-08-27** Dependency pins in §2 were stale; corrected against crates.io: ratatui 0.29→**0.30.2**, crossterm
  0.28→**0.29.0**, cpal 0.15→**0.18.2**, rubato 0.16→**5.0.0**, rtrb 0.3→**0.4.0**, lofty 0.22→**0.25.1**. Symphonia
  0.6.1, nucleo-matcher 0.3.1, postcard 1.1.3, thiserror 2.0.20 as written.
- **2026-08-27** §2's claim that Symphonia's `opt-simd` is what pulls in `rustfft` is **wrong in practice**: `rubato`
  depends on `realfft` → `rustfft` unconditionally, so it is in the tree either way. Disabling Symphonia's defaults
  therefore costs decode SIMD and saves no dependency. Currently still off pending the "measure before enabling" rule in
  §2 — recommend turning it back on, since the stated rationale no longer holds.
- **2026-08-27** rubato 5.x is a full API rewrite (`Async::new_poly` / `new_sinc` + `FixedAsync`, `Fft` + `FixedSync`,
  buffers via the `audioadapter` crate). Nothing like the `SincFixedIn` the design assumes. §11.5's resampling-policy
  decision should be made against the 5.x API.
- **2026-08-27** M1 does **no** resampling: the device is opened at the file's native rate and an unsupported rate is a
  hard error rather than wrong-pitch playback. Deliberately defers §11.5.
- **2026-08-27** Confirmed Symphonia 0.6.1 has no `opus` feature, as §2 states. `symphonia-adapter-libopus` 0.3.0 does
  exist. Note that `Various Artists/YouTube Sessions` in the target library is now 5 `.opus` files, which this player
  cannot play until that opt-in feature lands.
- **2026-08-27** **Opus is now supported and on by default**, reversing the initial "opt-in, off by default" decision —
  the target library contains `.opus` files, so a player that cannot play them is not usable here.
  `symphonia-adapter-libopus` 0.3.0 registered into a custom `CodecRegistry` alongside
  `default::register_enabled_codecs`. Cost is smaller than §13 feared: `opusic-sys` defaults to `bundled`, so libopus is
  compiled from source and **statically linked** — no runtime shared library, no pkg-config, no Homebrew `opus`. The
  only new requirement is the `cmake` binary at build time (installed 2026-08-27). Bindgen is not needed; pre-generated
  bindings ship with the crate. Kept behind a `opus` cargo feature (in `default`) so it can be switched off to get a
  pure-Rust build.
- **2026-08-27** Symphonia's `opt-simd` re-enabled (back to default features plus `aac`/`alac`/`mp3`/`isomp4`), per the
  finding above that `rustfft` arrives via `rubato` regardless.
- **2026-08-27** **§11.5 resolved empirically: always resample to the device rate.** Reconfiguring the device per track
  is not viable on this machine — the default output (a Bluetooth device) advertises only 16 kHz and 44.1 kHz, while
  every Opus file is 48 kHz. Built-in speakers do 44.1/48/88.2/96k. So a fixed device rate plus resampling is the only
  policy that plays the whole library on every output. **This makes the resampler a blocker for Opus in practice, not a
  nicety** — bring it forward, ahead of M4.
- **2026-08-27** Opus pre-skip is handled: `05 Play It Right.opus` reports 143.094s played against a 143.1005s container
  duration — a 6.5 ms difference, exactly the standard 312-sample pre-skip at 48 kHz. Encoder delay trimming works,
  which matters for §11.1.
- **2026-08-27** Temporary `GMUSE_DEVICE=<id substring>` env var selects an output device. Placeholder until device
  selection becomes a real setting.
- **2026-08-27** **Resampler implemented (`src/resample.rs`), closing §11.5.** Policy: open the device at the file's own
  rate when it offers one, and resample otherwise — so the common case still costs nothing and the Opus-over-Bluetooth
  case works at all. rubato `Async::new_sinc` with `FixedAsync::Input`, `SincInterpolationParameters::default()`
  (sinc_len 256, BlackmanHarris2, cubic, oversampling 128), 1024-frame chunks. The filter's `output_delay()` is
  discarded so position stays honest and the head of a track is not silence — this matters for §11.1 gapless. Measured
  **218x realtime** for stereo 48k→44.1k (60s in 275ms), so the cost is negligible against the decode budget. Verified
  end-to-end: 48k Opus over the 44.1k-only Bluetooth sink played 143.101s against a 143.1005s container, 0 underruns.
  **Superseded 2026-08-27 by the quality-tier decision below.**
- **2026-08-27** `resample_quality` implemented as `resample::Quality` (`Fast` / `Balanced` / `High`), **default
  `Balanced` = rubato's `Fft`**, overridable with `GMUSE_RESAMPLE` until the settings layer lands in M6. Switching the
  default off sinc-256 was driven by measurement, and confirmed by rubato's own guidance: the `Fft` resampler is _"both
  fast and high quality, the natural choice whenever the ratio is fixed"_, while `Async::new_sinc` _"matches the quality
  of the Fft resampler but is the most CPU-heavy option"_ — its advantage is handling **drifting clocks**, not better
  sound. A player converting between fixed file and device rates never needs that. Keep `High` available for a future
  variable-ratio path (clock-drift correction against a device that free-runs).

  Cost measured **in situ**, resampling a 31.7s 44.1k track to a 48k device:

  | quality  | resampler      | CPU    | % of a core |
  | -------- | -------------- | ------ | ----------- |
  | Fast     | poly septic    | 0.045s | 0.14%       |
  | Balanced | **Fft**        | 0.027s | **0.085%**  |
  | High     | sinc 256 cubic | 0.439s | 1.39%       |

- **2026-08-27** **Benchmark methodology warning.** A throughput benchmark (resample N seconds as fast as possible)
  understated the sinc resampler's real cost by 3-5x: it reported 0.50% of a core where the running player measured
  1.6%. The player's decoder thread sleeps between packets, so a sinc's ~128KB of filter tables go cold and are
  re-fetched on every wake; a tight benchmark loop keeps them in cache. Thread QoS was tested and ruled out as the cause
  (a spawned thread resamples as fast as the main thread). **Measure inside the running player, not in a throughput
  loop.**

- **2026-08-27** Whole-process CPU during steady playback, M5 MacBook Air, 20s samples, for reference targets:

  | player                         | % of one core |
  | ------------------------------ | ------------- |
  | gmuse opus + FFT resample      | 0.55%         |
  | gmuse mp3, no resample         | 0.65%         |
  | `afplay` (macOS native)        | 0.75%         |
  | gmuse opus, no resample        | 0.80%         |
  | `ffplay -nodisp`               | 1.00%         |
  | gmuse opus + sinc-256 resample | 2.70%         |

  **Target: keep steady-state playback under ~1% of one core**, i.e. at or below `afplay`. Everything except the sinc
  path already clears it. The UI render loop is the next thing that will spend CPU; budget for it when M4 lands and
  re-measure rather than assuming.

- **2026-08-27** Added `lofty`, `walkdir`, `serde` and `postcard` for M3, all already budgeted in §2.
- **2026-08-27** **§11.4's `albumartist` fallback rule, decided:** `albumartist` → `artist` → the literal "Unknown
  Artist". Filing by `artist` would scatter a compilation across a dozen one-track entries and split a normal album the
  moment one track carries a guest credit — which `Anderson .Paak/Malibu` in the test library does on 5 of its 16
  tracks. A missing title falls back to the file name, not "Unknown": the file name is almost always the more useful
  thing to show.
- **2026-08-27** **Removal is a tombstone (`Vec<Option<Track>>`), with an explicit `compact()` rebuild.** §4 forbids an
  index shift because it would invalidate every `TrackId` held in a playlist or the queue. `upsert` updates in place, so
  a rescan of a file whose tags changed keeps its id. `compact()` returns an old→new remap and is deliberately not
  called anywhere yet — nothing holds ids across a rescan until M4/M5.
- **2026-08-27** **The cache has no compatibility story.** postcard behind a magic + `FORMAT_VERSION`; a version bump or
  any parse failure returns `Ok(None)` and the next run rescans. A rescan costs under a second, so a migration path for
  a derived cache would cost more to maintain than it could save. Saves are write-to-temp-then-rename, so an interrupted
  save leaves the previous cache intact. Tombstones are compacted away on save: nothing holds a `TrackId` across a
  restart, so persisting them would grow the file and the id space for ever.
- **2026-08-27** **M3 measured on 20,000 tracks** (hard links to the 148 real files, so tag parsing is real work but
  costs no disk), M5 MacBook Air:

  | operation                      | time  | notes                         |
  | ------------------------------ | ----- | ----------------------------- |
  | cold scan, 20k tracks          | 0.86s | ~23,000 tracks/second         |
  | incremental rescan (unchanged) | 0.08s | a `stat` per file, no reopen  |
  | full rescan (`--rescan`)       | 0.92s | ignores the mtime fast path   |
  | 2,025 files touched            | 0.15s | only the changed ones re-read |

  Peak RSS **78 MB**, cache **7.7 MB** (~385 bytes/track). The real library (148 tracks) scans in 0.09s. `walkdir` is
  comfortably fast enough that the §2 note about swapping in `jwalk` can be ignored.

- **2026-08-27** Added `ratatui` and `crossterm` for M4, both budgeted in §2 (at the corrected versions).
- **2026-08-27** **View 2 is view 1 flattened, not an independently sorted list.** Building `sorted` by walking the tree
  guarantees the two views agree on ordering; sorting a second time invites them to disagree about ties.
- **2026-08-27** **`smart_artist_sort` applies to artists only.** "The Beatles" files under B, but the album "The Wall"
  keeps its T — an article is noise in a name and meaningful in a title.
- **2026-08-27** **Track order inside an album is disc, then track number, then title, then path.** A missing disc
  counts as disc 1 (single-disc albums omit it), and a missing track number sorts _last_ rather than first, so untagged
  strays collect at the bottom instead of displacing track 1.
- **2026-08-27** **The tree is flattened to rows every frame** rather than kept as a persistent structure. Two levels
  and a `HashSet` of expanded keys is less code than a parallel tree that has to be re-synced whenever the library is
  rebuilt, and it cannot drift out of step. Revisit if profiling at 20k tracks says otherwise — it has not been measured
  under a render loop yet.
- **2026-08-27** **Space on an album row collapses its artist and moves the cursor to that artist.** Without the cursor
  move the selection would be left pointing at whatever row slid into that index — usually a different artist entirely.
  Moving the tree cursor also resets the track-pane cursor, for the same reason.
- **2026-08-27** **The TUI is the default; `--repl` keeps the M2 line driver.** M4 is read-only by design, so making the
  TUI the only interface would have removed the ability to play anything until M5.
- **2026-08-27** `ratatui::init()` installs a panic hook that restores the terminal, so a panic mid-render leaves a
  usable shell. Note that ratatui 0.30 made `Backend::Error` an associated type, so `ui::run` is bound on
  `Backend<Error = io::Error>`; `TestBackend` is used directly in tests rather than through that bound.
- **2026-08-27** **M4's acceptance cases are golden-file tests** over `TestBackend`, per §10: a compilation whose tracks
  carry three different `artist` tags stays one artist with all its tracks; a multi-disc album inserted disc-2-first
  renders disc 1 above disc 2; and a file with no tags at all appears under "Unknown Artist" showing its file name.
  Navigation clamping, `Tab` focus and view switching are covered by driving `handle_key` directly.

- **2026-08-27** **`next()` does not pop the queue.** §5 requires a pure function, and popping is a mutation. So
  `next_with_source` reports _where_ the track came from and the caller consumes the queue once it commits. `next()`
  keeps the exact signature §5 specifies and delegates.
- **2026-08-27** **Shuffle is a seeded permutation, not a played-set.** §5 describes "pick unplayed-at-random, tracking
  a played-set per pool". A Fisher-Yates permutation seeded from `rng_state` gives the same guarantee — nothing repeats
  until the pool is exhausted — while keeping `next()` pure, which §5 demands in the same paragraph. A played-set would
  have to be threaded in and out of every call and invalidated whenever the pool changed; a permutation simply _is_ the
  play order. Cost: repeating a shuffled pool would replay the same order, so `Source::Wrapped` tells the caller to
  reseed. `split_mix64` inline avoids taking a dependency on `rand` for one use.
- **2026-08-27** **`aaa_mode` narrowing resolves against the library tree, not the `Store`.** `Library::locate` finds
  the current track's artist and album, so `NextInput` needs exactly the fields §5 lists and no more.
- **2026-08-27** Two behaviours §5 left open, decided: **narrowing with nothing playing falls back to the whole
  library** (there is no artist to narrow to, and returning nothing would deadlock playback), and **a `current` that is
  not in the pool restarts the pool** rather than stopping — it happens whenever a queued track finishes or the mode
  changes under playback.
- **2026-08-27** A duplicate entry in a playlist resolves to its _first_ occurrence, because position is found by id.
  Playback advances rather than sticking, but the second copy is skipped. Fixing it properly means identifying a
  playlist position by index rather than by `TrackId`; deferred, and covered by a test that documents the behaviour.
- **2026-08-27** `C` moved from collapse-all to free the key for cmus's "continue"; collapse-all is `Backspace`.
- **2026-08-27** **M5 verification is partial.** Every row of the §5 table is covered — 26 tests in `next.rs`, plus 8
  more driving the UI's own decisions (queue consumption, end-of-pool, active-source switching). What has **not** been
  done is §9's "then listen for an hour". That is a human check and it is still outstanding.

- **2026-08-27** **§11.2 is smaller than feared on the default device.** cpal 0.18 reports a route change as
  `ErrorKind::DeviceChanged` and documents the stream as _"automatically rerouted, remains active, no rebuild required"_
  — but only for a stream built from the **default** device, which is what `preferred_config` does unless `--device` is
  passed. Switching speakers → headphones mid-track therefore just works. The rebuild-at-the-same-position problem §11.2
  describes still applies to a stream pinned to a specific device with `--device`, where a disappearing device reports
  `DeviceNotAvailable` instead. That case is still unimplemented.
- **2026-08-27** **Nothing in the audio path may print.** The stream error callback used `eprintln!`, which wrote over
  the alternate screen the TUI owns and corrupted the display on every route change. Errors are now classified in the
  callback into an atomic flag, turned into a `PlayerEvent` by the decoder, and shown in the status line.
  `DeviceChanged` is informational; `Xrun`, `RealtimeDenied` and `DeviceBusy` are advisory and ignored (they are not
  ring underruns and must not pollute that counter); everything else is treated as device loss. **The rule is general:
  once a TUI owns the screen, a stray `println!` anywhere below it is a rendering bug.**
- **2026-08-27** Status-line notices expire after 4 seconds, so a one-off message stops hiding the now-playing line for
  the rest of the session.

- **2026-08-27** **Stop does not clear the current track.** The decoder keeps the track loaded and merely rewinds it, so
  clearing `playback.current` in the UI desynced the two: `c` resumed real audio while the status line insisted nothing
  was playing. Stopped means stopped _at_ a track — which is also the cursor `b`/`z` step from. The general shape of
  this bug is worth remembering: the UI holds a shadow copy of player state, and every place the two can diverge is a
  defect waiting to be reported.
- **2026-08-27** The underrun counter is shown in the status line when non-zero. It is the only way to tell "the OS
  hiccuped during a route change" from "we failed to feed the device", after the fact and without a debugger.

- **2026-08-27** **Transport changes use an 8ms raised-cosine declick ramp, not zero-crossing detection.** Cutting
  straight to silence on pause leaves a step discontinuity, which is broadband by definition — that is the pop, and
  rapid pause/resume makes one per press.

  Zero-crossing is the classic wrong answer for this. It is the right tool for _splicing_ two waveforms (editing, loop
  points), but for transport: it removes the jump in value and not in slope, so a softer click survives; stereo channels
  rarely cross zero on the same sample, so at best one channel is clean; and the wait is unbounded — a bass-heavy
  passage can sit 25ms from a crossing and DC-offset material may never cross at all. A fixed ramp is bounded, applies
  to every channel at once, and a raised cosine is continuous in both value and slope where a linear ramp still has a
  corner at each end. This is what the Web Audio guidance means by preferring `linearRampToValueAtTime` over
  `setValueAtTime`, and what DAWs apply on transport.

  8ms is under the ~20ms at which a fade starts to read as a fade, so pausing still feels instant. The cost is that a
  pause consumes ~8ms of audio into the fade, so it lands fractionally later than the keypress — the trade the ear
  wants.

  The ramp also fires on **seek**: audio either side of a seek is unrelated, so `Fade::cut` drops to silence and the new
  position fades in rather than splicing. Volume is folded into the same per-frame multiply, so the buffer is walked
  once. All of it is float arithmetic on callback-local state, so the §3 invariant holds.

- **2026-08-27** **The declick ramp has to cover ring starvation, not just transport.** The first version faded on
  pause, resume and seek but left the underrun path cutting straight to silence: the last real sample sat at full level
  and the next was zero, then playback jumped back to full level when the ring recovered. Two clicks per underrun, and
  one at the end of every track, since a track ends by draining the ring.

  `Fade::fade_to_silence` scales the step to the frames actually available, so it lands on zero at the last real sample
  however little audio is left, and leaves the fade silent so playback ramps back in.

  The lesson is about where the test was pointed, not about the ramp: `Fade` had seven tests and all of them passed
  against the broken code, because the discontinuity lived in the buffer handling _around_ the ramp. Splitting
  `fill_output` out of the stream closure made the starvation path reachable from a test; verified by restoring the old
  behaviour, which fails the new test with a step of 0.64.

- **2026-08-27** **An interrupted ring write must keep its remainder.** `push` stops short when a command is waiting, so
  `stop` and `seek` stay responsive with a second of audio buffered — but the caller was discarding whatever had not
  been written. That spliced a **gap into the ring itself**, heard as a click when playback reached it, which is a
  second or so _after_ the command that caused it. That delay is what made it look like a fade bug rather than a
  producer bug: the transition sounded clean and the pop arrived later.

  The decoder now keeps an `outbox` of frames not yet written and drains it before decoding anything new. It is cleared
  wherever the ring is flushed (load, stop, seek), because those frames belong to the old position.

  `push` is also now **non-blocking**: it returns what fits and the run loop owns the one backpressure sleep. Waiting
  inside `push` made it untestable without a concurrent consumer — the first attempt at a test deadlocked — and put a
  sleep somewhere that had no way to be interrupted.

- **2026-08-27** Three clicks, three different layers, all reported as "a pop": the transport transition (no ramp), ring
  starvation (unramped cut at the buffer boundary), and a dropped chunk (a hole in the data). **A single audible symptom
  is not a single bug**, and the delay between action and pop is the clue to which layer it came from.

- **2026-08-27** **The default bindings are stored as rc-file text, not as constructed values.** `bind::DEFAULTS` is an
  array of `"bind common j win-down"` strings parsed at startup. The defaults therefore go through exactly the parser a
  user's rc file does, so a default that could not be expressed in the file — or that the parser gets wrong — fails a
  test instead of shipping. It also makes the defaults readable as documentation.
- **2026-08-27** **`cmd::keys::Key` is not `crossterm::KeyEvent`.** The terminal type also carries a press/release kind
  and a state bitfield, and two events a user would call "the same key" can differ in those, so a map keyed on it misses
  lookups. `Key` holds a code plus ctrl/alt, and **shift is folded into the character**: the terminal reports `J` with
  SHIFT set, and treating that as distinct from a bound `J` would mean no capital binding ever fired.
- **2026-08-27** **`:`, `/` and `?` are UI modes, not commands.** They open a prompt; there is nothing to execute until
  a line has been typed. The prompt swallows every key while open, which is what stopped `q` quitting mid-command while
  `q` was still bound to it.
- **2026-08-27** The rc file reports a bad line by number and **keeps loading**. One typo in a config should not cost
  every binding after it.
- **2026-08-27** Added `WinExpandAll`/`WinCollapseAll` beyond §6's list, bound to `M-e`/`M-c` — §6's table gives `E` and
  `C` to add-targets and continue, and expanding a whole tree is worth a key.
- **2026-08-27** **`--repl` does not apply the rc file** and says so, rather than loading it and silently doing nothing:
  the rc configures UI state that the line prompt has none of. The prompt still has its own small parser; folding it
  into `Command` is worthwhile once it is clear whether it survives at all.
- **2026-08-27** **§9's M6 criteria are two round-trip tests and one coverage test.** Every `Command` and every bindable
  `Key` must survive `parse(format(x)) == x`, or saving a config would quietly rewrite it. And every entry in `DEFAULTS`
  is resolved through `bind::lookup` from its own context and then executed, so a binding that parses but resolves to
  nothing — or panics when run — fails rather than waiting to be discovered by hand.

- **2026-08-28** **The rc file is replaced by TOML, and the command language survives intact.** Taken from eilmeldung
  (`docs/inspiration/eilmeldung.md`), whose bindings live in TOML but whose _values_ are command strings. That reframes
  "rc **or** TOML" into "what holds the commands": commands are imperative and need a language, settings and themes are
  declarative and want structure. §6's "one mechanism" is untouched — `:bind`, the `:` line and the config all still go
  through one parser and one `execute`.

  Gained: `deny_unknown_fields`, so a typo is an error located to `keys.common.^n` rather than a line that silently does
  nothing; a list value as a **macro** (`o = ["add-queue", "player-next"]`), which the flat file could not express;
  nested tables for theming, which it could not express either.

- **2026-08-28** **Theming is two levels: a palette of named colours, and styles that name a semantic role.** Widgets
  ask for `Role::BorderFocused`, never for cyan. Before this the UI had **21 hardcoded `Color::` literals across two
  files**, each one somewhere a theme could not reach; there are now zero outside `theme.rs`'s defaults. A palette name
  deliberately wins over a colour name, so a theme can rename anything. A config overrides only the roles it names, so a
  two-line theme is valid.

- **2026-08-28** Config problems are **collected, not fatal on the first**: a file with three typos reports three. Same
  reasoning as the rc file it replaces, kept.

- **2026-08-28** **gmuse is GPL-3.0-or-later.** Chosen, not inherited: copyright covers expression, not ideas, so
  reading eilmeldung and russ for architecture created no obligation. Nothing has been copied from either. Before this
  the project had no licence at all, which meant all-rights-reserved by default.

- **2026-08-28** Started `docs/inspiration/`, with notes on eilmeldung (config, theming, saved queries) and russ (SQLite
  as the store, non-eager refresh). It records the open questions those raise — chiefly whether the library should move
  from a postcard cache to SQLite before §7's filter language lands in M8, since "filter runs over the whole library on
  every keystroke" is a scan today and would be an index lookup there.

- **2026-08-28** **Catalogs are planned and recorded in §4**
  ([issue #3](https://gitlab.com/gabrielgellner/gmuse/-/issues/3)): several separate bodies of music, one or a subset
  active at a time, with playlists still crossing between them. It is written into §4 rather than left in the issue
  because it constrains what §4 and §5 may become, and both are INVARIANT — specifically, **nothing may start building a
  pool from the `Store` instead of the `Library`**, or confining the library would stop confining shuffle.
- **2026-08-28** **Catalogs and §7's filter language must be designed together in M8.** A catalog defined by a filter
  expression is a named, persistent filter; building the filter language first without that in mind risks two mechanisms
  for one idea.

- **2026-08-28** **Catalogs implemented** (`src/catalog.rs`), closing issue #3. A catalog is a set of scan roots; a
  track belongs to it when its path is under one of them, compared **by path component** so `/m/rock` does not sweep in
  `/m/rockabilly`.

  The prediction in §4 held: `next()` needed **no change at all**. Confining `Library` confines shuffle, `aaa_mode` and
  repeat, because §5 builds its pool from the library. The test that guards this walks 40 shuffled tracks and asserts
  every one is inside the active catalog — it would fail the moment anything started building pools from the `Store`.

- **2026-08-28** **`Library` moved from `Context` into `Ui`.** It was borrowed immutably for the whole run loop, which
  made it impossible to rebuild. Owning it is also what lets `smart_artist_sort` take effect immediately rather than
  reporting "applies on the next rescan", which is what it used to do.

- **2026-08-28** **Scanning covers every catalog root regardless of what is active**, so switching is instant rather
  than a rescan. Catalogs partition what is _shown_, never what is indexed — one scan, one cache.

- **2026-08-28** **`all` includes tracks no catalog claims.** Otherwise scanning a stray folder would make music vanish
  with no indication why. Adjusting from `all` (`:catalog -jazz`) starts from everything, which is the only sensible
  reading.

- **2026-08-28** **Mixing bare names with `+`/`-` in `:catalog` is rejected rather than guessed.** `:catalog jazz -rock`
  could mean "just jazz, minus rock" or "add jazz, remove rock"; being silently wrong is worse than asking again.

- **2026-08-28** The active set is **persisted in the cache** (`FORMAT_VERSION` 2) so a choice survives a restart, and
  names that no longer exist are dropped on load. Definitions still come from the config, not the cache.

- **2026-08-28** **Library roots belong in the config, not only the cache.** Bumping `FORMAT_VERSION` to 2 for catalogs
  discarded the old cache — and the roots lived _inside_ it, so there was nothing left to rescan from. The app then
  wrote an empty cache over the good one, making the loss permanent. A `just ui` with no arguments showed a blank
  library and no explanation.

  Three fixes, and the general lesson is the middle one:

  1. `library = ["~/Music"]` in `config.toml` is the durable home for roots. **The cache is derived data and may be
     thrown away at any time; nothing that cannot be recomputed may live only there.**
  2. **Never write an empty cache.** With no roots and no tracks there is nothing to record, and overwriting a good
     cache with an empty one destroys what it held.
  3. `cache::Outcome` distinguishes `Missing` from `Discarded`, so a version bump reports "discarded; rescanning"
     instead of silently starting from nothing.

  An empty library now also explains itself in both the TUI and the prompt, rather than rendering a blank pane.

- **2026-08-28** **A sentinel value cannot stand in for "the user did not say".** The remembered catalog choice was
  allowed to override the config only when the config's value looked like the default (`Active::All`) — but
  `active_catalogs = ["all"]` produces exactly that value, so writing it explicitly silently lost to whatever the last
  session had remembered. `Loaded::active_specified` now records _whether the key was present_, separately from what it
  resolved to. The general shape: **presence and value are different questions, and one field cannot answer both.**
  Found only because the prompt's `library` command was made to report the active catalogs, which turned an invisible
  behaviour into a number.
- **2026-08-28** The prompt's `library` command reports what the **active catalogs** cover rather than the whole store,
  which is what the views show. Previously the two contradicted each other.

- **2026-08-28** **Album dividers in track lists (issue #7), and the two coordinate systems they create.** A divider
  occupies a screen row but **no index**: cursors, counts and motions all address _tracks_, and `list::row_of_track` is
  the single place the two meet. That is slightly more bookkeeping in the renderer than making every motion
  header-aware, and it is the version where nothing outside `ui/list.rs` needs to know dividers exist — which is what
  keeps issue #6's motion layer simple.

  Rules that fell out of building it:

  - **A single-album list gets no divider.** The pane title or the tree selection already says which album it is; a lone
    header at the top is a wasted line.
  - **The artist is named only when the list spans more than one.** In a tree's track pane the artist is fixed by the
    selection, so repeating it on every divider is noise; in the sorted view it is the useful half of the label.
  - **A new disc divides within an album**, and a missing disc number counts as disc 1 — otherwise a partially tagged
    album divides itself in half.
  - **The playlist and queue are never divided.** They are deliberate orderings, and an interleaved one would get a
    divider on nearly every row. This was found by a test, not by design: `draw_tracks` served all three views and was
    dividing the playlist.

  Styled through `Role::AlbumHeader`, the same row cmus styles with `color_trackwin_album_fg`.

- **2026-08-28** **Vim motion layer (issue #6), and with it the chord machinery from #1.** Bindings are keyed on a
  _sequence_ of keys rather than one, written space-separated and quoted: `bind common "g g" win-top`. A separator
  character was not available — `-` and `+` are themselves bindable.

  - **Counts.** `3j`, `12G`. Digits `1`–`9` start a count; **`0` only ever continues one**, because in vim `0` is a
    motion of its own and here it would otherwise swallow the keypress.
  - **`count` is an `Option`, not a defaulted `1`.** `gt` is "next view" while `1gt` is "view 1" — the presence of a
    count changes the meaning, so collapsing the two would lose it. Same shape as the `active_catalogs` bug earlier:
    presence and value are different questions.
  - **The digits are freed**, so views move to nvim's tab model: `gt` / `gT` step, `<count>gt` jumps, and **`g<Tab>`
    returns to the previous view** — an alt-tab between library and queue that cmus has no equivalent for.
  - **An exact match beats a prefix.** `G` fires immediately rather than waiting to see whether a `G x` binding was
    meant. The consequence is that **a chord cannot extend an already-bound key**: binding `z z` while `z` is
    `player-prev` makes the chord unreachable. Pinned by a test, because it looks like the chord is simply broken.
  - **A part-typed chord shows in the status line** (`4g`), or it feels like a dropped keypress.

- **2026-08-28** **The relative-number gutter counts tracks, not screen rows** — the decision made when album dividers
  landed, now load-bearing. A divider gets blank gutter space, so a relative `3` is always three `j` presses. Vim's
  hybrid: absolute on the cursor row, relative elsewhere. Off by default.

- **2026-08-28** `Role::ALL` and `SettingKey::ALL` became `&'static [T]` slices. Rust refuses `[T; _]` in an item
  signature — inference is allowed in a function body, but a const's length is part of its public signature — and the
  count carried no meaning, unlike the `[Month; 12]` and `[Weekday; 7]` in the registry, where twelve and seven _are_
  the fact being stated. Ours grow; theirs cannot.

- **2026-08-28** **Directional pane movement and the `in <pane>` wrapper (issue #5).** `h`/`l` move between panes.
  Directional rather than reusing the `Tab` toggle: with two panes they behave the same, but the binding says what it
  means and keeps working when a third arrives.

  The more interesting half is **aiming a command at a pane that does not have focus**, taken from eilmeldung
  (`docs/inspiration/eilmeldung.md`). `bind library ^j in tree win-down` steps the tree while the _track_ pane keeps
  focus — so browsing album to album no longer needs a `Tab` over and a `Tab` back for each one.

  **A wrapper, not a variant per motion per pane.** `Command::In { pane, cmd }` sets the focused pane, runs the ordinary
  command through the same `execute_counted`, and restores focus. Every existing command works through it, counts pass
  through unchanged, and it is expressible on the `:` line (`:in tree win-bottom`) — none of which would be true of
  `win-down-in-tree`-style variants. This is why §5's issue said to design both halves together: doing `h`/`l` alone
  would not have forced the question.

- **2026-08-28** **The pending-key indicator is derived, not stored.** It was written into `status` with no expiry, so a
  completed `gg` left its own `g` behind for ever and hid the now-playing line. `status` was doing two unrelated jobs —
  transient notices, which expire, and a live indicator, which should vanish the moment the state it reflects does — and
  they fought.

  It now renders from `pending`/`count` directly (`Ui::pending_label`), which is vim's `showcmd`. Nothing to clear, so
  nothing can be forgotten. It also fixed a gap nobody had reported: a bare count (`12` with no chord yet) showed
  **nothing at all**, because the digit branch returned early without touching `status`.

  The general shape, and the third time this session: **state that is a view of other state should be computed, not
  mirrored.** The two copies always eventually disagree.

- **2026-08-28** **The default theme is Catppuccin Frappé, expressed through the ANSI sixteen** (issue #2). Named
  colours rather than hex, on purpose: a terminal themed to Frappé resolves `blue` to exactly `#8caaee`, so names give
  the true palette _and_ follow the terminal if it is later rethemed. `examples/catppuccin-frappe.toml` carries the hex
  variant for terminals whose palette we do not control — over SSH, or on a plain box.

- **2026-08-28** **Nothing may put a dark foreground on a light background**, a rule taken verbatim from the cmus rc
  this palette came from: the ANSI sixteen carry no colour dark enough to read against blue or pink, so a selection is a
  _surface_ with a coloured foreground. The previous defaults broke it — black on cyan, for both the active tab and the
  selection. Now pinned by a test that walks every role.

- **2026-08-28** One deliberate deviation from that rc: it uses `black` (surface1) as the selection background, which is
  right on a Frappé-themed terminal and **invisible on an unthemed one**, where `black` is the background. `DarkGray`
  (surface2) is a Frappé surface too and reads on both. The hex theme uses surface1 as written, because there the
  background is known.

- **2026-08-28** Two tests keep the shipped theme honest: it must **parse cleanly**, and it must **name every `Role`**.
  A role added in code and forgotten in the example would silently keep its default, which is the kind of gap nobody
  notices until a theme looks half-applied.

- **2026-08-28** **`:` completion (issue #4), and the vocabulary problem it forced.** Completion needs an enumerable
  list of verbs; the parser's match arms are not one. Rather than restructure the parser around a table, `parse::VERBS`
  sits beside it and **a test scans the source** to prove the two agree. Blunt, but it is the only thing that catches a
  verb added to the match and forgotten in the table — which would silently make it uncompletable rather than fail.

- **2026-08-28** **Completion is prefix-based, not fuzzy, and that is deliberate.** The `:` line is a small closed
  vocabulary the user is trying to type exactly, where a prefix match is predictable and a fuzzy one is a guess. Fuzzy
  matching belongs on `/` search, over titles — which is where `nucleo-matcher` from §2 will eventually earn its place.

- **2026-08-28** Arguments complete per verb, and **`set` values depend on the key already typed** — the same
  key-directed typing the parser does in `setting_value`. `set key=value` is one word, so the value half needs its own
  entry point: the split is inside the word, not at a space.

- **2026-08-28** Two smaller rules that fell out: a **single match gains a trailing space**, because the next thing you
  type is an argument; and **nothing matching leaves the line untouched** with a notice, because completion must never
  eat what was typed.

- **2026-08-28** **A session log (`--log`), because underruns are the one defect a test cannot catch.** §3 says they
  show up only in listening, and by the time the counter is noticed the moment has passed. The log records loads,
  finishes, device reroutes, errors, and every underrun with **when, in which track, and how many frames short** — the
  last being the difference between a handful of frames and a whole period, which a bare count cannot tell you.

  **Nothing is written from the audio callback.** It stores `last_short_frames` as a plain atomic; the UI loop polls the
  counter every 200ms and writes. A 200ms tick is ample: which track and roughly when is what matters, not the exact
  millisecond.

  Off by default — a player that writes to disk every session without being asked is a surprise. Every line is flushed
  as it is written, because a session that ends by being killed is exactly the one worth having a log of.

- **2026-08-28** **`cli.rs` had no tests, and shipped a flag that never worked.** `--log` used
  `default_missing_value = ""`, which clap ignores — so the bare flag failed at runtime with "a value is required",
  while compiling and passing every test. The idiom for an optional value is `Option<Option<T>>`: absent is `None`, bare
  is `Some(None)`, with a value is `Some(Some(v))`.

  `cli.rs` now has tests, including `Cli::command().debug_assert()` — clap's own consistency check, which catches
  duplicate short flags and malformed defaults that are otherwise a runtime panic. **A CLI surface needs tests for the
  same reason a parser does: it is a boundary, and the compiler checks none of it.**

- **2026-08-28** The playing track is emphasised in every list it appears in (`Role::Playing`, bold and the same green
  as the status line's play indicator, so "playing" means one thing wherever it shows). Useful during a long listen,
  where the question "is this still the track I started" otherwise means reading the status bar.

  **Known limit:** on the _selected_ row the mark is indistinguishable, because the selection style is bold too. A
  marker glyph in the gutter would separate them, at the cost of a column; bold was what was asked for and the overlap
  only affects the one row the cursor is already on.

- **2026-08-28** **An end-of-track drain is not an underrun, and counting it made the figure meaningless.** The first
  logged listening session — 45 minutes, 11 Burial tracks — recorded exactly one underrun per track, every one at a
  position equal to that track's own duration, sharing a timestamp with its `finished` line. The counter had become a
  count of tracks played.

  **This is a regression, and the log is what caught it.** M1 had the guard — _"only count it as an underrun if the
  decoder still owes us audio; a short final callback at end-of-stream is normal"_ — and it was lost when the callback
  moved into `device.rs` in M2. Restored as a `draining` flag the decoder sets when it has pushed the last frame and
  clears on load, seek and stop. The fade still runs, so the gap is entered smoothly either way; only the count changes.

  **The genuinely good news in that log: zero underruns during playback.** 45 continuous minutes, no mid-track
  starvation at all. The audio path keeps up comfortably, and the ~200ms gap between tracks is the non-gapless behaviour
  M7 exists to remove.

  Two lessons. The counter must distinguish _"we failed to keep up"_ from _"the track ended"_, or it reports the second
  and hides the first. And a metric with no test is a metric that can silently invert — pinned now.

- **2026-08-28** **The cursor landing on the playing track keeps the playing colour** (`Role::SelectionPlaying`: the
  selection's surface, the playing green). Otherwise the selection masks the one thing a listening test is watching for
  — and the row you are most likely to be parked on is the one that is playing.

  Only while the pane is focused. An unfocused pane should still read as unfocused, and its dim selection is the only
  thing saying so.

  This is the general shape of the earlier `status` collision: two pieces of information competing for one cell, where
  the answer is a third style rather than letting one silently win.

- **2026-08-28** **M5's hour of listening, logged.** 56 minutes, 19 tracks, 15 played to their end, on the built-in 48
  kHz output with mixed paths — resampled 44.1 kHz MP3s interleaved with native 48 kHz Opus. **Zero underruns.**

  The three logged sessions together are what makes that number mean anything:

  | session | device rate | track ends | underruns | build          |
  | ------- | ----------- | ---------- | --------- | -------------- |
  | 2       | 44100 Hz    | 13         | 14        | before the fix |
  | 3       | 48000 Hz    | 1          | 1         | before the fix |
  | 4       | 48000 Hz    | 15         | **0**     | after the fix  |

  Sessions 2 and 3 recorded one underrun per track end, which is the signature of the miscount fixed in `!22`, not of
  starvation. Session 4 is the acceptance criterion met. §9's M5 is now closed.

- **2026-08-28** **Encoder delay and padding are already handled — do not handle them again.** §11.1 listed this as half
  of the gapless work on the strength of Symphonia 0.5, where `FormatOptions::enable_gapless` was an opt-in the caller
  had to set. That option does not exist in 0.6, and the behaviour changed with it: the **decoder** applies the trim,
  and `Packet::trim_start`/`trim_end` are there to be read, not to be acted on.

  Measured, rather than assumed, on real files:

  | file        | packet | `dur` | `trim_start` | `trim_end` | frames decoded |
  | ----------- | ------ | ----- | ------------ | ---------- | -------------- |
  | MP3, first  | 0      | 1152  | 1105         | 0          | **47**         |
  | MP3, last   | 8343   | 1152  | 0            | 323        | **829**        |
  | Opus, first | 0      | 960   | 0            | 0          | **648**        |

  47 = 1152 − 1105 and 829 = 1152 − 323, so the MP3 decoder has already applied both. Opus reports no trim at all
  because libopus consumes the 312-frame pre-skip internally; the packet fields never mention it. Implementing the
  documented "apply the trim" step would have cut a further 25 ms off the head of every MP3 — a bug that listens as a
  clipped attack and that no test comparing our own output to our own output could have caught.

  Note also that `dur` is reported as the nominal block duration (1152 uniformly), not the "valid frames" the trait
  documentation describes. Nothing here depends on it — frame counts come from the decoded buffer — but it is not a
  field to trust.

- **2026-08-28** **`open()` no longer rewinds, because rewinding un-trims the first packet.** It used to decode one
  packet to learn the sample rate — codec parameters can omit it — then seek back to zero and throw that audio away. The
  seek is what broke it: a decoder applies its encoder-delay trim only while positioned at the head of the stream.

  Measured on the same Opus file, before and after `seek(0)` + `reset()`:

  |                 | `trim_start` | frames decoded |
  | --------------- | ------------ | -------------- |
  | fresh decoder   | 0            | **648**        |
  | after rewinding | 0            | **960**        |

  So every Opus file was playing its 312-frame pre-skip — 6.5 ms of decoder ramp-up — at the head of every track. MP3
  survives the rewind (1105/47 both times), which is why this hid for so long: the format that reports the trim keeps
  it, and the format that does not, loses it.

  It was inaudible on an ordinary load only because the 8 ms transport fade ramps straight over it. A gapless join has
  no fade, which is what surfaced it. The fix removes work rather than adding it: keep the probe packet's audio as the
  head of the track instead of seeking back past it.

  The one case left is `Stop`, which rewinds through `seek_to_frame(0)` and so still re-emits the pre-skip on Opus. It
  stays under the fade, and recovering the decoder's stream-start state would mean reopening the file.

- **2026-08-28** **M7 — gapless, and why the resampler was the hard part.** The transition is a splice in the decoder:
  at end of stream it keeps the same ring, the same device and the same producer, opens the next track, and carries on
  pushing. No flush, no drain to silence, no gap. §3's "the device is opened once" was the prerequisite.

  Three things had to be got right, and only the first was expected.

  **The resampler must be carried across the join, not flushed and rebuilt.** `flush()` zero-pads the final chunk to
  complete it, and that padding is up to 1024 frames — 23 ms at 44.1 kHz — of silence spliced into the join, with the
  new track's filter then discarding its own delay on top. Fed continuously instead, one filter resamples the two tracks
  as the single signal they are, which is exactly what gapless means. Only done when the ratio is unchanged; a rate
  change flushes the old tail first, because that tail is still the end of the outgoing track.

  **The head of a track cannot be resampled at open time.** `open()` decodes a probe packet before the resampler is
  settled, and a spliced track _inherits_ the previous track's filter. Running the head through the track's own filter
  first discarded that filter's delay and swallowed 160 frames at the join. The head is now kept in source frames and
  resampled on first use, once it is known which filter it belongs to.

  **`Loaded` had to be held back.** The decoder crosses the join a ring's depth — about a second — before the listener
  hears it. Announcing at splice time would put the next title on screen a second early and jump the elapsed time before
  the audio moved. `Loaded` is now queued against the frame position of the join and emitted when the playhead reaches
  it, which also makes it mean the same thing it always did: _this is what you are hearing_.

  Position follows from that: `position_frames` is not reset at a join, since there is no flush, so `track_base` marks
  where the current track began and `PlayerHandle::position` is the difference. Every flush clears it.

- **2026-08-28** **The next track is resolved when it is needed, not announced in advance.** The obvious design — tell
  the player what comes next as soon as a track starts — has a staleness problem with no clean answer: toggling shuffle
  or editing the queue mid-track invalidates the announcement, and once the decoder has committed to it there is no
  well-defined meaning for a replacement.

  Inverted instead. At end of stream the decoder emits `NeedNext` and keeps decoding nothing while the ring still holds
  its last second; the UI answers with `SetNext(Some(path))` or `SetNext(None)`. The resolution therefore happens at the
  moment it is used and cannot go stale, `next()` stays pure, and the queue is popped exactly once by whoever answers.
  No generation counters, no revision protocol.

  The reply budget is bounded by two numbers that were already fixed: the UI's 200 ms poll and the ring's 1000 ms depth.
  500 ms sits between them. Missing the window is not a fault — it is the pre-M7 behaviour, a gap.

- **2026-08-28** **A test whose expectation is built from the code under test proves nothing.** The first gapless tests
  compared the spliced output against `open()` + `decode_chunk()` of the same two files. Deleting the head of every
  track — a real bug, 47 frames of MP3 and 648 of Opus — broke the expectation in exactly the same way and the tests
  stayed green. The reference is now derived from the generator that wrote the test WAV, so it depends on nothing the
  decoder does.

  The other two both had the same shape. Checking for runs of exact zeroes at the join found nothing when the resampler
  _was_ being flushed, because zero padding comes back out of an FFT filter as ringing rather than silence — the right
  assertion is equality against resampling the concatenation as one signal. And the event-timing test could not fail
  while the test sink drained the ring instantly, which kept the decoder and the playhead in lockstep and made "a second
  early" indistinguishable from "on time"; the sink is now paced.

  All four are now confirmed by mutation: breaking the resampler carry, the held-back `Loaded`, the head handling, or
  the splice itself each fails the test that claims to cover it. This is the fourth time this project has produced a
  test aimed at the mechanism rather than the outcome — after the seek, the removal, the dividers and the playing mark.
  Writing the assertion against something the implementation cannot influence is the only reliable defence.

- **2026-08-28** **M7's acid test, passed — and how to find one.** §9 asks for a continuous-mix album and the library
  turned out not to advertise which of its albums that is. There is no reliable metadata for it: encoder delay/padding
  is on every file regardless of intent, iTunes' "part of a gapless album" flag (`pgap` in MP4, `iTunPGAP` in ID3) is
  absent here, and there are no CUE sheets. The answer is measured, not read — decode the last 150 ms of one track and
  the first 150 ms of the next, and a join where neither is silent was meant to be continuous:

  | album                        | continuous joins |
  | ---------------------------- | ---------------- |
  | Anderson .Paak — _Malibu_    | **12 of 15**     |
  | Lewis Del Mar — _AUGUST_     | 4 of 14          |
  | Mr. Oizo — _The Church_      | 4 of 9           |
  | Burial — _S/T_               | 0 of 12          |
  | Christian Löffler — _A Life_ | 0 of 10          |

  The hardest join available is _Put Me Thru_ → _Am I Wrong_: −5.8 dB out, 0.0 dB in, both effectively full scale, so a
  gap would be a hole punched in loud music. Reported smooth.

  The log then measured the other half of the claim, in the running player rather than in a test:

  | splice             | `next splicing` | `loaded`     | lag        |
  | ------------------ | --------------- | ------------ | ---------- |
  | Burial 01→02       | 00:00:46.797    | 00:00:47.806 | 1.009s     |
  | Malibu 05→06       | 00:07:17.149    | 00:07:18.160 | **1.011s** |
  | Malibu 05→06 again | 00:09:05.265    | 00:09:06.275 | 1.010s     |

  All three land on the ring's depth (`DEPTH_MS = 1000`). That is the held-back `Loaded` doing exactly what it is for:
  the decoder crosses the join a second before the listener does, and the title changes with the audio. Zero underruns
  throughout, across both resampled 44.1 kHz paths and a manual restart mid-album.

  Worth keeping the near-miss on record too. The first pass of the boundary scan reported _every_ album 100% continuous,
  because `ffmpeg -v error` suppressed `volumedetect`'s output and the comparison read the empty string as `0 > -40`. A
  missing measurement scored as a passing one — the same shape as the test failures earlier in this milestone, in a
  throwaway shell script rather than in the code. Default a missing reading to the failing value.

- **2026-08-29** **The completion menu is navigable (issue #8).** `C-n`/`C-p` walk it, `C-y` accepts, and the three open
  questions were settled as: **`Tab` cycles once the menu is open**, as vim's wildmenu does — a second `Tab` would
  otherwise re-complete against an unchanged candidate set and do nothing, so cycling is strictly the more useful
  reading; **`C-y` takes the first candidate when the selection has not been moved**, which is what a shell does and
  keeps the key from being dead most of the time the menu is up; and **the walk wraps**, in both directions.

  That second rule then forced a correction. The menu first opened with _nothing_ highlighted, on the reasoning that
  "listed" and "being navigated" were different states — but they are not, because `C-y` acts on a candidate either way.
  The state was a fiction the screen told and the keys ignored, and it left the key's effect to be discovered by
  pressing it. **The menu now opens on the first candidate and always shows its selection**, so `selected` is a plain
  `usize` rather than an `Option` and the impossible state is gone with it.

  This is the `v`/`c` shadow and the pending-key indicator arriving from the other direction: there, state was mirrored
  when it should have been derived; here, the UI drew a distinction the behaviour did not honour. Both are the interface
  disagreeing with itself.

  `Esc` now closes the **menu** first and the **line** second. Dismissing a list of possibilities should not throw away
  what has been typed.

  Two things fell out of the rewrite the issue predicted. `completion_rows` used to pre-format each row into one padded
  `String`, which made styling a single candidate impossible; it now returns candidate **indices** and the caller builds
  the spans. That also fixed a latent bug for free — the overflow notice counted words in a rendered row, so any
  candidate containing a space (a catalog name can) was miscounted.

  And the menu is capped at eight rows, so cycling had to **scroll**: a selection that state agrees on but the screen
  has moved past is not a selection. The offset is computed at draw time from the selected index rather than stored,
  because it is a view of other state — the same rule as the `v`/`c` shadow and the pending-key indicator.

  `Role::CompletionSelected` is new rather than reusing `SelectionFocused`: a transient menu choice and a list cursor
  are different things and a theme may want to tell them apart. It takes the surface-plus-foreground shape the other
  selections use — the first attempt was a dark foreground on cyan, which `no_dark_foreground_on_a_light_background`
  rejected, correctly, because in the default theme those are the terminal's own sixteen colours.

  All eight tests are mutation-checked: breaking the wrap, the scroll, the dismiss-on-accept, the `Esc` order, the `Tab`
  cycle, the stale selection, the opening selection, or the `key=value` split each fails the test that covers it. The
  selection assertions read what is **highlighted on the rendered buffer**, not `prompt.selected`, which is what makes
  the scrolling case testable at all.

- **2026-08-29** **`cat` dropped as an alias for `catalog`.** The vocabulary is 41 distinct commands behind 62 verbs, so
  a third of what the completion menu lists is synonyms — which is how this surfaced. Most of them earn it: the
  `win-*`/`player-*` pairs are cmus's own names with the short forms people actually type, and `vol`/`volume`,
  `q`/`quit`/`exit` are two spellings of one word.

  `cat` was the weakest. It is not a synonym but a truncation, it collides with the best-known command name in Unix, and
  catalogs are ours rather than cmus's — so there is no compatibility argument for it the way there is for `win-up`.
  Whether the rest of the vocabulary is carrying cmus-shaped cruft is issue #9.

- **2026-08-29** **The arrows drive the completion menu too, and a chord is not a keystroke.** `Up`/`Down` are synonyms
  for `C-p`/`C-n`. They are not what this UI is for, but a menu that ignores them reads as broken. Because candidates
  run _down_ the columns, `Down` really is the next candidate rather than an approximation of it.

  `Left`/`Right` are deliberately absent. They would mean one column, and the column count is computed at draw time from
  the terminal width — the key handler does not know it. Caching it on `Prompt` would be exactly the mirrored state this
  log keeps recording the removal of, to make an arrow key work.

  Writing the test for it turned up a real bug. The first attempt asserted the arrows do nothing when no menu is open,
  which cannot fail: `step` is already a no-op on an empty menu. Mutating the guard away broke nothing, and probing for
  what the guard _does_ change found that `C-n` `C-p` `C-y` on a bare `:` line left **"npy"** in the buffer. The
  `KeyCode::Char(c)` arm did not look at modifiers, so every chord typed its own letter — `C-a`, `C-w` and the rest
  included. Guarded on ctrl now.

  Worth noting how it was found: not by the test passing or failing, but by the _mutation_ passing. A test nothing can
  break is not evidence, and asking what a guard actually changes is what turned an empty assertion into a fix.

- **2026-08-29** **The command vocabulary audited (issue #9): 41 commands and 62 verbs became 40 and 41.** cmus is this
  project's behavioural specification, and §14 already said to consult it for _what does it do when…_ and never for
  structure. Command **names** sit on that line, and they had drifted onto the wrong side of it.

  The `win-` and `player-` prefixes are gone. cmus namespaces because its command set is flat and large; ours is forty
  commands, and the short forms were what anyone typed anyway — the long ones were fifteen verbs of completion noise for
  a compatibility nobody had asked for. `:p<Tab>` offered thirteen entries covering seven commands; it now offers six.

  Dropping the prefix only where a short alias already existed would have left `up`, `down`, `activate` beside
  `win-expand-all`, so it came off everywhere. That forced two names, because the bare forms were taken:

  | was             | now         | why                                                                                     |
  | --------------- | ----------- | --------------------------------------------------------------------------------------- |
  | `win-toggle`    | `expand`    | `toggle` belongs to settings; this pairs with `expand-all`                              |
  | `win-update`    | —           | **deleted**: `Command::WinUpdate` and `Command::Update` were already the same match arm |
  | `win-next-pane` | `pane-next` | so the three pane commands read alike                                                   |
  | `update`        | `rescan`    | the CLI already has `--rescan` and the justfile a `rescan` recipe                       |

  The word-for-word synonyms went too — `previous`, `volume`, `append`, `enqueue`, `exit` — leaving `q` as the only
  alias in the language. `rsearch` stays: it is opaque, but it sits beside `search-next`/`search-prev`, which mean
  _repeat_ forward and back, and `search-back` would read as a third member of that family.

  The `Command` variants were renamed with the verbs. `Command::WinUp` describing a verb spelled `up` is exactly the
  cruft this issue was about, one layer down.

  **The tests were the interesting part.** Six broke for a reason that had nothing to do with what they were testing:
  they needed _some_ ambiguous prefix and had reached for whichever one was nearest. `win-p` → `win-pa` became `pan` →
  `pane-`; `pl` → `play` became `sea` → `search`. Two were worse than brittle — one asserted the settings view was
  drawing bindings by looking for the substring `"win-"`, and one asserted the completion menu overflows, which stopped
  being true once the vocabulary shrank enough to fit. Both now derive what they need (`parse::format`, a narrower
  render) instead of hardcoding a fact about the vocabulary.

  This is the same lesson as the gapless work, in a different costume: an assertion that depends on something the change
  is allowed to alter is not testing what it claims to. A vocabulary audit is unusually good at finding them, because it
  invalidates every incidental name at once.

  Earlier entries in this log still quote `win-down` and `player-next`. They are records of what was decided when, and
  are left as written.

- **2026-08-29** **The line prompt follows the same vocabulary rules.** `--repl` has its own small language — it has
  commands the `:` line does not (`load`, `status`, `library`, `find`) — but it had the same synonyms #9 removed, so
  `open`, `volume` and `exit` are gone and `scan` is now `rescan`, matching the `:` line, the `--rescan` flag and the
  justfile recipe.

  Its single-letter forms stay. They look like the aliases #9 deleted and are not: the repl is typed at speed with no
  completion menu to clutter, and one letter per command is a deliberate system rather than an accumulation. Even the
  two-letter oddities are earned — `status` is `st` and `library` is `lib` only because `s` and `l` are taken by `stop`
  and `load`. That is the test the deleted aliases failed: `cat` existed for no reason but truncation.

- **2026-08-29** **Volume maps linearly onto decibels, not onto amplitude (issue #11).** The percentage _was_ the
  amplitude multiplier, handed straight to the callback. `:vol 50%` therefore meant -6dB — "slightly quieter" rather
  than "half" — and a ±5% step moved **0.45dB at the top of the range and 6dB near the bottom**. The first few presses
  of `-` sat below the ~1dB a listener can notice; the last were a cliff. Reported as "little impact until it drops
  off", which is exactly what a thirteenfold spread in step size feels like.

  Checked against the references rather than guessed, and the result was not what was expected:

  | player                         | mapping                      | source                                                           |
  | ------------------------------ | ---------------------------- | ---------------------------------------------------------------- |
  | cmus                           | dB-linear table, ~48dB       | `soft_vol_db[100]`, "copied from alsa-lib src/pcm/pcm_softvol.c" |
  | librespot (via spotify-player) | dB-linear, 60dB, the default | `VolumeCtrl::Log(DEFAULT_DB_RANGE)`, `mixer/mappings.rs`         |
  | mpv                            | cubic, `(vol/100)^3`         | `audio_get_gain()`: `gain = pow(gain, 3)`                        |

  spotify-player does no scaling of its own — it converts its percentage linearly to librespot's `u16` and defers. Nor
  is that curve "Spotify's": the protocol carries a 0-65535 value and the client picks the shape. librespot picked one
  citing dr-lex.be.

  **The decisive finding is that a power law does not fix the unevenness at all.** With `gain = p^n` the step in dB is
  `20n*log10((p-d)/p)`, so the exponent scales every step by the same factor. mpv's cubic has a **13.5x** spread between
  its top and bottom step; the raw amplitude mapping has **13.4x**. Cubic makes each press three times bigger and leaves
  the problem exactly where it was. Only a dB-linear mapping equalises the steps, and that matters here because the
  control is incremental keys rather than a slider dragged to an absolute position — mpv's choice is defensible for a
  GUI and does not answer our question.

  So: 60dB, following librespot, which also lands a 5% step on exactly 3dB. Silence is a special case, because the
  formula gives -60dB at the bottom and a fader wound down should be _off_.

  The **ceiling is unchanged** at +6dB. This alters the shape of the control, not how loud it can get, so the fader runs
  to 110% rather than stopping at unity.

  The position is now the source of truth and the gain is derived from it. The gain is still stored in its own atomic,
  which looks like the mirrored state this log keeps deleting and is not: the callback may not compute a power (§3), so
  the mapping has to happen where the value is set. That is a cache with a stated reason rather than a second copy of
  the truth.

  The test that matters asserts the _step_, not the curve: every 5% move changes the level by the same 3dB, across the
  whole range. Checking the curve's value at a few points would have passed against the old mapping. It is confirmed by
  mutation against both the linear mapping and mpv's cubic — the latter being the point, since "not linear" was never
  the requirement.

- **2026-08-29** **ReplayGain applied (issue #12), and analysis deliberately left outside.** §4 has listed
  `replaygain: Option<ReplayGain>` on `Track` since the start, and `scan.rs` has been reading all four tags through
  lofty for as long as there has been a scanner. Nothing had ever used the field. This wires it up.

  **The measurement stays out of the program.** §1 puts tag _editing_ out of scope, so we could not write results back
  even if we computed them, and computing them means decoding every track in full — minutes for a small library, hours
  for a large one. `rsgain` (the maintained successor to loudgain, which stopped in 2019) already does this well, and
  its easy mode writes ReplayGain 2.0 tags at -18 LUFS with album figures and clipping protection. Reading what it wrote
  is the whole feature. The local library had **0 of 148 files tagged**, which is why "apply the tags" alone would have
  looked like it did nothing.

  Three decisions:

  **`auto` follows the play order.** Shuffling jumps between masters, so each track's own level is what matters. Playing
  an album in order, the album figure preserves the _relative_ levels inside it — turning those into per-track
  corrections would flatten exactly what the mastering engineer intended, and on a continuous mix it would be audible at
  every join. Album mode falls back to the track figure rather than giving up, so a track-only scan still works.

  **A known peak caps the gain.** Positive gain on a quiet master is where clipping happens; being a decibel under the
  target is better than being over full scale. A cut is never capped, because it cannot clip.

  **The gain is applied in the decoder, not the callback.** This is the opposite of the volume fader, which lives in the
  callback precisely so a change is heard immediately. A _per-track_ gain wants the other property: it must land exactly
  at the track boundary, and after M7 the decoder is a ring's depth ahead of what the listener is hearing. Applying it
  at the output would put the next track's gain on the last second of the current one. Applying it as frames are
  produced means it travels with the audio it belongs to and alignment is not something anyone maintains.

  That last point is also what the test had to check, and the first two attempts at the mutation were both duds. One set
  `t.gain` immediately before `*t = nt` — dead code, since the outgoing track is fully pushed by then. The other patched
  the `pending` branch of `decode_chunk` rather than the `head` branch, because the two are textually identical and the
  replacement took the first. Only the third — the spliced track's head bypassing the gain — moved the join, and the
  failure names frame 30000 exactly. **A mutation that passes is not evidence the test is weak; it can equally mean the
  mutation was inert.** Check which before believing either.

- **2026-08-29** **The library stays a postcard cache. SQLite is not needed, and the reason is measured.** The russ
  notes left open "whether the library should move from a postcard cache to SQLite before §7's filter language lands in
  M8, since 'filter runs over the whole library on every keystroke' is a scan today and would be an index lookup there."

  That was the main argument, and it does not survive a measurement. A compiled closure over synthetic tracks with
  realistic string fields, timed over 50 passes:

  | filter                         | 20k tracks | 100k tracks |
  | ------------------------------ | ---------: | ----------: |
  | `bitrate>192` (numeric only)   |    0.01 ms |     0.06 ms |
  | `genre=jazz` (string equality) |    0.02 ms |     0.11 ms |
  | bare word across 3 text fields |    0.92 ms |     4.58 ms |
  | text AND numeric               |    0.31 ms |     1.82 ms |

  A whole-library scan per keystroke costs **under a millisecond at 20k tracks**, and the worst case — a bare word
  substring-matched against every text field — is 46 ns per track. There is no index to be won here at any library size
  this project will see. §7's "compile once to a closure, do not re-interpret the AST per track" is the optimisation
  that matters, and it is already in the brief.

  **What remains true is a lifecycle argument, and it points somewhere else.** The cache and per-track user state (#13,
  #14) want opposite things:

  |                    | library cache         | user state              |
  | ------------------ | --------------------- | ----------------------- |
  | nature             | derived               | **not** derived         |
  | on a format change | discard and rescan    | must survive            |
  | written            | in bulk, at scan time | incrementally, per play |
  | if lost            | a slow startup        | gone for good           |

  postcard is right for the first and wrong for the second — a whole-file rewrite, and a `FORMAT_VERSION` bump means
  data loss, which is the roots incident again with something that cannot be rescanned. So the conclusion is **two
  stores with different lifecycles**, not one database for everything, and it follows directly from the rule that entry
  already established: the cache is derived data and may be discarded at any time.

  For the durable half, a plain file written temp-then-rename is enough. Atomic rename alone prevents the "wrote an
  empty cache over the good one" failure, with no dependency at all. `rusqlite` would also mean compiling SQLite from C
  — a second C dependency, considerably larger than libopus, in a project whose §2 budget is deliberately small.

  SQLite would win if we wanted ad-hoc querying with external tools, or concurrent access from a second process, which
  M9's remote socket could eventually raise. Neither is demonstrated today. Revisit then, not now.

- **2026-08-29** **Three more projects read (`docs/inspiration/`): spotatui, trollstov, audium.** The folder's rule
  still holds — navigation, keybindings and TUI architecture only — but two of these argue with §1 rather than adding to
  it, and that is worth recording rather than filing away.

  **§1's "tag editing (reading only)" is a trade, not a law.** trollstov forbids metadata editing and then carves out
  the rating as its single exception, because portability is the whole point of a rating: back up the music folder and
  the ratings come with it. audium goes further and writes title, artist and album back to the files as a headline
  feature. Neither makes §1 wrong — we do not want to be the program that corrupted someone's library — but both show
  the carve-out is a coherent position rather than the top of a slope, and issue #14 should be read with that in mind.

  **audium's storage layout independently reproduces the conclusion above**: config in `$XDG_CONFIG_HOME`, state in
  `$XDG_STATE_HOME`, a small index in `$XDG_DATA_HOME`, and _"all writes use atomic rename operations for crash
  safety"_. Separate stores by lifecycle, temp-then-rename instead of a database. That the same answer arrives from
  somewhere else is mild evidence it is the right one.

  Three concrete things to take, in order of value:

  1. **Session resumption** (audium): queue, current track, position and modes restored on launch, **paused**. We
     restore nothing. It is state, it belongs beside the audit log in `$XDG_STATE_HOME`, and `paused` is the detail that
     makes it inoffensive. **Taken — issue #16**, where the gmuse-specific part is that shuffle is a seeded permutation,
     so restoring `rng_state` restores the _sequence_ rather than merely the mode.
  2. **Do not guess the terminal — considered and declined, 2026-08-29.** trollstov picks a light or dark palette from
     `terminal_colorsaurus::theme_mode()`; audium falls back to 16 colours when truecolor is unavailable. Broad terminal
     support is not a goal here, the ANSI-name palette already degrades by design, and detecting would mean a dependency
     to solve a problem this project does not have. Recorded so it stops being rediscovered as a recommendation.
  3. **Presentation as configuration** (spotatui): which columns a track list shows, in what order, how wide and under
     what heading, as a config table. This is the natural home for a rating or play-count column without either needing
     a layout decision in code, and it belongs in the M8 conversation.

  Two smaller ones: spotatui's format templates use **named placeholders** (`{state}`, `{volume}`) where §7 specifies
  cmus-style `%a`. That choice has not been made deliberately and should be — long names have room for fields like
  `play_count` that have no letter left. And its `playbar_status_source` template has placeholders that arrive with
  their own `| Shuffle:` prefix baked in, so the template author cannot control layout: the concrete argument for §7's
  **conditional groups** being non-optional rather than a nicety.

  A rejection was also corroborated. audium uses rodio, and its feature list has seeking, speed and loop modes but **no
  gapless** — exactly the gap §13's "rodio's `Source` abstraction blocks gapless, precise seek, and controlling buffer
  depth" predicted.

  Finally, spotatui is worth keeping as a mirror: a well-built TUI that began as a Spotify client and now carries six
  playback sources, synced lyrics, a visualiser, an AI DJ and a Lua plugin runtime. §1 opens by naming scope creep as
  this project's risk; that is what the far end of the road looks like from outside.

- **2026-08-29** **§1 amended: "no network sources" narrowed to "no services".** The out-of-scope list is the part of
  this brief with the strongest instruction attached — CLAUDE.md says it stays out _"even when a crate makes it
  trivial"_ — so weakening it is a decision recorded here rather than a line quietly deleted.

  The original rule bundled two different things under one prohibition: **services** (Spotify, YouTube, yt-dlp — an
  account, a catalogue, and an interest in what you play) and **your own library reached over a socket** (a NAS, a Plex
  server). The first is what §1 was defending against. The second is the same music this player was built for, on other
  hardware.

  Narrowed accordingly. Streaming services stay out permanently. A personal media server — browse, stream, and sync
  ratings and playlists back — is now in scope, with three constraints that are the actual guard rail:

  1. **Local first is not negotiable.** Every feature works with no network. A disconnected gmuse is a complete player,
     not a degraded one.
  2. **No account, no catalogue, no recommendations from anyone else's server.**
  3. **Opt-in, behind a feature flag**, so the default build is unchanged and an HTTP client is a dependency recorded
     like any other.

  The pattern already established twice — rsgain computes ReplayGain and we read the tags; bliss would analyse and we
  read the result — applies here too and is the first thing to try: a **sync tool** that moves files and metadata, with
  gmuse staying a local player, is a smaller change than a network source and preserves §1 almost entirely. See the
  tracking issue.

  Recorded honestly: this is scope growth, and §1 opens by naming scope creep as this project's failure mode. The
  defence is that the boundary moved to a _principled_ place — one's own files versus somebody's service — rather than
  eroding by increments. Judged by that standard, the next request should be argued against the same line.

- **2026-08-29** **§1 gained a statement of what the interface is _for_**: gmuse is a way to interact with music, not a
  thing to look at — keyboard-driven, off screen most of the time, judged by whether the next action is cheap.

  Written down because it decides things. It is the reason there are no visualizers or album art, and it is equally the
  reason the remote socket, media keys and session resumption matter more than their size suggests: the interface must
  be usable when it is not in front of you. For an arguable feature the question is now explicit — does this make acting
  faster, or only make looking nicer?

- **2026-08-29** **The log records what happened, not what was asked for (issue #17).** Reading a 75-minute session
  found the ReplayGain figure on the `next` (splice) line and nowhere else. 21 of 27 tracks arrived by splice and had
  their gain recorded; the 6 played by hand did not — and a manually played track is disproportionately the one being
  investigated, because it was chosen on purpose. A diagnostic covering 78% of cases reads as complete while hiding the
  interesting one.

  The gain moved to `loaded`, which fires for every track that becomes audible, and it is now **reported by the decoder
  rather than remembered by the caller**: `PlayerEvent::Loaded` carries the gain the decoder is actually multiplying by.
  The two could only diverge if the splice stopped carrying the gain across — which is exactly the bug worth catching,
  and is what the test mutates to check.

  **The mode was solved by not plumbing it.** `replaygain=auto` resolves per track against `shuffle`, so the obvious
  move is to thread the resolved choice through to the log. But the mode is a _setting_, not a property of a track:
  logging every `apply_setting` and a summary line at startup records the whole state trajectory, and the per-load gain
  records its effect. Together they answer the question with no per-track plumbing at all.

  The startup summary is emitted **after** the config's commands have run. Written before them — which is where it first
  went — it recorded the defaults rather than what was in force, which is a diagnostic that lies about precisely the
  thing it exists to report.

  Also noted, not fixed: `--log` with `--repl` silently does nothing, because the audit is only wired in the TUI path.
  The line prompt prints gain and decibels in its own `status` output, so there is somewhere to look, but the flag
  should either work or say it does not apply.

- **2026-08-29** **Session resumption (issue #16), and the first use of `$XDG_STATE_HOME` for durable state.** Queue,
  current track, position and modes come back on the next launch, **paused** — starting playback unbidden would be
  obnoxious. It is §1's "the interface has to be usable when it is not in front of you" cashed in: a player that forgets
  where you were every launch is the opposite of that.

  **The shuffle seed is the interesting part.** §5 makes shuffle a seeded permutation, so the order being heard is a
  function of `rng_state` and the pool. Restoring the _mode_ without the _seed_ resumes a different sequence while
  looking entirely correct — one `u64` is the difference between resuming a shuffle and restarting one. It also has to
  be restored **after** shuffle, because setting shuffle reseeds; the test mutates that ordering to prove it.

  **Precedence: the config wins where it speaks, the session fills the rest.** Writing a setting in `config.toml` should
  take effect immediately rather than being shadowed by a stale session file — the confusing failure is editing a config
  and seeing nothing happen. The keys the config named are collected from its own `Set` commands before they run, which
  reuses the existing `active_specified` idea: _what the config said_ is distinct from _the value it produced_, and only
  the former should outrank memory.

  **The store is `$XDG_STATE_HOME/gmuse/session.toml`, written temp-then-rename.** Not the cache: this cannot be rebuilt
  by rescanning, and the Decision Log records what happened last time durable data shared a lifetime with derived data.
  Not keyed by `TrackId` either — an arena index is meaningful only while the cache that produced it survives, so a
  queue keyed by id would silently restore _other tracks_. Paths, with anything missing skipped and counted rather than
  treated as an error.

  **A bug found by writing it down rather than by a test.** Restoring loads the track, and loading emits `Loaded`, which
  is a save point — but at that instant the position is still 0, because the decoder has not yet processed the seek that
  follows. The first save would therefore overwrite the very position being resumed, and it would present as "resumption
  does not work". A one-shot guard skips that first `Loaded`; the test now fails without it.

  Saving happens at every track boundary as well as on quit. On quit alone, a crash loses the whole session — which is
  precisely the session worth having, the same reasoning that made the audit log flush per line.

- **2026-08-29** **The completion menu filters as you type (issue #10).** Typing used to dismiss it, which is backwards:
  you type to home in on something, and throwing the list away is the opposite of that. Backspace widens it again, and a
  line matching nothing leaves the menu **open** saying `no matches` — a list that simply vanished reads as a swallowed
  keystroke.

  **The structural blocker was that `candidates.is_empty()` meant "closed".** "Open with nothing matching" — the exact
  state the notice describes — was unrepresentable. It is now `menu: Option<Vec<String>>`, where `Some(vec![])` is that
  state and `None` is closed. Small change, but every `menu_open` check, the height calculation and the draw all read
  from it.

  **Deriving the candidates is what makes Backspace free.** The list is a view of the buffer, so it is recomputed from
  scratch on every keystroke rather than whittled down. A shorter buffer simply matches more and there is nothing to
  undo; filtering a stored list would have needed both the original set _and_ the prefix the menu opened on. This is the
  same rule as the `v`/`c` shadow, the pending-key indicator and the menu's own scroll offset — the fifth time it has
  decided a design here, which is enough to call it a house rule rather than a recurring observation.

  Filtering deliberately **does not extend the line**. `Tab` is the only key that edits what was typed; narrowing that
  silently completed for you would be the rule against eating what someone is typing, broken from the other direction.

  `set key=value` needed the same split-inside-the-word that completion does, or typing a value narrows against verbs.
  Worth recording how that surfaced: the test asserting it failed, and the failure was **the test's fault, not the
  code's** — `Tab` had already extended `set aaa_mode=` to `set aaa_mode=a`, so the keystrokes it sent produced
  `aaa_mode=aal`. Reading the actual buffer rather than assuming it turned a false alarm into a passing test that covers
  the real path, and a mutation confirms the split is load-bearing.

- **2026-08-30** **Format strings, half of M8 (§7). Named placeholders, not `%a`.** §7 specified cmus-style
  `%a - %l - %02n. %t`; it now specifies `{artist} — {title}`. Command _names_ were kept close to cmus because a cmus
  user types them and `:bind` strings quote them, but nobody carries a format string between players, so the
  compatibility argument that saved `win-up` does not apply here. Against it: the alphabet runs out.
  `%a %l %t %n %y %d %g` are already taken and the fields worth adding next — `play_count`, `rating`, `replaygain` —
  have no sensible letter left. spotify-player and spotatui both use named placeholders.

  **`[…]` groups are the feature that earns the language.** A group is emitted only if every field inside it has a
  value, so `{artist}[ — {album}]` drops the separator along with the album rather than leaving `Sierra Ferrell — `
  behind. Without it the caller needs a conditional per field, which is what a format string exists to avoid — and it is
  the concrete thing `docs/inspiration/spotatui.md` records going wrong elsewhere, where placeholders arrive with their
  separators baked in and the template author cannot control layout.

  **Escaping is backslash, not doubling, and that was forced by a failing test.** Doubling is what Rust's own format
  strings use and it cannot work here: `]]` is ambiguous between a literal `]` and two nested groups closing together,
  so `[{album}[ ({year})]]` would not parse. Rust avoids this only by having nothing that nests. The test for nesting is
  what surfaced it — worth noting because the obvious reading of a failing test is that the test is wrong, and this time
  it was the design.

  Scope: **the now-playing description only**. The state icon, elapsed/total and mode flags keep their own theme roles
  and stay structured, because one template for the whole line would flatten per-segment styling unless placeholders
  could carry `Role`s — a much larger design for no benefit yet. Track-list rows are the obvious next surface and are
  really spotatui's "columns as config" idea, which is a different feature.

  Templates are parsed in `config.rs`, so a bad one is a reported problem naming `format.now_playing` rather than
  something the renderer discovers per frame, and the default survives a rejected template.

  A note on the mutation testing, since it went wrong twice in the same way: two of the mutations written to check these
  tests were **inert** — one replaced an identical-looking `ConfigError` block earlier in the file, because
  `replace(..., 1)` takes the first match. That is the third time this session a passing mutation meant a bad mutation
  rather than a weak test. **Check the mutation landed where it was aimed before drawing any conclusion from it.**

- **2026-08-30** **The filter language, designed (§7) — and catalogs turn out to be filters already.** The log warned
  twice that "a catalog defined by a filter expression is a named, persistent filter; building the filter language first
  without that in mind risks two mechanisms for one idea". Reading cmus's manual made the collision exact rather than
  theoretical:

  | cmus                           | gmuse today                       |
  | ------------------------------ | --------------------------------- |
  | `fset dance=…`                 | `[catalogs.dance] collection = …` |
  | `factivate dance`              | `:catalog dance`                  |
  | view 6 lists them, space/enter | view 6 is a stub saying "M8"      |

  **We had already built cmus's named filters and called them catalogs.** That is a good outcome: the concept was right
  and only the membership rule was narrow. So a catalog becomes a named filter expression and view 6 finally has a job.
  (`roots` stayed as sugar at the time; #94 removed it — see 2026-09-05.)

  The distinction that survives is not catalog-versus-filter but **lifetime** (named and persistent versus transient and
  anonymous) and **role** (narrows the world versus adds a play source — which is the line to smart playlists, #15). One
  language, three usages.

  **The combining rule differs by role, and that is deliberate.** Active catalogs union; a transient filter intersects.
  Under a single rule `:catalog folk pop` would either break (AND: no track is both) or filters would stop narrowing
  (OR). "Which bodies of music" is a selection, "what am I looking for" is a restriction.

  **Syntax: terse, cmus-shaped**, chosen after seeing the alternatives written out. MPD spells its operators
  (`contains`, `starts_with`) and is the most explicit of the three tools surveyed — but it has **no `OR` at all**,
  which is disqualifying when `genre="jazz"|genre="blues"` is exactly the catalog one wants. beets is the tersest and
  most implicit (bare words search everything, space means AND). cmus sits between and reads well for something typed at
  a prompt.

- **2026-08-30** **Globs, not regex — decided on the library's own data, not on taste.** Regex was the obvious candidate
  for string matching and is the wrong one here, because music metadata is made of regex metacharacters. Counted across
  170 distinct tag values in the reference library:

  | character       | values containing it |
  | --------------- | -------------------- |
  | `.`             | **19**               |
  | `(` and `)`     | **23**               |
  | `?`             | 1                    |
  | `*`             | **0**                |
  | `[` `+` `^` `$` | 0                    |

  The failures are silent, which is what makes them disqualifying rather than merely inconvenient:

  - `Burial (S/T)` as a regex matches `Burial S/T` and **not** the actual album — a silent miss.
  - `Have You Got It In You?` — the `?` makes the preceding letter optional, so the literal title does not match.
  - `Anderson .Paak` — `.` matches any character, so it also matches `Anderson XPaak` — a silent over-match.

  Nothing errors; you simply get the wrong tracks. With globs, `*` collides with **nothing** in this library, and `?` is
  deliberately _not_ treated as a metacharacter: glob's "exactly one character" is never wanted from a music search,
  while a question mark in a title is real. That leaves the language collision-free on real data.

  Regex is deferred rather than rejected. If globs prove insufficient it arrives as a distinct `=~`, opt-in, at the cost
  of the `regex` crate and of `|` meaning two things on one line.

  **Smart-case**, as in vim and ripgrep: insensitive unless the pattern contains a capital. Tags are inconsistently
  capitalised, so `genre="jazz"` must find "Jazz"; `artist="Burial"` stays precise.

  **No bare words.** `any="*ferrell*"` searches every text field, and `any` is a real field that composes with the rest
  — the convenience without the one implicit thing in an otherwise explicit language.

  Left open on purpose: **whether a transient `:filter` survives a restart**. Session resumption now remembers modes and
  a filter is arguably one, but "transient" and "restored on launch" are in tension and the answer is clearer after
  living with it. Also deferred: **runtime-defined named filters** (cmus's `fset`), which need somewhere to persist —
  the same durable store #13 and #14 want, and not worth inventing twice.

- **2026-08-30** **The filter language implemented (`src/filter.rs`), and `:filter` wired in.** Built to the spec
  settled earlier the same day; the notable part is what the reference library confirmed once it could be run against.

  `album="Burial (S/T)"` finds its 13 tracks and `title="*In You?"` finds the one — the two cases that decided globs
  over regex, now demonstrably working rather than argued about. `path="*/Dance/*"` gives 47, `filename="*.opus"` five,
  `any="*burial*"` thirteen.

  **And a finding that mattered: the library had no genre tags at all.** `genre="*folk*"` returned nothing across all
  148 files. So `[catalogs.jazz] where = 'genre="jazz"'` — the headline example of why catalogs should become
  filter-defined — could not have worked, and folder-based catalogs were not a limitation being worked around but the
  only thing the tag data supported.

  **Fixed the same day, outside the program.** Genres were written with `mutagen`, per album, as compound values —
  `Electronic; Dubstep`, `Americana; Bluegrass` — which suits glob matching exactly: `genre="*electronic*"` finds all 59
  electronic tracks and `genre="*dubstep*"` narrows to Burial's 13. Verified afterwards that all 148 files kept their
  ReplayGain tags; `mutagen` edits in place, where an ffmpeg remux would have risked them.

  That is the third time the pattern has held: **the heavy or file-modifying work happens outside gmuse and gmuse reads
  the result** — rsgain for loudness, mutagen for genres, and bliss for similarity if #19 ever happens. §1's "tag
  editing (reading only)" is not a gap to route around; it is what makes each of those a separate, replaceable tool.

  Two implementation decisions worth recording. **`Filter` carries its source text** alongside the parsed AST, because
  §6 requires a command to round-trip through text and rendering the AST would turn `genre=folk` into `genre="folk"` — a
  different line for the same meaning. Parsing still happens at the boundary; nothing downstream sees the string.

  **A missing tag matches nothing, including `!=`.** Three-valued logic is more correct and much harder to predict; `!`
  on the whole comparison is how to ask for "or missing". Pinned by a test, because the alternative is defensible enough
  that someone will eventually "fix" it.

  Unquoted values are accepted where unambiguous (`genre=folk`, `year>2020`) — not an implicit rule, because the _field_
  determines the type, so there is nothing to guess.

- **2026-08-30** **`field?` for presence, after measuring this language against foobar2000 and iTunes.** The comparison
  was worth doing, and mostly reassuring:

  |                        | foobar2000                           | iTunes                    | gmuse                         |
  | ---------------------- | ------------------------------------ | ------------------------- | ----------------------------- |
  | booleans and grouping  | `AND OR NOT ()`                      | match all/any, nested     | parity                        |
  | substring              | `HAS` (word set)                     | _contains_                | `"*x*"`                       |
  | prefix / suffix        | —                                    | _begins/ends with_        | **ahead of foobar**           |
  | case                   | always insensitive                   | always insensitive        | **smart-case, ahead of both** |
  | present / missing      | `PRESENT` / `MISSING`                | —                         | **was the gap**               |
  | dates, play statistics | `SINCE`, `DURING LAST n DAYS`        | _in the last n days_      | absent                        |
  | sort / limit           | `SORT BY`                            | _limit to N, selected by_ | absent                        |
  | field vs field         | `%last_played% AFTER %first_played%` | —                         | absent                        |

  **Three of the four absences are not language problems.** Dates and play statistics are a _data_ gap — the grammar
  would express them today if #13 and #14 existed. Sort and limit belong to smart playlists, not filters: a filter
  answers "which tracks", a smart playlist answers "which tracks, in what order, how many", and folding `SORT BY` into
  `:filter` would blur a line drawn on purpose. Field-to-field comparison is genuinely missing and genuinely rare.

  Presence was the real one — and checking rather than assuming showed it was already _expressible_: `genre="*"` works
  because a missing tag matches nothing, and `year>=0` is the numeric equivalent. But nobody would derive that, and
  **text and numbers needed different spellings**, which is the actual defect. `field?` gives one form for both.

  An empty tag counts as missing. A `GENRE=""` frame is a tagging artefact rather than an answer, and treating it as
  present would make `!genre?` miss exactly the files worth finding.

  The `?` is detected on the **field word only**, so `title=You?` still compares against a literal question mark — the
  design alternative, lexing `?` as its own token, is rejected by four tests.

  Not taken from foobar: **`HAS` word-set matching**, where `artist HAS sierra ferrell` matches "Ferrell, Sierra"
  regardless of order. Expressible here as `artist="*sierra*"&artist="*ferrell*"`, which is wordier but composes with
  everything else rather than being a special operator.

- **2026-08-30** **foobar2000 added to `docs/inspiration/`, for a reason none of the others are there for.** It is not a
  TUI and its interface and engine are irrelevant; it is read for its **two mini-languages**, which are the most
  developed of their kind, and as somewhere to look when a power-user question arrives that this project has not
  reached. The folder's README now says so, because the existing three-point remit does not cover it.

  **Its title-formatting conditional sections are `[…]`, exactly ours**, with the same rule — the section disappears if
  a field inside is undefined. `%artist% - [%album% / ]%title%` is our `{artist} - [{album} / ]{title}`. That was
  arrived at here independently, from the failure mode recorded in `spotatui.md`, and finding the same answer in the
  language with the most use behind it is a good signal.

  Beyond that it is a full expression language and ours is a template: `$if`, `$ifgreater`, `$upper`, `$cut`, `$padcut`,
  arithmetic, and `$puts`/`$get` variables.

  **The observation worth keeping: three things we hardcoded are functions there.** `file_under` is
  `$if2(%albumartist%,%artist%)`, `display_title` is `$if2(%title%,%filename%)`, and `smart_artist_sort` is
  `$swapprefix`. Because the format language cannot express a fallback, each convenience had to become a field or a
  setting — which is why the field list keeps growing, and why each new one needs code. Not obviously the wrong trade:
  `{file_under}` is friendlier than `$if2(…)` and cannot be got wrong. But it explains the shape of the thing.

  **Truncation is the one real gap.** The status line is one line and a long template is simply clipped by the terminal;
  `{title:.30}` would be the natural spelling for foobar's `$padcut`. Worth doing.

  **The rest is deliberately declined.** Variables, arithmetic and nested conditionals add up to a programming language
  living in a config file, and foobar's power-user configurations are notoriously unreadable for exactly that reason. §1
  names scope creep as this project's failure mode; a format string that needs debugging is what that looks like here.

- **2026-08-30** **Truncation in the format language (`{title:.40}`), the one gap the foobar2000 comparison found.** The
  status line is a single line, so without it a long template is clipped by the terminal at whatever column it runs out
  — the author has no say in _what_ gets lost. `$padcut` is foobar's answer and this is the same idea in this language's
  spelling.

  A spec is now `[>|0]? min? ( '.' max )?`. The value is **padded to the minimum, then cut to the maximum**, so a
  contradiction like `{artist:20.4}` resolves to four characters rather than being an error: the maximum always wins,
  which is one sentence and stays predictable.

  Cutting is by **characters, not bytes** — half of a multi-byte character is not a string, and this library is full of
  them (`Christian Löffler`). Pinned by a test, because `&s[..max]` is the obvious wrong implementation and panics
  rather than misbehaving quietly, which is at least loud but still a crash in a render loop.

  **No ellipsis.** It would be friendlier, and it would make the rendered width depend on whether the cut happened,
  which is worse in a line that is already tight. A spec that sizes nothing (`{track:0}`, `{title:.}`) is rejected: it
  is a typo, not a rule.

- **2026-08-30** **A track with no album files under `singles`, and its tag stays empty.** Lowercase on purpose: album
  titles are capitalised, so it reads as a category rather than as a record called "Singles" — and there is no such
  record, which is the point of leaving the tag alone.

  The alternative was to _write_ `album = "Singles"` into the files. It looks identical on screen and costs no code, and
  it is a lie: those tracks are not from an album of that name, and it would break `!album?` as the way to find them.
  Presentation belongs in the player; the files stay true. The same reasoning that keeps analysis outside gmuse.

  `Track::album` is still `None`, so the format language's `[{album}]` groups vanish and the filter's `!album?` finds
  exactly these tracks. **A mutation caught that this claim was untested** — swapping the filter to read
  `album_or_unknown()` broke nothing until a test was added for it, and that swap is precisely the mistake that would
  make every single look like it belonged to a record.

  Layout, from issue #20: a shared `Singles/` folder **per catalog** — `Dance/Singles/`, `General/Singles/` — because
  the singles span both and one top-level folder would be a third catalog mixing dubstep with folk, which is what
  catalogs exist to prevent.

  Tagged with **`rsgain custom`, never `rsgain easy`**: easy mode computes an album gain per folder, and a folder of
  unrelated singles would get an average across tracks with nothing to do with each other. With no album gain written,
  `replaygain=auto` falls back to the track figure on its own — that fallback already existed for track-only scans.

  Importing the five pending files found something worth recording: **only one of them was actually album-less.** The
  Bug's "Poison Dart" is track 11 of _London Zoo_, ANTIGONE's "DANCE" is track 6 of a compilation, and two are genuine
  single releases whose album tag equals their title. So a `Singles/` folder is about **provenance — loose tracks not
  part of a complete album owned — not about the tracks lacking an album.** Their tags were left alone.

- **2026-08-30** **Per-track history (`src/history.rs`, issue #13): plays, last played, first seen.** The third store in
  `$XDG_STATE_HOME`, after the audit log and the session, and built to the same rules — durable rather than derived,
  keyed by **path** rather than `TrackId`, written temp-then-rename. Nothing new was invented: `session.toml` had
  already settled the shape, which was the argument for doing resumption first.

  **A play is half the track or four minutes, whichever comes first** — last.fm's rule and the usual one. A track of
  unknown length has no half to take, so it must earn the full four minutes; without that guard a zero duration counts a
  play the instant it starts. Counted once per track, polled from the render loop rather than driven by an event,
  because "half way through" is not something the player emits.

  **`added` can only start from now.** The obvious source for an existing library is the file's mtime, and it is useless
  here: writing ReplayGain tags across the library reset every mtime in an afternoon, which this project did to itself
  earlier today. So the first run stamps everything with the current time and says so in the log. The field becomes
  meaningful from that point forward, and there is no way to recover what it would have been.

  **A mutation exposed an untestable guard, and the fix was to move the decision.** `note_play_progress` read the
  position from the player and then decided; with no player in the tests it returned early, so removing the once-only
  guard broke nothing. Splitting it into `record_play_if_earned(ui, ctx, position)` — the decision, with the position
  passed in — made the guard reachable, and three mutations now fail against it. The remaining hole is honest: clearing
  the guard at a track boundary happens in the `Loaded` arm, which still needs a real device.

  Not yet exposed to the filter language. `play_count` and `last_played` as filter fields need the predicate to see more
  than a `&Track`, which is a signature change worth its own piece of work — and it is what #15 actually wants.

- **2026-08-30** **History reaches the filter language: `play_count`, `days_since_played`, `days_since_added`.** This is
  what turns #13 from infrastructure into something usable, and it is the prerequisite #15 was actually waiting on.

  **Dates are exposed as "days since", not as timestamps.** `days_since_added<30` reads; `added>1735689600` does not. It
  also avoids date literals in the grammar entirely — foobar2000 needs `BEFORE`, `AFTER`, `SINCE`, `DURING`, and
  `DURING LAST n DAYS` to say what two integer comparisons say here.

  **Missing and zero are kept apart, deliberately.** A track never played has _no_ `days_since_played`, so
  `!days_since_played?` finds exactly those — reading it as zero would make them look freshly played. But `play_count`
  is `0` for them, because "never" is an answer rather than an absence. With no history at all — the line prompt keeps
  none — every history field is missing, because "cannot say" is not "never". Three mutations pin these apart.

  The predicate signature changed from `Fn(&Track) -> bool` to taking an `Env { history, now }`, since history lives in
  its own store keyed by path rather than on `Track`. That reached `Catalogs::members`, which is how a catalog can now
  be `where = "play_count=0"`. Verified against the reference library with a synthetic history: 279 unplayed of 311, 32
  played this week, 176 unplayed in the Dance folder.

  **History is loaded before the library is built, not after.** A catalog may be defined by `play_count`, and building
  the projection without the history would silently give the wrong membership until something happened to trigger a
  rebuild. Ordering, again, being the thing that makes a diagnostic or a projection quietly wrong.

  Not added to the **format** language. Those fields render from a `&Track` alone, and giving them the same environment
  is a separate change — the shared vocabulary is for the file's own fields, and `any` was already filter-only.

- **2026-08-30** **Ratings (issue #14): ours, not the file's.** `:rate 1`–`5`, `:rate none` to clear, a bare `:rate` to
  report, and `rating` as a filter field. Stored in the history file beside plays and first-seen — the same store, as
  the issue argued, rather than a second one.

  **§1 holds: gmuse still writes no tags.** trollstov carves out exactly this exception and says why — a rating in the
  file travels with the music — and audium writes metadata wholesale. Both were read before deciding. The trade taken
  here is portability lost, and never being the program that rewrote someone's library gained. #14 records that the door
  is not closed; reopening it would be an amendment to §1 with an entry of its own, not a quiet exception.

  **Reading existing `POPM` tags was dropped after checking, not after arguing.** lofty exposes them and it would have
  cost little — but **0 of 80 sampled files carry one**, so it would have been speculative work delivering nothing. The
  star scale is also a genuine mess: lofty ships per-player constructors (`windows_media_player`, `musicbee`, `picard`)
  because the same byte means different star counts, which is a second reason to keep our own value authoritative if the
  two ever meet.

  **A rating is 1 to 5, or absent — zero is not a value.** `!rating?` finds what has not been judged; reading unrated as
  nought stars would make every untouched track look actively disliked, which is a different claim. `:rate none` clears;
  **`:rate 0` is refused**, because it reads as setting a value and there is no such value.

  That left one incoherence, spotted in review rather than by a test: **`rating=0` was a query that could never match**.
  The `Option` is not the redundant part — it carries the distinction between _never judged_ and _judged and disliked_,
  which collapsing to zero would lose. The mistake was letting `0` be _spelled_ as a rating value. So a comparison of
  `rating` against zero is now a parse error naming the idiom: "`rating?` is rated, `!rating?` is not". Every such
  comparison was either dead (`=0`, `<0`) or a long way of writing `rating?` (`>0`), and it is the first thing someone
  reaching for "unrated" will type.

  The same rule then had to hold in the command language, which review caught straight after: `:rate 0` was still
  spelling a clear as a value. Zero now means nothing anywhere — **one rule stated once, rather than a filter rule and a
  command rule that happen to agree.**

  Deliberately narrow: `play_count=0` and `days_since_played=0` stay valid, because zero plays and played-today are real
  answers. Mutations check both directions — accepting zero again, and widening the rejection to every history field.

  `:rate` acts on **what is playing**, falling back to the cursor when nothing is — rating is something done to a song
  being heard, and the fallback is what makes it work while browsing. Written immediately rather than at exit: a
  judgement is worth more than a play count and losing one to a crash would be worse.

- **2026-08-30** **A smart playlist is a source, not a narrowing** — and saving is not what decides that. Catalogs are
  saved too, and they narrow. The test is exclusivity: while a catalog is active the rest of the library is unreachable
  and shuffle cannot leave it; a playlist sits alongside the library as one more place to play from. The decisive
  argument is that a saved expression which narrows **already exists** — `[catalogs.x] where = "…"` — so making smart
  playlists narrow would be the "two mechanisms for one idea" this log has now warned about three times. Membership is
  therefore evaluated over the `Store`, inheriting §4's rule that a playlist is not filtered by the active set.

  Evaluated **on use**, as foobar2000 does, rather than materialised: a filter pass is 0.92 ms at 20k tracks, so there
  is nothing to buy by caching and drift to lose. Which in turn is why it is **not editable** — a per-track exception
  list bolted onto a rule is how these become unexplainable. `:freeze` drops the rule and keeps the tracks.

- **2026-08-30** **Sort is a named vocabulary, shared with the filter and format languages** — a comma-separated list
  applied left to right, `-` for descending, plus `random`. foobar2000 sorts by an arbitrary title-format pattern, which
  is more general and sorts track 10 before track 2 unless you remember `$num()`; iTunes offers a fixed menu that does
  not compose. Type-aware keys give both. Missing values sort **last in both directions**, which costs strict
  reversibility and buys never opening a list with a screen of blanks; ties break by path so the order is total.

- **2026-08-30** **The two languages spell dates differently, on purpose.** A filter says `days_since_added`, a sort
  says `added`. `days_since` reads in a question and inverts in an order — ascending means newest first — so
  `-days_since_added` written for "newest" returns the oldest, with no error and a list that looks plausible. A wrong
  guess in either language now costs one message naming the other spelling. This is the first place the shared
  vocabulary has been deliberately broken, and the reason is that the failure it prevents is **silent**.

- **2026-08-30** **Durable data is split by worth, not by kind, and the precious half is a setting.** `history.toml`
  moves from `$XDG_STATE_HOME` to `data_dir`, defaulting to `$XDG_DATA_HOME/gmuse`; `session.toml` and the audit log
  stay behind. The line is whether the thing can be recovered by using the program again — where you were can, what you
  decided cannot. Ratings were sitting in the directory a backup tool skips, and §1 keeping tag editing out of scope is
  precisely why they have nowhere else to be.

  A setting rather than a fixed path because pointing it inside the library makes ratings travel with the music, and
  that is a personal answer. It cannot _default_ there: gmuse has no concept of "the library folder" — roots may be
  several and on different disks, and a playlist crosses them by design.

  A legacy history is **moved**, not copied, and the move is reported. Falling back to a read of the old location would
  have been easier and wrong: a version that silently started from an empty history looks exactly like the ratings being
  gone.

- **2026-08-30** **Moves are detected at scan time, and ambiguity is declined.** Everything durable is keyed by path,
  which is the right key — an arena index is meaningful only while the cache that produced it survives — but nothing
  noticed when the key changed. A reorganisation orphaned ratings, reset play counts, and reset `added`, so a file owned
  for a year reappeared in `days_since_added<30`. Worst of all it was **silent**: the scan reported one addition and one
  removal, which is exactly what a correct scan of a real addition and a real deletion prints.

  A departure is paired with an arrival on a signature of size, duration, audio properties and tags — **only when that
  signature is unique on both sides.** Two copies of one track, or two untagged files of the same length, would
  otherwise be paired arbitrarily, and a wrong pairing attaches somebody's rating to the wrong song, which is worse than
  the loss being prevented.

  `mtime` is deliberately excluded, though it would make the signature stricter: a plain `mv` preserves it, but a copy
  without `-p`, an rsync without `-a`, and a restore from backup all reset it — and those are exactly the
  reorganisations worth surviving. `Track` gained `size` for this, which is what forced a cache format bump; `size`
  carries most of the signature's weight because for a byte-identical move it is conclusive.

  The scan **says** what it did. A reassociation the listener never sees is as bad as the silent loss it replaces, and
  worse if it ever guesses wrong.

- **2026-08-30** **Ratings and plays are events, not state.** `history.toml` held the answer and rewrote the whole file
  to change it — fine on one machine, lossy on two. Play a track on the laptop and rate one on the desktop, and
  whichever file wins the sync takes everything the other machine did with it; the blast radius of a whole-file write is
  the library, not the track.

  Now the base is written once and never again, and each machine appends to `events-<machine>.log`. No two writers touch
  one file, so a sync has nothing to resolve. Reading is `base + fold(all logs, in timestamp order)`. Plays are counted
  so two machines' plays add up; a rating is the latest by timestamp, which where the conflict is genuine at least gives
  both machines the _same_ answer, which two state files could not promise.

  The machine id lives in `$XDG_STATE_HOME`, **not** `data_dir` — the latter is the directory being synced, so both
  machines would read one id and collide precisely where it matters. It is generated rather than taken from the
  environment: neither `HOSTNAME` nor `HOST` is exported to a child process on macOS or under zsh, so every machine
  would have answered "local" and shared one log.

  **Log decisions, not interactions.** A play and a rating are things the listener meant; a seek is not. That rule is
  what keeps growth proportional to listening — measured, a year is ~4 MB folding in 4.6 ms and thirty years 62 ms, so
  compaction is a tidiness question and not a performance one. `History::save` survives unused as the other half of a
  compaction that is deliberately not built: truncating a log cannot be made safe without deciding what it means to a
  machine that has not synced yet.

- **2026-08-30** **Roots come from the config; the cache is a fallback, not an accumulator.** `main` seeded its root
  list from the cache and only ever appended, so a one-off `--library` stayed for ever. A stale root that happened to be
  the _parent_ of two real catalog roots then made `scan` walk every file twice and count it twice — which is how a
  311-track library announced `622 tracks` on every launch.

  Two fixes, because there were two faults. Cached roots are used **only when the config and `--library` supply none**,
  which keeps "scan once with `--library`, run bare after" working while stopping anything from lingering. And `scan`
  drops any root lying under another before walking, using `Path::starts_with` — component-wise, so `/m/Bach` is not
  inside `/m/B`, which string prefixes would have got wrong.

  `--library` now expands a leading `~`, as config roots always did. A shell expands it before gmuse sees it, but
  `GMUSE_LIBRARY=~/Music` does not, and a literal `~/Music` walks nothing at all — silently. One `expand_tilde`, shared,
  rather than two that disagree.

  The first attempt at testing this proved the pruning helper and not that `scan` called it: deleting the call left
  every test green. The tests that matter write a real WAV to a temp tree and scan overlapping roots, which is the only
  version that fails when the wiring goes.

- **2026-08-30** **Playlists are `.m3u8` files in a directory, and the directory is the index.** Not a format of our
  own: a playlist _is_ an ordered list of paths, which is exactly what M3U is, and M3U is what every other program
  reads. XSPF is technically better and never got adopted. Inventing one would have bought nothing and cost the ability
  to open the file anywhere else — including Plex (#18). We write `.m3u8` (the `8` declares UTF-8, the format's one
  genuine ambiguity) and read `.m3u`, `.m3u8` and cmus's `.pl`, which is a strict subset.

  No index file: the set of playlists is whatever the directory holds, so one dropped in from another program appears
  without being imported. An index would be a second source of truth to keep in step.

  Paths are written **relative to the playlist file**, so a library and its playlists move together — which is the point
  of keeping them under `data_dir`. `#EXTINF` is write-only: emitted because other programs use it, ignored on read
  because our own tags are better and a metadata cache in a text file goes stale.

- **2026-08-30** **`:playlist` takes subcommands, and a bare name is refused.** `open`, `save`, `list` — one verb, so
  everything playlist-shaped is discoverable from one place and `:playlist <Tab>` shows the lot. `open` takes a rule
  (#15) or a saved file (#22) without caring which, because §7 settled that both are play _sources_; sharing one
  vocabulary is the whole reason they do not collide.

  The bare `:playlist <name>` that shipped that morning is rejected rather than kept as a shorthand. Keeping it would
  mean a playlist called `save` could never be opened — a small, permanent ambiguity — and the error names the
  replacement.

- **2026-08-30** **The session remembers view 3.** It remembered the _queue_, which is consumed as it plays and is
  explicitly a one-shot, and not the playlist, so a list assembled deliberately was lost while the throwaway one
  survived. A rule-owned list is deliberately **not** captured: it is recomputed from its rule on open, and freezing a
  stale copy into the session would make it lie.

- **2026-08-30** **View 3 is two panes, not two views.** A first attempt made the playlist _browser_ view 8, beside view
  3's open playlist. That gave the tab strip `3:playlist` and `8:playlists` — two tabs for one subject, and a
  distinction nobody wants to hold. The set and the open one belong in one view, split the way view 1 already splits
  artists from tracks: pick on the left, see it on the right. `tab`, `pane-left`/`pane-right` and the `in <pane>`
  wrapper all work here without a new key, because the wrapper is about _panes_ rather than about the library.

  That took the count back to seven views, and made #23's other question moot — the numbering only grouped badly because
  of the tab that should not have existed.

  Two consequences worth stating, both simplifications of what shipped hours earlier:

  **There is no unnamed working playlist.** Adding means adding to _the one you picked_, so `:playlist new <name>`
  starts one and `y` refuses when nothing is open, naming that remedy rather than inventing a file. An invisible scratch
  list was the alternative and it is worse: it cannot be got back to once something else is opened.

  **The session stores the open playlist's name, not its contents.** Every playlist is a file or a rule, so the tracks
  have a home of their own and a copy in the session could only go stale. Reopening by name gets what the file says
  today, which is what you want after editing it in another program.

  Edits are **written through** on every `y` and `D`, the rule ratings and plays already follow. `:playlist save` is
  therefore not "write this out" but _save-as_ — copy the open one, which is also how a rule's current contents become
  editable.

  `Open { Rule(String), Saved(String) }` replaced a `smart: Option<String>` that would have needed a second, mutually
  exclusive `Option` beside it. Two `Option`s that cannot both be `Some` are a state that can lie; one enum cannot.

- **2026-08-30** **View 6 is `scope`, not `filters`.** It lists catalogs and shows the active `:filter` as context, so
  it was named after the smaller half — and the name collided with a command that does something else entirely. `scope`
  covers both halves: everything narrowing the library right now.

- **2026-08-30** **View 7 is editable, and the relative gutter reaches the tree.** A view that shows what can change and
  refuses to change it is a _display_, and §4's argument for a view is that it is a place to do something — the test #23
  proposes, which a read-only settings list failed. `space` and `Enter` now change the row under the cursor: booleans
  flip, named values cycle, which is what `:toggle` already means, so this is a way in rather than a second mechanism.
  Catalog rows toggle the same way view 6 spells it. Bindings are the one inert kind — there is nothing to cycle, and
  the message points at `:bind`.

  The distance gutter now draws in the artist/album pane too. It numbers **every row** there, unlike a track list which
  numbers tracks and blanks its album dividers: an artist and an album are both jumpable, so there is nothing to skip.
  One `gutter()`, shared, because two copies would drift apart in width and an uneven gutter is unreadable.

  Worth recording as a testing lesson: the first version of the tree-gutter test asserted on the rendered _screen_, and
  passed with the feature disabled — the track pane beside it has a gutter of its own and satisfied the same substrings.
  The test that bites reads the tree pane's cell specifically.

- **2026-08-30** **`label` is a field, because a label is not an artist.** Bandcamp writes the imprint into
  `albumartist` — correct for a single-artist release, wrong for a sampler, and it does not special-case one — so a
  label turns up in the library looking like a band. §11.4 files by `albumartist`, so this is not a display bug to patch
  around; the tag is simply in the wrong place.

  The settled convention is that a label sampler is a **compilation**: `albumartist` is `Various Artists`, `artist` is
  the real per-track artist, and the label goes in `TPUB` (`ORGANIZATION` in Vorbis, sometimes `LABEL`). Retagging to
  that convention fixes the tree — but destroys the label unless something reads it, which is why the field comes with
  the fix rather than after it. One field in the shared vocabulary, so `{label}` renders, `label="*trip*"` filters and
  `sort = "label"` orders, without any of the three languages being told about it separately.

  A **stray single** off a sampler is the sub-case worth naming: `Various Artists` would give it a node of its own
  holding one track. Better to drop `albumartist` entirely and let §11.4's fallback use the track artist, which files it
  beside that artist's other music.

- **2026-08-30** **A `<leader>` key, `space` by default, and `<leader>?` for help.** The trigger was wanting a which-key
  popup without losing `?` to it — `?` is reverse search, as in vim, and a keymap whose navigation is nvim's should not
  take it back.

  `<leader>` is a _placeholder_, not a key: parsing cannot know what the leader is and must stay pure, so `parse_seq`
  produces a `LEADER` sentinel (`KeyCode::Null`, which no real keypress can produce) and the substitution happens where
  the configuration lives. Bindings added at runtime resolve as `execute` inserts them; the shipped defaults are built
  by `bind::defaults()`, which has no leader, so `Ui::set_leader` re-keys the table — once at startup, and again if the
  config names one.

  **A leader must not be bound on its own**, because §6's binding layer refuses to let a chord extend a bound key. That
  is why `expand` moved from `space` to `o`. The alternative — a timeout that decides whether `space` was a leader or a
  command — would put timing-dependent behaviour into a keymap, which is hard to test and worse to use.

  It exposed a latent ordering bug: `:`, `/` and `?` open their prompts _before_ bindings are consulted, so `?` fired
  even mid-chord. A part-typed chord now wins, which is what makes the second key of a leader sequence able to keep its
  own meaning when pressed alone.

  The help popup reuses the completion menu's slot and its column layout, because two popups in one place that disagreed
  about their shape would read as two different things. It lists the current context's bindings and `common`'s — _what
  can I do here_ — which is a different question from view 7's flat list of every binding in every context, and both are
  worth being able to ask.

- **2026-08-30** **A moved `data_dir` is reported, never followed.** Editing the setting changes where gmuse looks, not
  where the data is, so the next launch folded from nothing and reported an empty history while the ratings sat in the
  previous directory — #24's failure one step later, and its reasoning applies verbatim.

  It cannot be fixed by moving automatically, because three situations are indistinguishable from inside: the directory
  changed and the data is elsewhere; the directory is right and this is a first run; the directory is right and nothing
  has synced from the other machine yet. The third is real now that the directory is meant to be shared, and it is where
  helpfully copying from somewhere else would attach one library's ratings to another. So gmuse detects, declines, and
  names the remedy — the shape every other ambiguity here takes.

  The last directory used is remembered in `$XDG_STATE_HOME`, **not** in `data_dir`: it is a fact about this machine,
  and putting it in the synced directory would have two machines overwrite each other's answer.

  **The subtle part, found by running it rather than by reasoning.** The obvious test — "the new directory already has
  data, so it was moved by hand; say nothing" — is wrong, because gmuse writes an events log to the new location on the
  very launch that warns. The second launch would then see data in both places, conclude the move had happened, and go
  quiet for ever with the ratings still stranded: the same silent loss, reached by a different route. Only the _old_
  directory being empty settles it, so two directories both holding ratings is reported rather than excused — that is
  what a half-finished move looks like, and being told is the point.

  `--migrate-data` moves the base, every log and the playlists; `history.toml` alone would strand the recent plays.
  Nothing is ever overwritten, and the warning repeats until nothing is stranded.

  A **log** whose name is already taken is the likely clash rather than a rare one — it is the same machine's, and gmuse
  writes one at the new location on the launch that warns. Only the events the destination _lacks_ travel, into a file
  of its own; logs fold together by timestamp, which is what several in a directory are for.

  Taking the **difference** is what makes that safe, and it is why an earlier all-or-nothing rule was wrong: it declined
  any overlap at all, stranding the new half of a partly-overlapping log — the realistic case. A copy contributes
  nothing and is named; a partial overlap contributes its new events; a disjoint log contributes all of itself. No play
  is counted twice and none is dropped, and both counts are reported so neither is silent. Only logs: a second
  `history.toml` is a base, not a stream of facts, and two of them cannot be folded.

- **2026-08-30** **Registers, and the key reshuffle they force.** A playlist was in insert order with no way to change
  it. vim offers two idioms — unimpaired's `[e`/`]e` nudge, and its own `dd`…`p` — and the second is strictly more
  useful here, because **vim's registers are global**. Yank in one buffer, `:b other`, put: nothing about the register
  is attached to where it came from. So `yy` … open another playlist … `p` moves a track _between_ playlists, which no
  nudge key can do, and `p` putting **after** the cursor makes `dd`+`p` the swap idiom, so reordering needs no key of
  its own either.

  The analogy is tighter than "tabs" suggests. gmuse already has both vim concepts, separated correctly: **views 1–7 are
  tabs** — numbered, anonymous, `gt`/`gT`/`3gt` — and **playlists are buffers** — named, listed, switched by name.
  `:playlist open X` is `:b X`, `:playlist list` is `:ls`, and view 3's left pane is a buffer list. Playlists having
  names is not a departure from vim; it is what makes them buffers rather than tabs.

  `yy` makes `y` a prefix, and §6 refuses to let a chord extend a bound key — the same constraint that moved `expand`
  off `space`. So the three cmus add-keys were reshuffled, and the result reads better than what it replaced: `a` for
  add-to-playlist, which is the common one and the obvious mnemonic; `A` for add-to-library, which is rare and exists
  for view 5 reaching music the library does not have; `e` for the queue, unchanged.

  One register, unnamed. vim has named and numbered ones; whether they earn their surface when the "text" is tracks is a
  question best answered by missing them.

- **2026-08-30** **Undo is per list, and stores states rather than inverses.** vim's undo is per _buffer_, and a
  playlist is a buffer (see the registers entry), so `u` in one playlist must not reach into another you are no longer
  looking at. Keyed by name, so a list keeps its history across being closed and reopened.

  A whole snapshot, not an inverse operation. A playlist is tens or hundreds of `TrackId`s at four bytes each, so
  storing the _state_ costs nothing and cannot get the inverse wrong — which a hand-written "undo an insert by deleting
  at index n" eventually does. Bounded at 100 steps per list; vim's `undolevels` is 1000, and a playlist is edited at
  human speed.

  `remember` is called **once**, in `execute`, rather than inside each editing command. An undo stack that is right
  almost always is worse than none, because it is trusted until the once it is not — and one call site is what makes a
  new editing command undoable by default.

  Which list an edit belongs to is a property of the **command**, not of the view: `a` in the library adds to the open
  playlist, so recording by view filed that edit under nothing and left it un-undoable. Caught by a test that pressed
  `a` from view 2, which is how it is actually used.

  `u` was `rescan`, which in the TUI only printed a message saying to use `:rescan` — so freeing it cost nothing. It
  moved to `<leader>u`.

- **2026-08-30** **View 1 is one pane holding all three levels.** Tracks lived in a second pane showing whatever the
  tree had selected, which meant exactly one album's tracks at a time. Inline, several artists and several of _their_
  albums can be open at once, and the whole view reads as one buffer — `j`, `3j`, `gg`, `G`, `/` and the relative gutter
  all work over it uniformly, which is the point. Jumping between distant albums stops needing a pane.

  It **removed** machinery rather than adding it: `Pane` and `track_selected` leave `TreeState`, `tab` and the
  `in <pane>` wrapper belong to view 3 alone, and the `^j`/`^k` bindings — which existed only to drive the tree from the
  tracks pane — are gone.

  A track row renders through its own format template. It sits beside an artist row and has to look different, which is
  the job a file explorer gives icons; a template says it without depending on a patched font.

  `toggle` deliberately is **not** `close()` then `open()`. `close` walks up when the node is already shut — right for
  `h`, wrong for a toggle, where it would close an album's artist instead of opening the album. Caught by pressing Enter
  on an album and watching the wrong thing collapse.

- **2026-08-30** **`Enter` confirms; `x` plays.** Enter opens or closes a node, and on a **track** — where there is
  nothing left to open — it plays. That is snacks' rule, and with all three levels in one pane there is no second pane
  to descend into instead, which is what made "Enter never plays" the right answer for two panes and the wrong one for
  this.

  `x` still plays what is under the cursor from anywhere, so the two are not redundant: Enter acts on structure until
  structure runs out. It also fixed `x`, which meant "resume if something is playing, else play the selection" — two
  behaviours, the first of which is what `c` does. In the queue, `x` consumes the entry.

- **2026-08-30** **Transport moved behind `<leader>`, and `z` became vim's fold prefix.** The collision recorded in the
  entry below — `z` is Winamp's `prev`, and vim opens a fold with `zo` — was resolved by moving the _other_ side.
  `<leader>x` `<leader>c` `<leader>v` `<leader>b` `<leader>z`, plus volume and the 60-second seeks; the arrows keep bare
  seek because nothing else wants them.

  The reasoning is about posture, not about which key is scarcer. In these views your hands are navigating; reaching for
  the transport is stepping outside what you are doing, and the prefix says so. That frees the bare letters for what
  browsing actually wants — the whole `z` fold family, and `-` for #31's parent motion — and it costs one keystroke on
  an action you take between songs rather than within one.

  **The letters do not change**, only the prefix. That is what makes it one alphabet rather than two, and it is what
  lets #33 add a transport mode later in which the same letters are bare: `c` and `<leader>c` will be the same pause
  reached from the two postures, not two things to remember. Doing this half first is therefore not work done twice —
  `<leader>`-prefixed transport _is_ normal mode's end state.

  Consequences: `Z` (close-all) and `M-e` (expand-all) are gone, replaced by `zM` and `zR` — and `zr`/`zm`, which
  neither had. `expand`, `fold-close`, `expand-all` and `collapse-all` collapsed into one `fold <what>` command with
  nine forms, since they were nine forms of one idea and the config language should say so.

- **2026-08-30** **The tree keeps a fold _level_, not a set of open nodes.** `Expansion<K>` in `src/ui/fold.rs` holds a
  `level` plus explicit overrides either side of it, because vim's folds are two ideas at once. A bare `HashSet` of open
  paths cannot express `zr` — "open one more level" — without walking the whole tree to insert every node at that depth,
  which then loses the distinction between "open because the level says so" and "open because you opened it", and makes
  the next `zm` do the wrong thing.

  This is the operation the tree can offer that a file explorer cannot: its levels are **homogeneous**. In a filesystem,
  depth 3 means nothing in particular, so "collapse to level 2" is not a thought anyone has. Here every node at depth 0
  is an artist and every node at depth 1 is an album, so `zm` is "collapse to artists" and `foldlevel` is exactly the
  right word. Keyed on the ident path rather than a row index, so expansion survives a re-sort, a filter and a rescan.

  A level change discards the hand-made overrides, as vim's does: the point of a level command is to say what the whole
  tree looks like, and leaving exceptions behind would make `zM` not actually close everything. When a level change
  closes the row out from under the cursor, the cursor rides up to the deepest ancestor still visible.

- **2026-08-30** **`h` and `l` are the explorer motions, because vim's fold keys cannot be.** vim opens a fold with
  `zo`, and `z` here is Winamp's `prev` — the one place gmuse's two borrowed traditions actually collide. Checking
  snacks.nvim's explorer settled it: it does not use `z` at all. `l` opens, `h` closes, `Z` closes everything.

  **Folds only.** An earlier version fell through to moving between panes when there was nothing to open, which made `l`
  unpredictable mid-fold — open, or jump? — because the answer depended on state you could not see. `h` on something
  already closed closes its _parent_ and moves there, which is how repeated `h` walks up and out without anything
  needing a concept of "level".

  Worth recording: snacks independently uses `a` for add, `y` for yank and `p` for paste, which this project had arrived
  at hours earlier from vim alone. Converging on a keymap by two routes is decent evidence the reshuffle was following
  something real.

- **2026-08-30** **View 8 is guides — prose, not a key list.** `<leader>?` answers "what does this key do"; the guides
  answer "how do I do the thing", and the second cannot be found by pressing keys. `dd` then `p` swapping two tracks is
  not deducible from a table of bindings, and neither is the register being global.

  Two panes, view 1 and view 3's shape, because the list is how you choose and has to stay visible while you read. It
  passes #23's test by being navigated and scrolled with the ordinary motions rather than merely displayed.

  The text is `include_str!`d from `docs/guides/*.md`: real files, reviewable and diffable when the behaviour they
  describe changes, but compiled in so the binary needs no data directory and cannot ship without them.

  **Prose is rewrapped by paragraph; examples are untouched.** prettier reflows the source to 120 columns, which no pane
  is, so wrapping happens at render — but an example is a key table or a diagram whose alignment carries meaning.
  Wrapping each _source line_ on its own is also wrong: it leaves a short line wherever the file happened to break.

  **The scroll range is a deliberate over-estimate.** The cursor's extent must be known without a frame to measure it,
  and wrapping only adds lines, so it is computed at a width narrower than any pane and clamped where it is drawn.
  Under-estimating instead makes the end of a guide unreachable. A test at a _narrow, short_ pane pins it — a tall one
  hides the bug, because the clamp rescues it.

  **A test asserts the keys the guides teach are really bound.** Prose about a keymap goes stale silently; this is the
  cheapest thing that makes it fail loudly.

---

- **2026-08-30** **Every list remembers where it is scrolled to.** A fresh `ListState` starts at offset zero, and
  ratatui then scrolls the least it can to bring the selection into view — which lands the selection on the _last_
  visible row whenever it is past the first page. Going down that is invisible, because the view only starts moving once
  the cursor reaches the bottom and that part was always right. Coming back it is plainly wrong: after `G`, `k` dragged
  the whole view up a line at a time with the cursor stuck to the bottom line, instead of walking the cursor up the page
  it was already on.

  `Scroll(Cell<usize>)`, one per list, read into the `ListState` before the frame and written back after. A `Cell` for
  the same reason `guide_view` is one — only `draw` knows the pane's height. One per list rather than one shared,
  because switching views must not scroll the view you came from.

  The offset now outlives the rows it referred to — a fold closing, a filter narrowing, `dd` on the last entry — so a
  stale one has to clamp rather than blank the pane. ratatui does clamp; there is a test that says so rather than
  trusting it.

- **2026-08-30** **Text folds before it is compared, in two forms.** `src/text.rs`. Metadata carries marks a keyboard
  may not have, and this library's own filenames carry fullwidth stand-ins for the characters Windows forbids — so a
  track tagged `Air & Lack Thereof / Sparing the Horses` is `… ⧸ …` on disk. Neither is typeable by hand, and both were
  making things unfindable.

  **Two forms, because a mark can be typed around two ways.** `Löffler` is `loffler` to someone dropping the umlaut and
  `loeffler` to someone spelling it out, and German does the second. A search tries the flattened text and then, only
  when the text actually spells differently without its marks, the spelled-out form. Most characters have one sensible
  answer and the two agree, so the second pass is skipped outright — `expanded()` returns `None` for all-ASCII text and
  for `é`, `ñ`, `ç` alike.

  **Hand-rolled rather than `unicode-normalization`.** NFD would decompose the accents generically but still miss
  `ø æ ß ł đ œ`, which do not decompose at all, and every one of the punctuation substitutions — the half this library
  actually needs. Measured before choosing, per the project's own rule: the entire tag cache contains
  `ö ü í ´ ’ ＂ ⧸ ？` and nothing else, so Latin-1 plus Latin Extended-A is already far past what is there. No new
  dependency.

  **Smart-case governs capitals only.** Typing a capital asks for case to matter; there is no way to type a capital to
  ask for a diaeresis that is not on the keyboard, so marks always fold. The two are different axes and the rule says
  so.

- **2026-08-30** **Sort keys flatten their marks, which fixes an ordering bug nobody had noticed.** `ö` is U+00F6 and
  `z` is U+007A, so a raw comparison filed Christian Löffler after every artist beginning `lz` — off the end of the L
  section entirely. `sort_key` now folds through `text::needle`.

  The consequence worth recording: two artists whose names differ only by a mark now share a tree node, exactly as
  `MUSE`/`Muse`/`muse` already did under case folding. That is the desirable reading — it is what inconsistent tagging
  across an album looks like, and merging beats showing the same artist twice — but it is a merge, and it is silent.

- **2026-08-30** **`zr`/`zm` hold the cursor's screen row — which nvim does not do.** Measured rather than assumed,
  because the question was asked directly. In nvim, with the cursor on buffer line 60 and the window scrolled:

  |             | topline | cursor line | screen row |
  | ----------- | ------- | ----------- | ---------- |
  | before `zr` | 41      | 60          | 10         |
  | after `zr`  | 43      | 60          | **18**     |
  | after `zm`  | 42      | 60          | **9**      |

  nvim anchors `topline` to a **buffer line** and lets the cursor slide down the screen as the folds above it open. The
  cursor keeps its line, never its place.

  gmuse anchors the other end: the cursor's screen row is held and the tree opens behind it. That is a deliberate
  departure, and it is the right one here — in a file you are reading downward from the top of the window, so the top is
  what should stay put; in this tree the cursor _is_ the subject, and `zr` is asking "show me more around what I am
  looking at". Only the level forms are anchored: `zo` and friends change nothing above the cursor, and `zc` walking up
  to a parent should move the cursor up the screen, as it does in vim and as `h` always has here.

- **2026-08-30** **`~` filler past the end of every list, as vim draws it.** Without it a list stops scrolling once its
  last row reaches the bottom line, so the cursor's screen position depends on how many rows happen to be _below_ it —
  and holding the cursor still across a `zm` becomes impossible near the end however carefully the fold code tries.

  Checked against nvim rather than assumed: `^E` at the end of a buffer scrolls until `topline == lastline`, the last
  line alone at the top with `~` below. `Scroll::state_within` clamps to exactly that, which also stops a stale offset
  asking `pad_to_view` for ten thousand filler rows.

  The padding is a property of the screen rather than of the list, which is the whole reason vim draws `~` at all. New
  `Role::Filler`, set behind the gutter — the gutter is information and the filler is the absence of it.

- **2026-08-30** **which-key, derived from the binding table.** Pressing a prefix — `z`, `<leader>`, `g`, `d` — lists
  what can follow it in the bottom slot the completion menu and the `<leader>?` popup already share. Three occupants,
  one shape: they are all "a list of things you might type next, without covering the view it is helping you use".

  **Derived from `pending`, never stored**, for the same reason the pending-key indicator is: there is no second copy of
  the truth to keep in step, and a `:bind` shows up in the panel the moment it takes effect in the keymap.

  **No timeout.** which-key's `timeoutlen` exists because vim has keys that are both a command and the start of a longer
  one, so it must wait to find out which you meant. Here `resolve` fires the shorter binding outright — the README has
  always said a chord cannot extend a key bound on its own — so there is nothing to wait for. A fast `gg` flashes the
  panel for one frame, which is what which-key looks like with the timeout at zero.

  Two things the panel must get right, and both are about what it does _not_ say. Context shadowing is applied exactly
  as `lookup` applies it, or a view would be offered a key that does something else. And a key bound on its own is shown
  as what it runs rather than as a prefix, because anything extending it is unreachable — `+n more` is reserved for keys
  that really do only go deeper.

  Ordered case-insensitively with the unshifted key first, so `c` is followed by `C`. Two separate alphabets — every
  capital, then every minuscule — is how ASCII sorts and not how a keymap is read.

- **2026-08-31** **`zv` is not bound, because here it could never do anything.** vim's `zv` opens just enough folds to
  make the cursor's line visible. In this tree the cursor always sits on a row that `rows()` produced, so it is visible
  by construction and `zv` is a guaranteed no-op — binding it would be shipping a key that does nothing.

  What is useful is the compound: **`zx`** — vim's "reapply `foldlevel`, then `zv`" — because closing everything is
  exactly the moment the cursor's row stops being visible. It records the folds holding the cursor's row, sets the level
  to zero, reopens those, and puts the cursor back. On an artist that degenerates to `zM`, correctly.

  Which folds "hold" a row depends on its kind, and that is why `Expansion::reveal` was deleted rather than used: an
  ancestors-only walk cannot say that a _track_ needs its own album open. `TreeState::ancestors_of` answers per row —
  none for an artist, its artist for an album, both for a track.

  The question that prompted it — "can I close all open folds except the one my cursor is in?" — has a vim answer, and
  taking vim's spelling was cheaper and more memorable than inventing one.

- **2026-08-31** **`zz`/`zt`/`zb` read the cursor's row and the pane's height off the last frame.** Neither number is
  available outside `draw`: the height depends on the pane, and the row differs from the track index wherever album
  dividers occupy a screen row. Both live on `Scroll` beside the offset, recorded at render — the same device as
  `guide_view`, for the same reason.

  Worth recording because it was nearly missed: ratatui scrolls to keep the selection visible, so it **silently
  corrects** an off-by-one in `zb`. The mistake is invisible in a rendered pane and observable only in the offset that
  was asked for, which is where the test now looks. A rendering test would have passed either way.

  `zt` on the last row works because of the `~` filler; without it the list stops scrolling and the request is ignored.

  Placing acts on the focused list, and `focused_scroll` returns `None` for the two panes that are not scrolling lists —
  view 3's set of playlists and view 8's open guide. Reaching across to scroll the _other_ pane is the failure this
  prevents, and both cases have a test.

- **2026-08-31** **"Where is the thing I am listening to?" is `gp`, not `zv`.** A real question, and one neither the
  cursor nor a search asks. It goes under `g` — already the go-somewhere prefix — rather than on `z`, because it is not
  a fold operation and pretending otherwise would have broken the `zx = zM + zv` relationship above.

  In view 1 it opens the way to the playing track and nothing else, leaving the rest of the tree as you built it. In the
  flat lists it moves the cursor. Where the track is not in the view at all, it says so rather than moving somewhere
  arbitrary.

- **2026-08-31** **The guides are checked against the keymap in both directions.** There was already a test that keys
  the guides _teach_ are really bound. The other direction is the one that actually rots, because a key can be added or
  moved without anyone remembering the guides exist — and it had: the entire transport row had gone behind `<leader>`
  with nothing written about it anywhere, `moving-around` still said there were seven views, and the five playback-mode
  toggles had never been documented at all.

  Two things the audit had to get right before it was worth anything:

  **Only the key column of an example row counts.** Taking every token let the words of a description stand in for a
  key, so single-letter bindings passed on prose alone.

  **Letters keep their case; only named keys are folded.** Here case is meaning rather than typography — `r` and `R` are
  different bindings, as are `zc` and `zC` — and folding everything let a sentence mentioning `c` satisfy the bound `C`.
  `enter`/`Enter` and `g tab`/`g<Tab>` are folded by name, which is the only place capitalisation really is spelling.

  An `UNTAUGHT` list carries the deliberate exemptions with a reason each, and a second test checks those are still
  bound — an exemption for a key that no longer exists is a comment pretending to be a decision. A third feeds the audit
  a binding no guide mentions and requires it to be reported, because a test that can only fail on absence would pass
  just as well if it looked at nothing.

- **2026-08-31** **Structural motions, and one set of them for both shapes of list.** `{` `}` `[[` `]]` `-` `]z` `zj`
  `zk` — #31. `}` is the one that earns its place: with an artist expanded, `j` walks every track of every open album,
  and stepping artist-to-artist is the most frequent move there is. vim's paragraph motions are unused here and the
  semantics line up exactly.

  **The same eight ideas read in a flat list**, because there the album divider _is_ the structure: a "sibling" is the
  next album, a "section" the next artist, the "group" the run of tracks sharing an album. So view 2 gets structure to
  move through without a second set of keys, and `-` finally has the netrw meaning it should have had — free now that
  transport moved behind the leader (#33).

  Boundaries come from `list::album_starts`, derived from the same key `list::rows` uses to decide where to draw a
  divider. Recomputing them would have been three lines and a standing invitation for a motion to land somewhere the eye
  cannot see a boundary.

  All eight are one function over `rows()` — scan from the cursor for the first row matching a predicate — because what
  differs between them is the predicate and the direction. Eight separate walks would have been eight chances to get the
  edges wrong. They stop at the ends rather than wrapping, as vim's do, so a count that overshoots lands on the last
  match rather than nowhere.

  `[z` is bound to the same command as `-`: in a three-level tree, "the start of the current fold" and "the parent" are
  the same row. Binding both costs nothing and lets either muscle memory work, as `l` and `Enter` already do.

- **2026-08-31** **A fixture has to be able to tell the answers apart.** Six of the first mutation run's survivors were
  the same mistake: the tree fixture has one album per artist and two artists, so "the last track of the album" and "the
  last track of the artist" are the same row, and a count of two lands where a count of one does. The tests passed for
  reasons unrelated to the code being right.

  `deep_fixture` is three artists, one of them with two albums and one of those with three tracks — the smallest shape
  in which every one of these motions has a distinct answer. Similarly `two_albums_then_another_artist` for the flat
  list, because the shared fixture's only multi-album artist sorts _last_, leaving `]]` nothing to reach.

  The general lesson, and it is not the first time this project has paid for it: a passing test over a degenerate
  fixture is indistinguishable from a passing test over a correct one.

  **The mutation runner now refuses to start on a failing baseline.** A backgrounded run was killed between writing a
  mutation and restoring the file, which left `tree.rs` mutated — and every subsequent mutation then reported CAUGHT,
  because the suite was already red before anything was changed. A whole clean-looking run was worthless. Restoring in a
  `finally` narrows the window; checking the baseline first is what makes the _next_ run's results mean anything.

- **2026-08-31** **Three of #31's keys were vim-shaped but not vim, and are now canonical.** Trying the branch raised
  the suspicion that the motions were tied to artist/album/track. Mostly they are not — `step` scans by depth and names
  no level — but reading them against `:h` found three real divergences, and all three were _under_-taking vim rather
  than departing from it.

  **`zk` went to the previous fold's start; vim's goes to its end** (`:h zk`, against `:h zj`'s start). The pair is
  deliberately not symmetric: read together they walk the gaps _between_ groups, which is a move the old version could
  not make at all, and it left the end of a fold with no key. The test that covered it asserted `zj` then `zk` returns
  to where it started — a round trip vim does not have — and it passed because on the two-artist fixture "the previous
  fold's start" and "where I came from" are the same row. The same degenerate-fixture failure as the entry above, found
  the same way. `is_fold_end` also implements vim's "a closed fold is counted as one fold", so `zk` still steps row to
  row when the tree is collapsed.

  **`][` and `[]` did not exist.** vim's section motions are four keys — `]]` `[[` for starts, `][` `[]` for ends — and
  shipping the starts alone left no way to leave a section from the bottom, which is what you want when the next thing
  you press is `j`. `]z` is not a substitute: it stops at the enclosing _group_, so on a track it reaches the end of the
  album, never the end of the artist.

  **`zA` did not exist**, leaving `zO`/`zC` as the one fold pair in the keymap without its toggle, while `zo`/`zc` had
  `za`.

  **What was deliberately _not_ changed: `]]` stays absolute.** The suggestion was to make it address "level 1" relative
  to the cursor. Vim's `]]` is a fixed outermost level and nvim's modern answer — treesitter-textobjects, where `]f` is
  bound to `@function.outer` — is keyed on _what a node is_, not on how deep it sits. Neither is relative, and for the
  same reason: a motion whose meaning depends on where the cursor already is gives two answers you cannot tell apart
  without first checking your own depth. The generalisation this actually wants is a level addressed **by name**
  (`go next-level artist`), which is the treesitter model and fits "specs are data" — but it needs `ViewSpec` to exist
  first, and is recorded as an issue rather than guessed at now.

  One hardcode did go: `GroupEnd` asked `depth < 2` where it meant "is this row a container", which agrees only because
  the tree is exactly three levels. `flat_step`'s `artist_starts`/`album_starts` remain the real tie and are noted with
  the issue.

- **2026-08-31** **A vision document (`docs/design/vision-and-future.md`), and what it is not allowed to do.** The
  ambition has outgrown what §1 describes — librarianship with BPM, key and embeddings; tag editing; a two-deck
  instrument shaped like a tracker; one library across machines that cannot talk to each other. Written down so the next
  design decision can be judged against the whole rather than against the feature in front of it.

  It is explicitly **not** an authorisation. This brief stays the only place scope moves, and the vision document says
  so in its first paragraph. What it does instead is name the three boundaries now under pressure — tag editing (§1 says
  reading only), the three-thread architecture (INVARIANT, and two decks is a revision of it rather than an extension),
  and waveforms as a navigational aid against the no-visualizers rule — so that each is argued and recorded here when it
  moves, on the standard set by the 2026-08-29 amendment: a boundary may move to a principled place, and may not erode
  by increments.

  Its one load-bearing claim is that this has converged on **one expression vocabulary behind every surface** — filter,
  sort, format, catalog, smart playlist, `ViewSpec`, sync policy — so the design rule for a new capability is to try it
  as a field, a predicate, an event or a view spec before it becomes a subsystem. Ratings, history and catalogs are the
  evidence: each was small because it entered that way.

- **2026-08-31** **The which-key popup waits 300ms, and the reasoning it replaces was half right (issue #46).** The
  panel drew on the frame after the prefix, so `zz` flashed the whole fold vocabulary and took it away again. The
  comment on `which_key_rows` defended that, and the defence conflated two different waits:

  > which-key, without a timeout: a chord here is always deliberate — no key is both a command and a prefix … so there
  > is nothing to wait to find out.

  True, and it is an argument about **disambiguation** — vim's `timeoutlen`, which decides what the keys _mean_. This
  keymap genuinely does not need it. which-key's `delay` is a separate option that decides only **when to draw**, so
  that a chord you already know never gets a popup at all. The old comment even predicted the symptom ("a fast `gg`
  flashes the panel for one frame") and judged it acceptable; use said otherwise.

  Three details taken from `which-key/view.lua:99`, and they are what make it feel right rather than merely late. **The
  delay is charged once per chord, not once per key** — an already-open popup redraws with no delay — which is held here
  by keeping the _first_ key's instant in `pending_since` and never resetting it. **Nothing waits that was asked for by
  name**: `<leader>?` sets `ui.help` and takes a different path, mirroring which-key's `ctx.plugin and 0`. And **the
  poll shortens to what is left of the delay** — at a flat 200ms tick the popup would appear anywhere up to 500ms
  depending on where in the cycle the key landed, which is worse than being late, because it is inconsistent between
  presses. `tick()` returns the ordinary 200ms again once the popup is due, or the loop would spin on a zero timeout for
  as long as it stayed up.

- **2026-08-31** **`SettingValue::Millis(u16)` — the first numeric setting, chosen over a named enum.**
  `off | short | long` would have fitted the existing completion model exactly, and it is a number wearing a disguise;
  the next timing setting would have reopened the question.

  **The completion question was settled by measuring vim, not by reasoning about it.** `:` completion is prefix-based
  over a closed vocabulary (2026-08-28) and a number has none, so the first attempt offered `0`, `200`, `300`, `500` as
  _suggestions_ — which would have meant every other perfectly valid number reading as "no matches". Running
  `getcompletion("set scrolloff=", "cmdline")` in nvim gives `['7']`: **the current value, alone**, where
  `set foldmethod=` gives the whole enum. So vim does complete numerics, and what it offers is the one thing it knows
  rather than a list of plausible ones.

  Taken as-is. `set which_key_delay=` completes to what it is set to now, which answers "what is this at?" — the
  question you actually have before editing it — and offers nothing when what you have typed is not a prefix of it,
  because there is nothing to offer. The current value reaches `setting_values` through `World`, alongside the catalogs
  and bindings already there.

  `:toggle which_key_delay` is refused by name, like `resample_quality` before it, rather than cycling through guesses.

- **2026-08-31** **The settings view is deleted, and the keymap listing becomes a generated guide (issue #47).**
  Settings are a file and a `:` command; a pane that renders them is a UI for something vim has never needed one for.
  The rule, stated so it can be applied again: when a thing could be a view or a command, prefer the command unless
  there is a reason beyond "it was cheap to add". §6 justified view 7 as costing nothing once bindings and settings were
  enumerable — true, and _cheap to build_ is not _worth having on screen_.

  Two of its three jobs needed no replacement. Catalogs were **already duplicated** by view 6, which is the scope view
  and lists them as its whole purpose. Editing a setting is `:set` and `:toggle`, and the playback modes it displayed
  are in the status line already.

  The third did. View 7 was the only complete listing of the keymap, and both the which-key overflow notice and
  `<leader>?` leaned on it — `<leader>?` answers "what can follow this prefix", never "what is bound". So the listing
  survives as **a guide that is generated rather than written**: `Body::Keymap` renders `bind::sorted` into the same
  shape the written guides use, so it goes through the same wrapping and styling, and a `:bind` typed a moment ago is in
  it. vim keeps the same artifact at `:help index`, which is where the title comes from.

  Views renumber 8 → 7, ending at seven as cmus does.

- **2026-08-31** **A generated guide would have made the guide audit pass vacuously, and that is the interesting part.**
  `keys_the_guides_write` scans guide bodies for indented key columns, and `every_binding_is_taught_by_some_guide`
  reports any binding no guide mentions. An index generated _from the binding table_ contains every key by construction
  — so counting it would have made that test pass for every binding that has ever existed, and the prose guides would
  have silently stopped being checked.

  Nothing would have failed. The test would have stayed green while checking nothing, which is the same failure the
  degenerate-fixture entries above record, arriving by a new route: not a fixture too small to tell answers apart, but
  an oracle that has been handed the answer key.

  `keys_the_guides_write` therefore reads `Body::Text` only, and `the_generated_index_does_not_count_as_teaching` binds
  a key nothing writes about and asserts it is _still_ reported — because a guard that is only exercised when it works
  is not a guard.

- **2026-08-31** **`^d`/`^u` (issue #44), and `^f`/`^b` measuring the pane they are in (#45).** `^d`/`^u` were never
  bound — the report was that they did not work in the guides, and they did not work anywhere. `^f`/`^b` were bound but
  moved a hardcoded ten rows, which is a third of a page on a tall terminal and an overshoot in a split.

  Both now measure. `page_height` is the pane less **two rows of overlap**, which is vim's (`:h CTRL-F`) and is not
  incidental — the overlap is what makes the jump readable. `half_page_height` takes no overlap, because half a page
  already leaves half the screen in common.

  **One number serves every pane, and the reason is worth writing down: every split in this UI is side by side.** So a
  pane is exactly as tall as the body area, and `Ui::body_height` — set once in `draw` — is right for all of them,
  including the three that render as a `Paragraph` and have no `Scroll` at all: the playlist list, the scope view, and
  the guide body. Reaching for `Scroll::height` instead would have left the guide body out, which is precisely where the
  report came from.

- **2026-08-31** **The `zz` that `^d` owes is deferred to the next frame, and applying it at once was wrong.**
  `Scroll::place` reads `row`, which is the cursor's **screen row as of the last frame**. `zz` is correct with that
  because it does not move the cursor. `^d` does, so recentring in the same breath would centre where the cursor _was_ —
  and in a track list the error grows with position, because rows and track indices diverge by however many album
  dividers sit above the cursor.

  So `centre_next` is a flag, honoured inside `state_within` after the row for this frame is known. The test asserts the
  cursor ends up within one row of the middle after six `^d`s rather than asserting on an offset: the offset is an
  implementation detail and the centring is the property.

  It also has to bypass `execute`, or one keystroke would record two undo steps and two entries in the repeat state.

- **2026-08-31** **The help popups scroll (issue #42), and the key exception is exactly as wide as the affordance.**
  `<leader>?` lists every key in the view — 41 rows against a cap of seven — and the rest was unreachable: the notice
  said to go and read the Index guide instead. `^d`/`^u` now move half of what the popup is showing, clamped, which is
  which-key's own scroll (`win.lua:104`) and matches what those keys do everywhere else here.

  It needed an exception to a rule chosen on purpose — any key dismisses the popup and is swallowed doing it — so the
  exception is drawn to be self-evident rather than remembered: **the popup takes `^d` only when `^d` would do something
  in it.** `popup_is_scrollable` asks whether anything is hidden, which is the same condition that draws the
  `^d ^u scroll` footer, so there is never a press that looks like it should scroll and does not, nor a key silently
  eaten by a panel that had nothing to show. `draw` clears `help_view` when no popup is drawn, or a popup still waiting
  out its delay would inherit the last one's scrollability.

  The footer follows which-key's rule (`view.lua:448`): shown **only** when the content is taller than the window. Not
  taken from it are `<esc> close` and `<bs> back` — here _any_ key closes, so naming one would be a lie, and there is no
  going back to name.

- **2026-08-31** **`+N more` cannot happen in the shipped keymap, so naming prefix groups is not built.** The other half
  of #42 was that a prefix reads as `+3 more` where which-key names it — which the local nvim config spends most of its
  lines doing.

  Measured before building: **every default binding is at most two keys**, so no continuation is itself a prefix and
  `Next::More` is never constructed. `z` offers 16 continuations and 0 groups; `g` 5 and 0; `<leader>` 12 and 0.

  It is reachable — a user binding `z g g` would see it — but naming groups needs a `group` verb, config plumbing, a
  name table and completion, for a case the defaults never reach and for which there would be no default names to write.
  Left in the issue with the measurement recorded, so the decision is on file rather than rediscovered.

- **2026-08-31** **Transport mode (issue #33), and the one rule that makes it worth having: a mode does not inherit.**
  `i` enters, `escape` leaves, and `Context::Transport` is looked up _instead of_ the view's context rather than over
  it. `Context::inherits_common()` is false for exactly one context, and that is the whole mechanism.

  It is what vim does — insert mode does not fall through to normal mode — and it is not a detail. A mode that inherited
  would leave `j` navigating in the mode where `j` is volume, and then the mode has bought nothing: the keys it wanted
  were already taken. Everything else follows from it: the way out has to be bound inside the mode or it is a trap, `?`
  has to be bound there or there is no way to ask, and `<leader>?` lists only the mode because listing `common` would be
  listing keys that do nothing.

  **The letters do not change, only the prefix.** `c` and `<leader>c` are one pause reached from two postures, which is
  what the 2026-08-30 half of this promised when the transport row moved behind the leader.

  `:` works from both and `/` and `?` do not. Searching is navigation and there is no list you are looking through in
  transport mode; `:` is the way to everything and a mode should not be able to trap you away from it.

- **2026-08-31** **The mode indicator is drawn before the notice, not after it.** #33 called a mode you cannot see
  non-negotiable. The status line's first tenant is `ui.status`, which holds a message for four seconds — so putting the
  indicator behind it would hide the mode for four seconds after every message, which is precisely when you are least
  sure what the keyboard is about to do. It is unconditional and first, and normal mode draws nothing at all, as vim
  draws nothing for it.

- **2026-08-31** **The mode is not remembered across a restart.** The session remembers the view, and #33 asked whether
  a mode is the same kind of fact. It is not: vim never resumes in insert mode, and starting in a keymap you did not
  choose — with the indicator as the only warning, on a screen you have not looked at yet — is the "mode you press the
  wrong key in" hazard in its purest form. Navigation is the safe default to land in because it does not touch the
  audio.

- **2026-08-31** **`50%` is vim's, and it reads the count the way `12G` does.** A count with `%` seeks that far into the
  track, exactly as `50%` in vim goes that far into a file; bare it means halfway. It is the clearest argument for the
  mode existing at all — there was no room for it in a keymap where the digits are counts for navigation and `%` had
  nowhere to sit.

  `seek-percent` shares a completion prefix with `seek`, so `see<Tab>` now stops on `seek` without the trailing space
  that says "finished". That is the same shape as `search`/`search-next` and `add`/`add-playlist`, both of which predate
  it, so it is consistent rather than a new wart.

- **2026-08-31** **The transport panel (issue #34), and the §1 argument it has to win.** `-- TRANSPORT --` on the status
  line was easy to miss, so the mode now raises a panel in the bottom slot: what is playing through its own format
  template, a position bar, the times and volume, and **what is next** — which earns its line precisely because shuffle
  makes it unguessable, and which `next()` can answer purely.

  §1 refuses "something to watch" and has refused a permanent now-playing panel twice. **What makes this different is
  that it is scoped to the mode.** While you are browsing — which is the whole of what the program is for — it is not
  there and costs nothing. It appears only once you have said, by entering the mode, that playback is what you are
  attending to. A panel you cannot dismiss is clutter; a panel that _is_ the thing you switched to is the screen doing
  what you asked. Judged by the vision document's §7 standard, the boundary moved to a principled place rather than by
  an increment.

- **2026-08-31** **The panel is the slot's lowest-priority tenant, and the only one that yields to a short terminal.**
  `:` and `<leader>?` displace it, because those are things you just asked for and it is a standing display; it comes
  back when they close.

  The height guard is the part that is not obvious. The other three tenants are gone in a keystroke, so the slot's
  ten-row cap is enough for them. This one lasts as long as the mode does, so on a terminal too short to spare six rows
  it would leave the library pane as nothing but borders — the "swallows the view it is helping you use" failure, held
  open indefinitely. Below `2 + 6 + 4` rows the panel simply does not rise, and nothing is lost that must be seen
  because the status line still carries the indicator.

- **2026-08-31** **`[format] transport` is its own key, not a reuse of `now_playing`.** The panel has a whole line where
  the status line has whatever is left after the state glyph, the times and the mode flags — having room is the reason
  the panel exists, so sharing a template would have thrown that away. The default is richer accordingly, with album and
  year in `[…]` groups so a track carrying nothing but a filename still reads as one line.

  The glyphs are the status line's three, unchanged: `▶` is single-width everywhere, while `⏸` and `⏹` sit in a range
  many terminals draw double-width or not at all — so `■` stands in for stop, and a mis-measured glyph cannot shift the
  row. #34 flagged this and the answer was already in the status line.

- **2026-08-31** **The transport panel floats over the centre, because in the bottom slot it did not read as a mode.**
  #34 put it in the slot beside the completion menu and `<leader>?`, on the reasoning that a fourth tenant was a `match`
  arm rather than a new concept. That was true of the code and wrong about the screen: **the slot is where transient
  helpers live**, so the panel read as one — and the reported symptom was pressing navigation keys while in transport
  mode, over and over.

  A mode indicator that can be mistaken for furniture is not doing the job §33 called non-negotiable. It now draws last,
  over the middle of the body, in a double border in `Role::Mode` — the same role the status line's `-- TRANSPORT --`
  uses — and titled the same, so the two are visibly one fact rather than two decorations. `Clear` blanks what is
  underneath.

  **It covers the list on purpose.** In transport mode the list is not what your hands are doing, and that is the whole
  claim: a panel that reports the mode from the margin is furniture; a panel over the middle _is_ the mode.

  Two things fall out. It no longer competes for the slot, so **it stays up while `:` is open** — opening a prompt does
  not leave the mode, and the old version made the indicator vanish at exactly the moment you were about to type. And
  the short-terminal rule inverts: rather than yielding rows it will not fit, it declines to draw when it could not
  float with a margin around it, because a panel filling the pane is a replacement rather than an overlay. The status
  line carries the indicator either way, which is what makes both refusals safe.

- **2026-08-31** **`/` searches as you type and paints what it matched (issue #55), and `/` was silently
  case-insensitive while the guide said otherwise.** Three things, and the third was found by reading:

  **incsearch.** The search ran only on `Enter`; it now runs on every keystroke, from the position the prompt opened at
  rather than from the cursor. Searching from the **origin** is what makes it a preview rather than a chain: `bur` then
  `i` refines one search, so deleting a character walks back exactly where adding it went. `Esc` restores the cursor and
  `Enter` commits — without that an abandoned search is an unintentional jump, which is the detail most
  reimplementations of this miss.

  **Highlighting**, and the offset problem underneath it. `text::flatten` folds marks before comparing, and folding does
  not preserve length: `ß` folds to `ss`, `＂` to `"`. So an offset into the folded string means nothing in the
  original, and `match_range` walks both at once to report a char range in the **source**. It runs per _visible_ row,
  not per stored row — a screenful is thirty rows where the search walks thousands.

  **Smart-case**, which `docs/guides/finding-things.md` already promised — _"Typing a capital still asks for case to
  matter"_ — and which `text::needle` did not do: it lower-cased unconditionally. §7 committed the filter language to
  smart-case and `/` quietly disagreed. `Needle` now carries the decision, made once at the boundary, and both surfaces
  answer the same way. One existing test asserted `"LOFFLER"` matched `Löffler`, which smart-case correctly makes
  precise; updated, because the old assertion was the old behaviour rather than a property.

- **2026-08-31** **nvim's `CurSearch` is not copied, and the reason is that its cursor is a thin bar and ours is not.**
  #55 proposed two roles, as nvim has: `Search` for every match and `CurSearch` for the one the cursor is on, inverted
  relative to it. Built both, then looked at the rendered buffer: the cursor row is painted **end to end** by
  `SelectionFocused`, and ratatui's `highlight_style` overrides any per-span style within the selected item — so
  `SearchCurrent` could not be seen even where it was set.

  It is also unnecessary. nvim needs the distinction because a thin cursor does not tell you which match you are on; a
  fully highlighted row already does. One role, and the second removed rather than left in as decoration.

- **2026-08-31** **The theme-coverage test passed for a role the theme never named.**
  `the_shipped_theme_covers_every_role` asserted `text.contains(role.name())` — a plain substring over the whole file.
  `Role::Mode` was added with the transport work and never written into `examples/catppuccin-frappe.toml`, and the test
  stayed green because the file contains `status_modes`, which contains `mode`.

  Now checked as an **assignment**: split each line on `=` and compare the key. A substring is how a vacuous test gets
  written by accident, and this one had been silently passing for a role that fell back to its default.

- **2026-08-31** **View 4 shows what would play next, and it cost nothing because shuffle is a permutation (#56).** The
  question was whether the transient queue is the right shape, or whether the future should be something you can see and
  manipulate. The answer turned on a decision made on 2026-08-27 and never cashed in.

  Shuffle is a **seeded permutation**, so the play order is a pure function of the pool and the seed — `step` already
  materialises the whole order on every call and keeps one element of it. `upcoming` reads the rest. **No new state, no
  simulation, no bookkeeping**, and §5 is untouched: nothing about what `next()` decides has changed, only what is shown
  of it. A played-set could not have answered this question at all, which is the payoff the original entry recorded a
  cost for.

  The projection is checked _against_ `next()` rather than beside it: the tests assert its head is what would play, and
  that walking `next()` by hand traces exactly the projected path.

  **A queued track appears twice**, once from the queue and again where the pool reaches it. That is correct — §5 says
  playing a queued track does not move the library cursor — and it is shown rather than hidden, because hiding it would
  display a future that does not happen.

  **The limit is the contract, not a cap.** With `repeat` the true answer is infinite, and one row past the wrap is all
  that can honestly be shown: `Source::Wrapped` advances the seed, so the second lap is a different permutation from the
  one on screen.

- **2026-08-31** **Only the queued rows are editable, and the divider says where they stop.** The cursor ranges over the
  whole projection, but `dd` on a projected row is refused by name rather than silently eating a row the queue never
  owned.

  The reason is not caution, it is that there is nowhere for the edit to land: there is no seed that yields "this
  permutation but with track 7 moved to position 3". The queue is a real list and a playlist is a real list; the pool's
  order is a function. So the natural shape for editing later is that moving a row _is_ a queue insertion — the concept
  already there, finally visible — and removal from the tail has no home short of #28's exclusion set.

- **2026-09-01** **Tag editing is in scope; §1's "reading only" is withdrawn.** The exclusion was written while the open
  question was whether a terminal player was worth building; it is not, and a scope rule that protected a proof of
  concept does not automatically bind the project it became. The focus was real and it was paid for — the other §1
  exclusions stand, and the 2026-08-29 standard governs the move: a boundary may move to a principled place, and may not
  erode by increments.

  Three guardrails come with it, in §1: scanning stays read-only and editing is an explicit act on a selection; bulk
  retagging lives in `tools/` on the genre pipeline's propose/review/apply pattern; and an edit must be undoable.

  Deliberately **not** decided here: whether the UI grows a tag editor, and whether edits are `kind = tagged` events or
  snapshots. That is #41. Admitting the capability to scope and choosing its shape are separate decisions, and the case
  that prompted this one — multi-artist credits, where the tags already settle 332 of 367 tracks and three strings need
  a person — argues for a `tools/` pass rather than an editor.

  **Decided 2026-09-02: there is no tag editor, and the capability was admitted under the wrong name.** What the UI
  edits is _library metadata_ — a credit, a genre, a rating, a pin — which lives in the library whether or not a file
  exists to carry it. Writing tags is a separate flow in the other direction: an **export**, invoked over a selection
  when you want other programs to see what you know.

  The distinction is not pedantic. "Edit the tag and let the scan pick it up" makes the file the authority and the
  library a cache of it; "edit the library and serialize when you want to" makes the library the thing. Only the second
  survives a second machine, and the first is what stranded a four-star rating. See `library-api.md` §6.

- **2026-09-02** **`serde_json`, for the proposal format and nothing else.** A tool says what it found by writing one
  JSON object per line, which `library-api.md` §5 settles: facts are heterogeneous, and a TSV covering every kind anyone
  might assert would need a column union — a shape this project has already been bitten by once, when a genre containing
  the separator fought its own quoting.

  Measured cost: two crates in the lock, `serde_json` and `zmij`. It is used at the boundary only; the stored form stays
  postcard, which encodes a variant by index rather than by name — so `Source` and `Confidence` are lowercased on the
  wire, because a proposal file is something a person types, and it costs the stored form nothing.

- **2026-09-01** **One player at a time, held by the kernel (`std::fs::File::try_lock`).** Dogfooding put gmuse in a
  long-lived tmux session, where a second instance started by hand is easy and silent. What it actually breaks was
  measured rather than assumed, and is narrower than expected: **ratings and plays are safe**, because an event is one
  short `write_all` to a file opened `O_APPEND` and two writers interleave whole lines without tearing one — the
  append-only design that exists for syncing two _machines_ turns out to make two _processes_ safe too. The library
  cache is a whole-file write of something regenerable. What is not safe is `session.toml`, written whole on quit so the
  last quit decides what the next launch restores, and the audio device, which both instances open and both play.

  So the lock is for the position file and the speakers, and is deliberately not sold as protecting the history.

  **An OS lock, not a pid file.** The kernel releases it when the process ends _however_ it ends — `kill -9`, a panic, a
  terminal closing under a tmux popup. A pid file needs a liveness check, a staleness rule and cleanup on every exit
  path, and is still racy between the check and the write. Tested: after `kill -9` the next launch starts, and a
  leftover file containing a dead pid does not block. A pid **is** written, as the file's contents, so the refusal can
  name the other process — never to decide whether the lock is held.

  **No dependency and no `unsafe`**, which is why this is worth doing now rather than earlier: `File::try_lock` was
  stabilised in Rust 1.89 and the toolchain here is 1.98. Before that it meant `libc::flock` behind `unsafe` — barred
  outside `src/audio/` — or a crate.

  It lives in `$XDG_STATE_HOME`, not `data_dir`: `data_dir` is the shared and possibly synced directory, a lock there
  would claim to speak for machines it cannot see, and advisory locking over a network filesystem is not dependable.

  `--no-lock` opts out, because wanting a second instance is legitimate — auditioning a file with `--repl` while the
  player runs is the obvious case — and the refusal names the flag.

- **2026-09-01** **`--repl` and the bare file argument are retired (issue #61).** This is a **TUI-first** program, and
  the CLI is a way to do things _for_ the TUI — not a second way to be a player. `--repl` was the version that forgot
  that: it was not an interface _to_ gmuse but a second gmuse, with its own audio device, library and session.

  Its stated reason had expired. The 2026-08-27 entry above keeps it "because M4 is read-only by design, so making the
  TUI the only interface would have removed the ability to play anything until M5." M5 shipped; M1–M8 are done. The
  workaround outlived the thing it worked around, and the day's lock made it unusable — `just play` was `--repl`, so the
  repl's last use case was precisely what the lock exists to prevent.

  **The bare file went with it, and that is the sharper half.** `playback.current` is `Option<TrackId>`; `TrackSource`
  is `{path, gain}` with no id. So `gmuse <file>` produced audio while `current` stayed `None`, and everything
  downstream keys off `current`: `next()` could not run, nothing was recorded in history, nothing was displayed, and
  §5's pure `next()` did not apply to it at all. Not a feature with limits — a **second, shadow notion of "a thing that
  can play"** beside the real one. Library-first means everything that plays is a track in the library.

  Removing it removes the only call to `player.load` with something the store does not know, so `TrackSource` is now
  always built from a `Track` by `source_for` and the untracked-playing state stops existing rather than being handled.
  `TrackSource::plain` survives `#[cfg(test)]`, which is not a leftover: the audio layer sits _below_ the store and is
  deliberately ignorant of `TrackId`, so testing it should not require a library.

  Deleted: `repl.rs`, and `run`, `print_library`, `print_matches`, `print_status`, `drain_events` and `fmt_time` from
  `main.rs`. `PlayerHandle::gain` went with them, having no other caller; `Store::capacity` and `Store::tombstones` are
  now `#[cfg(test)]`, `print_library` having been their only use outside tests. 683 tests pass, no warnings.

  **Deliberately not replaced.** A `--play-and-exit` was considered and refused as the same incoherence in a smaller
  wrapper. Auditioning an un-imported file is `afplay`'s job; gmuse's answer is to import it, after which it is a real
  track with a real history. The one genuine loss is a _scriptable_ rescan — `just rescan` was headless through the repl
  and now opens the TUI — which is M9's socket to restore, not a flag to add. That also retires the example given in
  this morning's lock entry: `--no-lock` still exists, but not for auditioning a file with `--repl`.

- **2026-09-01** **M9: a unix socket, and a fourth thread to serve it.** §1 asks for a `cmus-remote` equivalent "for
  media keys and status bars", and that phrase is the design — the socket exists for the two things a TUI cannot do: be
  reached from a key binding not focused on it, and answer a question asked by someone else's status line.

  **One parser.** A request is parsed by `cmd::parse::command`, the same function the `:` line uses, so there is no
  second command language. That was the standing complaint against `--repl` (#61), which had `load`, `status`, `library`
  and `find` that the `:` line did not. Commands are applied through `ui::execute` — the same door every key binding
  uses — so a remote `pause` is not a second implementation of pausing.

  **Queries and commands are served differently, deliberately.** A command is posted to the UI thread and applied on its
  next tick. A query is answered by the listener from a snapshot the UI _publishes_ each tick, so a status bar polling
  once a second never waits on a render and can never hold one up.

  **`play` does not travel, and that was the surprise.** It reads like the most obvious transport verb there is, but
  here it means _play what the cursor is on_ — `execute` starts `ui.selected_track(ctx)`. Down a socket it would start
  whatever row the player happens to be sitting on, which the caller cannot know. `next` is the cursor-free way to
  begin, because `advance` resolves from the **pool** through `next()` and never consults the screen. The refusal says
  so by name rather than returning a generic "needs a cursor", which would read as a bug rather than a fact.

  **The stale socket problem does not exist**, and that is a dividend of the morning's lock. `UnixListener::bind` fails
  if the path is there, which normally means guessing whether a leftover socket belongs to a live process. It cannot
  here: the lock is taken before the bind, so holding it proves no other gmuse is running and any socket present is
  stale by construction. Verified end to end — a socket left by a killed player is rebound over on the next launch with
  no warning.

  The client is `gmuse --remote "<command>"`: one binary, so the existing symlink keeps working, and no lock, because a
  client is not a player. A refusal from the player exits non-zero, or a status bar would show a stale line and call it
  success.

- **2026-09-01** **`status` takes a format, because a status bar cannot use a rendered line.** M9 shipped `status`
  returning the track through `now_playing`, which is right for a person typing it and wrong for the consumer §1 named.
  This library's template is `{artist}[ — {album}] — {title}` — 41 columns for a real track, against maybe 30 free in
  the bar — and **the title is last**, so truncating to fit throws away the only field you were looking for. Splitting
  on the separator instead would depend on a template the caller does not control.

  So `status <format>` renders the current track through a caller-supplied string, and it is **the same format language
  as the config and the `:` line** — `status {title} · {artist}` needs nothing learned and adds no second display
  vocabulary. The vision document's claim is one expression vocabulary behind every surface; this is a surface that
  would otherwise have needed its own.

  The snapshot carries a cloned `Track` for it, taken **when the current track changes** rather than every tick — the
  render loop runs five times a second and the clone is a handful of `String`s. A bad field errors once rather than
  printing blank forever, which is the failure a status bar would never notice.

- **2026-09-01** **Repairing a stranded history means backdating the `moved` event, and that is not a detail.** A
  renumber of `YouTube Sessions` renamed three files, and because history is keyed by path it stranded three entries —
  one of them a **4-star rating** on a file that still existed. The event log already had the machinery: `Kind::Moved`
  is folded by `History::rename`, with a test pinning that plays, rating and `added` all carry across. So the repair is
  three appended lines, not a patch to `history.toml`.

  Two behaviours decide whether those lines do anything, and both are silent when they do not:

  1. **`fold_into` sorts events by timestamp** before applying them, so a `moved` event does not act "last" — it acts at
     the moment it claims to have happened.
  2. **`History::rename` refuses when the destination already has an entry**, returning `false` and changing nothing.
     Conservative and right, since merging two histories is a decision the fold should not make on its own.

  Together they mean a repair stamped _now_ is applied **after** the `seen` that the renamed file has already
  accumulated, finds the destination occupied, and is refused — leaving a repair that looks done and did nothing. The
  event has to be stamped inside the window when the rename actually happened: after the old path's last event, before
  the new path's first. Here that was between `1788227551` and `1788233784`, and `1788233783` worked. Verified by
  replaying the fold: 3 renamed, 0 refused, 0 dead paths, rating recovered.

  `History::save` being dead code is what makes this safe to do with the player running: `history.toml` is an immutable
  base and everything since is in the log, so nothing can overwrite the repair between appending it and folding it.

  Recorded because it is also evidence for #35. The fix required knowing the rename window, the fold's sort order and a
  refusal rule — none of which would exist if identity came from the audio rather than the path.

- **2026-09-01** **Events carry a device and a sequence, and the id is `device:seq` rather than a ULID (issue #36).**
  Two things were broken. Dedup on import was whole-line string equality, so **two plays of the same track in the same
  second on two machines are the same line and one was silently dropped** — rare, invisible and permanent. And the fold
  ordered every machine's events together by wall clock, which §5 of the sync design explicitly forbids: a device
  offline for six months with a drifting RTC produces timestamps that interleave wrongly with everything else.

  **`device:seq`, not a ULID.** The sync design reaches for a ULID; the pair already has every property the merge needs
  — unique without coordination, monotonic per device, and a watermark that is an integer comparison — and it has one a
  ULID does not: **it cannot be sorted across devices**. A ULID embeds a timestamp, so it silently invites the exact
  mistake §5 forbids. This makes that mistake unexpressible, and costs no dependency.

  **No migration, and two formats in one file.** They are told apart by the first column: a legacy line opens with a
  timestamp, which parses as an integer; a line written since ids exist opens with a device id, which does not. An
  append-only log should not be rewritten to change its schema, and the old lines still say everything they said. A
  legacy line's id is a hash of its own content — which is exactly the dedup those lines have always had, so nothing is
  retrofitted with an identity it never had and the imprecision ages out.

  **The counter is read back from the log**, not kept in a state file beside it. Two copies of a monotonic counter is
  one more than can be kept honest across a crash, and the log is the thing that must be right.

  Verified against the real 6492-line log: folds to 6564 events, 5774 tracks, both formats together.

  Deliberately **not** done here: the fold still orders by wall clock. Ordering across devices is a separate question
  and a larger one — this issue is the schema, and the schema is what makes the answer expressible.

- **2026-09-01** **An audio identity, hashed from the encoded packets (#35), and `twox-hash` as the one new
  dependency.** §2's budget is deliberately small, so: `twox-hash` with
  `--no-default-features --features xxhash3_128,std` pulls **nothing** transitively. Its default features want `rand`,
  purely to seed a hasher randomly — the exact thing a content id must never do — so turning them off is both cheaper
  and more correct. `std`'s `DefaultHasher` was the free option and is disqualified: it is explicitly documented as
  unstable across Rust releases, which is fatal for anything durable. Nothing here is adversarial, so a cryptographic
  hash would be paying for a property not needed; 128 bits puts a collision far past any library that will exist.

  **Packets, not decoded frames.** The sync design's §2 proposes hashing decoded audio. Measured: decoding costs about
  six times as much and buys nothing, because everything a tagger touches is outside the packets by construction.
  Verified stable across a title change, a 4 KB frame, an added and a stripped ID3v1, and an ID3v2.4 -> v2.3 downgrade,
  on all five container formats here — including Ogg, Opus and FLAC, where tags live _inside_ the stream. Neither hash
  unifies formats: the same recording as FLAC and as MP3 decodes to different samples, so nothing is given up.

  **The scheme byte is the point.** Symphonia's packet framing is not part of its public contract, so a version bump
  could reframe a format and silently invalidate every event ever recorded — the failure #35 exists to prevent. The id
  carries the scheme that produced it and a test pins a known file's hash to a literal, so a reframing fails loudly
  instead.

  **Cost, on 5774 files:** cold scan **16.5s**, warm rescan **0.02s**. The hash is computed only where the file is
  already being opened, so the incremental path is untouched and the whole-library cost is paid once. An existing cache
  loads with `audio_id: None` and fills in as files are next read, rather than forcing a rehash on upgrade.

  **`detect_moves` now keys on the audio when it is known**, and on the old size/duration/tags shape only when it is
  not. The shape includes the tags, so a pass that retags _and_ moves in one go broke it — which is why this project
  carried a rule about doing those separately, and why a renumber stranded a four-star rating. That rule is now advisory
  rather than load-bearing.

  Deliberately **not** done: `history.toml` and the event log still name paths. Migrating them is the step that
  "invalidates every event ever recorded", and #63 has not settled whether events should name an entry or a encoding.
  The id exists, is cached, and is used where it already pays.

- **2026-09-11** **`arc-swap`, and the librarian gets a thread.** #206, the first of milestone "The engine". The fold
  moves off the thread that renders, and what the rest of the program holds is a pointer to the last thing published.

  **What it replaces is a control-flow trick.** `run_tui` owned the `Librarian`, lent it to the draw loop, and the loop
  had to _return_ — with a `Stop::Refold` — before anything could rebuild it, because the borrow outlived every command.
  A rating, a rescan and a `:view` change all unwound the loop and re-entered it. That was never about the UI; it was
  about who owned the fold. An `Arc` has no such problem: a reader keeps the one it took while the next is published
  beside it.

  **`arc-swap` 1.7** for the publication. A `Mutex<Arc<_>>` would do for today's single reader and was rejected on what
  comes next: §5.2 of the engine document has the MIDI input thread reading the mapping table the same way, and that
  thread may not take a lock. Adding the lock-free primitive once is cheaper than adding a mutex and removing it.

  **Writes are sent, not taken.** A `Mutex<Instance>` would be smaller and would put every writer back on the caller's
  thread, which is the thing being removed — a rating taken while a scan held the lock would block the render, which is
  I6's head-of-line blocking wearing a different hat. So a write goes on a queue and the next publication is how the
  caller finds out. `Desk::settled` exists for the two places that genuinely must wait: a test, and quitting.

  **`ui::record` still waits, and that is not a regression.** It was synchronous before — append, fold, flush, all on
  the render thread — because "the UI answers a rating immediately" is the whole reason the fold is held. What changed
  is _who_ folds. Removing the wait is #208's job: a position-driven fold that is already caught up costs one integer
  comparison per device, so settling after one appended fact stops meaning "refold 279,000 records". Making it
  asynchronous here instead would be the wrong repair — it would show a rating that has not been recorded yet.

  One fold was also deleted rather than moved. `run_tui` called `log.catalogue()` at open and `bridge::librarian`
  separately, so two folds of one log ran a few lines apart with two chances to disagree about what it said.

- **2026-09-11** **The three-thread INVARIANT becomes a set of properties, and the UI thread leaves the process.**
  Authorises `docs/design/gmuse-engine-architecture.md` (v0.2), which is the argument; this entry is what makes it so,
  on the standard the 2026-08-31 amendment set — a boundary may move to a principled place and may not erode by
  increments. That entry named this boundary in advance, as one of three then under pressure.

  **What §3 was for.** It is stated as a count, and the sentence after it gives the purpose: _"it is what determines
  whether playback is glitch-free"_. The property underneath is one thing — **nothing with a deadline shares a thread
  with work that can stall**. Decoding is expensive and variable, so it is off the callback; terminal input blocks, so
  it is off the decoder. Three was the number of threads it took to say that when the program was one process with one
  output.

  **What moves.** Four things, and only the first is new in kind:

  1. **One of the original three leaves the process.** The UI thread becomes the **control thread** inside an engine
     daemon, and the TUI becomes a separate process attaching over a socket. This is not the 2026-09-01 socket amendment
     again: that added a fourth thread _beside_ the three and argued it protected them. This moves one of them out.
  2. **The UI thread stops owning the `Store`.** §3's diagram says "owns Store/views". Library state moves to a
     librarian thread; the control thread holds an `Arc` to a projection and folds nothing.
  3. **The count goes from four to roughly thirteen roles** — control, librarian, scanner, loader, RT, IPC acceptor,
     per-client readers and writers, MIDI input, query workers, analysis, collector, recorder.
  4. **Two decks** means the audio path is no longer a single decoder→ring→callback chain.

  **What does not move, which is the larger half.** The callback rules stay absolute and get stricter: the engine
  document's I2 adds the retire-ring discipline, so heap objects the RT side drops are freed on another thread rather
  than being an allocation in disguise. The shape of every audio-carrying path is unchanged — something decodes off the
  RT thread, hands frames over an SPSC ring, the callback drains it — there are simply more chains. Nothing added shares
  anything with the callback.

  **And the streaming player is kept.** Decks decode fully into RAM because continuous varispeed, jog and reverse over a
  forward-streaming decoder cannot be made to work. That is a price **DJ mode alone pays**. Ordinary playback keeps
  streaming exactly as it does today, and the deck path is a second way to fill a ring rather than a replacement for the
  first. One song at a time, streamed, is what §1 asks for; the DJ work is not a migration away from it.

  **Why the count has to go.** Its purpose is not being traded away — it is being strengthened. What is retired is the
  literal form, which was a proxy. With several views, MIDI, two decks and a librarian, counting threads no longer says
  the thing it was shorthand for, so the enforcement moves onto the properties themselves: the callback rules, "the
  control thread does only cheap state transitions", one producer per ring with a stated policy for a full one, and the
  control thread never folding.

  The library work forced this independently of DJ mode, which is worth recording because it means the revision is not
  merely the price of an ambition. Query performance drove the access-layer split; that split put the projection behind
  an `Arc`; and a librarian that folds for hundreds of milliseconds cannot live on the thread that renders a UI.
  Separately, "playback continues while views attach and detach" is incompatible with the UI thread being in the process
  at all.

  **The cost, named so it cannot erode quietly.** "Three threads, not negotiable" is checkable by reading the code.
  "These properties hold" is not. Enforcement moves from a countable fact to a tested one — which is why the allocation
  guard in debug builds, the null audio backend, and the sample-accurate scheduling assertions across buffer sizes are
  load-bearing rather than garnish. **A property nobody tests is a property nobody has**, and that is the way this
  particular boundary would erode by increments.

- **2026-09-11** **`pyo3` and `maturin`, and the crate becomes a library.** The tools reach the library through a
  boundary already — a work list from `shelf query`, a proposal file for `shelf apply` — and what it lacked was a
  contract anyone could fail against. A tool wrote JSONL by hand and found out three hours later, in `apply`, whether
  the fields were the ones it reads; two spellings of one album id cost eight albums their genre.

  **`pyo3` 0.29 with `abi3-py311`**, so one wheel covers every later CPython and a release does not need rebuilding per
  interpreter. **`maturin`** builds it, because `cargo` cannot: an extension module's Python symbols resolve at load
  time inside the interpreter, so a plain `cargo build` fails at the link step. Both are behind a `python` feature that
  is **off by default**, so an ordinary build needs no interpreter and links against none.

  `just ci` still type-checks it, through `clippy --all-features`, which is what stops the binding rotting between
  builds — the one real risk of a target nobody compiles.

  **The crate is now a lib plus a bin**, which a `cdylib` requires. `src/lib.rs` holds the module declarations and
  `src/main.rs` became what it always described itself as: an adapter that parses a command line and calls in. Nothing
  moved and nothing changed — output is byte-identical over all 7,271 rows.

  It had one consequence worth recording: **items that were crate-private are public now, so public-API lints apply to
  them**, and sixteen `#[expect(dead_code)]` annotations stopped being fulfilled. They were apologising for a
  binary-crate artefact. `expect` asserts a lint _will_ fire and in a library that depends on the consumer, so they are
  `allow` now — which is the correct tool rather than a way around the error.

- **2026-09-10** **Squash off. Every commit on a branch lands on `main`.** The project merged fast-forward with
  `squash_option: always`, which turned each MR into one commit taking its subject from the **MR title** and discarding
  every body. Now `squash_option: never`; the merge method is unchanged, so history stays linear and a branch behind
  `main` still needs a rebase.

  **The bodies were the reason.** This repository keeps its reasoning in commit messages — what was measured, what was
  tried and failed, the rule that came out of being wrong — and squash kept a single subject line of it. The commits
  here are individually well-typed, so ten of them are ten changelog entries and ten explanations rather than one of
  each. git-cliff reads every commit; it was being handed one.

  **What it costs.** Every commit must now be a well-formed Conventional Commit, not just the MR title, because
  `filter_unconventional` drops a bad one silently and that risk moves from per-MR to per-commit. And the MR title stops
  being load-bearing, which removes one failure mode and adds a smaller one.

  **What prompted it.** `ddd51bb` is titled _"every fact says how much of a projection it can change"_ and contains
  that, plus `shelf compact`, plus `durable::rewrite`, plus six design-doc commits — because three MRs were opened
  against one long-lived branch and the third squash took everything still on it. Not repairable: rewriting a merged
  commit is a force-push and `main` is protected. The branching was the proximate error and **one branch per issue, cut
  from `main`** is the rule that prevents it; squash is what made the error unrecoverable rather than untidy.

- **2026-09-10** **The reason a launch does not scan is void: the tree is an output of the library, not an input.**
  Supersedes the 2026-09-01 entry below, whose rationale was that "a file appearing on disk is a change to the
  _library_, and a change to the library is something you make". That described a world where the music folder was an
  input the player read, and it is no longer the world.

  **A file cannot appear.** `shelf import` is the only door in, and it is a declaration a person makes (#94).
  `shelf render` places what is imported, under `{file_under}/{album}/{track} {title}` and nothing else
  (`storage-layout.md`). #160's gate refuses to catalogue anything no authority has named. Tools move files from
  _outside_ the library to _inside_ it; nothing watches a folder and ingests on its own. So every file under the root is
  there because the library put it there.

  **The tree is an output that moves, and `render` is what moves it.** A fact never touches the filesystem: renaming an
  artist or correcting an album changes what the tree _should_ be, and `shelf render` is the operation that makes it so
  and records `HoldingsAsserted` for what it moved. So there are three states and two different gaps between them:

  |                              |                                                |
  | ---------------------------- | ---------------------------------------------- |
  | where a file **should** be   | derived from the facts and `storage-layout.md` |
  | where a file **is**          | `HoldingsAsserted`, in the log                 |
  | where a file **actually** is | the disk                                       |

  **Only the second is a gap.** `is ≠ actually` is drift, and a scan closes it. `should ≠ is` is rendering lag and it is
  _cosmetic_: the folder names are out of date, nothing is unreachable, and no query answers differently. `reconcile`
  binds by hashing the audio and stores the path as a value — a library of `<uuid>.flac` would bind every holding
  identically. So `render` is for people and for other software that reads folders, and the readable layout's one
  non-cosmetic value is recovery: lose the log and a named tree is self-describing where a hash is opaque.

  **Which means a scan is not discovery.** It cannot find news, because there is no news to find — only drift: a file
  moved, removed or replaced by something outside the system. Reconciling a tree the library owns does not make the
  library "a function of gmuse having been run", because the run cannot teach it anything it did not already know. That
  was the whole of the objection, and it does not apply to this operation.

  Two further things changed underneath it. `reconcile` is no longer the old whole-library rebuild — it asserts
  `HoldingsAsserted` and nothing else, so its entire effect is to say where known bytes currently sit. And it is now
  cheap: the read cache (#188, #190) makes a warm reconcile **~1.3s against a cold 24s**, because a `stat` settles every
  unchanged file.

  **What this does not decide** is that a launch _should_ scan — only that "it would make the library a function of
  having run the player" is no longer a reason it must not. The remaining question is whether a second of work at
  startup is worth catching drift eagerly rather than when somebody asks, and it belongs with #196, where the scan gets
  a working door at last (#64).

  **Unchanged:** the cache stays derivable and may be discarded at any time (2026-08-27); `--rescan` remains the
  deliberate ask; #160's gate is untouched, so a scan still catalogues nothing.

- **2026-09-01** **A launch does not scan.** Startup rebuilt the library from disk every time, which made the library a
  function of gmuse having been run. That is backwards under #63's reading: the library is the thing and the player
  reads it. A file appearing on disk is a change to the _library_, and a change to the library is something you make —
  not something a launch notices on your behalf and applies without being asked.

  Two cases still scan, and only two. `--rescan` is the deliberate ask. An empty store is a first run, where the
  alternative is an empty library and nothing to explain it. So the cache stays **derivable**, which keeps the
  2026-08-27 decision that it is derived data and may be discarded at any time — what changed is _when_ the derivation
  runs, not whether it can.

  Measured: a launch goes from a 16s scan to loading 5774 tracks and starting. A file added to disk is correctly ignored
  until asked for, and `--rescan` takes it in.

- **2026-09-02** **View 5, the file browser, is removed — and `5` stays unbound.** It was the last surface in the UI
  that navigated a filesystem rather than the library, and under `domain-model.md` that is not a gap to fill but a leak
  to close: whether a file exists on this disk is a `Holding`, and the thing you navigate is a `Recording`. The
  alternative readings were both worse. Making enter catalogue-then-play reintroduces the special case `--repl` and the
  bare file argument were retired for (#61), and playing a path ephemerally gives a play event no recording to name, so
  history has nowhere to land.

  Removing a view leaves a hole in the digits, which are the bindings. **The gap is deliberate**: renumbering `scope`
  and `guide` to 5 and 6 would move two keys already in muscle memory to save one dead digit, and the empty slot is an
  honest record that something was there. §1's scope list loses "browser"; `Context::Browser` and `ViewId::Browser` go
  with it, so `:bind` no longer accepts `browser` and `:view 5` is an error rather than a no-op.

- **2026-09-02** **`uuid` is the one new dependency the model layer needs (#65).** `Id<T>` is a UUID because these ids
  are exported, compared against MusicBrainz ids — which _are_ UUIDs — and read by people; a private 128-bit id would be
  a gratuitous divergence in the one place the library talks to the outside world. `v5` is what makes convergent minting
  possible (#66), and `v4` mints everything with no binding to derive from.

  Measured cost: **one** new crate in the lock, `sha1_smol`. `getrandom`, `cfg-if`, `libc` and `serde_core` are already
  in the tree via cpal and serde, and `sha1_smol` is a dependency-free SHA-1 that exists precisely to avoid pulling the
  RustCrypto tree in for a non-cryptographic hash.

  `NAMESPACE` in `model/id.rs` is pinned by a test, for the reason `AudioId::SCHEME` is: every derived id in every log
  depends on it, so a change re-mints the library silently.

  **The gap this opens, honestly.** `Command::Rescan` in the UI is a stub — it notifies "rescan lands with `:rescan` in
  the app" and does nothing. While startup scanned, that stub cost nothing. Now it means the only way to take in a
  change is to quit and run `gmuse --rescan`, which is the coupling this entry set out to remove, one step further away.
  The obstacle is real rather than an oversight: the UI holds `&Store` and a scan needs `&mut Store`, so it wants the
  loop to return a reason for stopping rather than only ever stopping. Filed as its own issue.

- **2026-09-05** **A catalog has no roots: no membership decision reads a path (#94).**
  `[catalogs.dance] roots = ["~/MusicLibrary/Dance"]` made membership a fact about where a file sits. Three costs, all
  real: tidying a folder silently emptied a catalog, the same config on a second machine selected nothing, and a
  recording held on two devices under different prefixes belonged to the catalog on one of them.

  A catalog is now `collection`, `where`, or both, and one with neither holds everything. A collection is a stored fact
  — `CollectionEntryAdded` — so it survives a move and travels to another machine, which is exactly what a root could
  not do.

  **What replaced the Dance folder is a collection, not a rule.** Measured first, because a rule would have been better
  if one existed: no genre predicate reproduces `~/MusicLibrary/Dance`. The closest is 876 false positives — every one
  of its files carries a genre that files outside it carry too. So the folder was read once, and what it held became
  1,522 facts naming 761 recordings. The folder is now where those files happen to live.

  `general` is gone rather than converted. It was the complement of `dance`, and a complement is not a catalog: `-`
  exists now, so it is `:view -dance` — a term you can drop again, not a second body of music that has to be kept in
  step with the first.

  Where the music is has one answer, `library` in the config, which is what `all_roots()` was standing in for. Scanning
  used to union every catalog's roots so that switching catalogs needed no rescan; with catalogs holding no roots there
  is nothing to union.

  Cost: `Catalog::roots`, `Catalogs::all_roots`, `catalog::under` and the path half of `Catalog::contains` are deleted,
  and `roots` on a catalog is now a config error rather than a key that quietly does nothing — `serde`'s
  `deny_unknown_fields` was already there, so an old config says "unknown field `roots`, expected `collection` or
  `where`" rather than presenting an empty catalog.

  One bug this found in passing: `Catalogs::members` had its own copy of the membership rule, so a catalog defined by a
  collection came back holding the whole library — the copy only knew about the filter half. It goes through `named`
  now, and there is one rule.

- **2026-09-05** **A facet is a query field, and an axis costs one line (#117).** `Field::Genre` was one enum variant
  for one axis, so `version` and `function` were not expressible at all — the query language knew about genre because
  somebody had typed it out, and every axis after the first would have been another variant, another `texts_of` arm,
  another row field.

  `Field::Facet(Id<Facet>)` resolves any key in `facet::ALL`. **The id derives from the key**, so the parser needs no
  vocabulary to consult — which matters because `Filter::parse` is called from the config, from the `:` line and from
  smart playlists, and only one of those has a library to hand.

  **A closed list of axes, over open lists of concepts**, and the promotion test in `vocabulary.rs` is why. Adding a
  _concept_ needs no behaviour written for it, which is what makes a genre data. Adding an _axis_ needs a query field
  name and a place in completion — little, but not nothing, and it is an editorial act besides: facet analysis is a
  decision about how a collection is described, not something a tag should be able to invent. So a new axis is one line
  in `facet::ALL` and nothing else. `FacetDefined` still exists so a facet's label is in the log and syncs; the const is
  what the parser knows without one.

  The row changed shape with it. `Listed.genre: &[String]` became `concepts: &BTreeMap<Id<Facet>, Vec<String>>`, so the
  seam carries every axis rather than one, and the album-falls-back-to-track rule now runs **per axis** — an album-level
  genre and a track-level version no longer shadow each other.

  Two things that fall out and were worth having:

  1. `Field::NAMES` was a hand-written list beside the enum, and a hand-written list is exactly what silently stops
     asking. It is `Field::names()` now, built from `facet::ALL`, so an axis reaches completion and the "unknown field"
     message for free.
  2. **A name no axis claims is still a typo.** Resolving any word to a facet id would have been shorter and would have
     turned `genrre=techno` into a query that quietly matches nothing — the inference this project keeps turning into a
     declaration.

  What this makes possible, and what it does not: `function=dance-floor` is now typeable, and it is _not_ the Dance
  collection. A facet is the collection development policy; the collection is the holdings; the gap between them is a
  review. That distinction is #90's and this entry does not blur it.

- **2026-09-05** **`:collect` — the write the model never had (#118).** Collections have been facts since #92 and
  nothing in the player could make one: `add_to_collection` had callers only in tests, so the Dance collection had to be
  seeded by a tool pass that read a folder. `:collect <name>` files what the view holds, and reads no path at all.

  Three decisions, each of which could plausibly have gone the other way:

  1. **Entries are recordings; the view yields copies.** A recording held twice — a FLAC and an MP3 — is one entry, so
     collecting 100 rows may file fewer than 100 things. Correct rather than a rounding error, and the notification says
     the number filed rather than the number seen.
  2. **The shape comes from the view.** A sequence keeps its order and becomes an ordered collection; a set has none to
     keep, so its entries share `OrdKey::first()` — an arbitrary order that looks meaningful is worse than none. This is
     #92's set-versus-sequence being load-bearing: `:view <a playlist> & year>=2015` then `:collect` keeps the running
     order, because narrowing a sequence leaves a subsequence of it.
  3. **Filing into a collection that exists is a union.** Entries are row existence, so re-collecting the same view is
     idempotent and collecting a second view adds to the first. Emptying one is a retraction, a verb nobody has needed
     yet.

  **The bug this found, and the half-fix it got.** `:collect Dance` wrote the facts correctly and `:catalog dance`
  showed nothing. The librarian is a fold of the log _as it was at startup_, and a catalog resolves membership through
  `Listing` (#94) — so a collection written this session was invisible until the next launch. Ratings dodge this because
  a filter reads them from `Env::listened`, which is the live log; collections went through the seam and had no such
  route.

  `view::Fresh` overlays the one answer that goes stale: **entries come from the log, everything else from the
  librarian.** It is not the general fix. The general fix is the librarian being refoldable while the UI holds it, which
  is the same obstacle `Command::Rescan` has been a stub for since the launch-does-not-scan entry above — the UI holds a
  `&` and refolding needs ownership. Two symptoms, one cause, and worth fixing once rather than overlaying twice.

  Two smaller things fell out. `Rows::of_collection` and `Stack::resolve` now take `&dyn Listing` rather than
  `&Librarian`, which is what the seam was for and what made the overlay possible at all. And `:view clear` is not a
  spelling — a bare `:view` is the clear, which a test asserting the wrong one found.

- **2026-09-05** **A playlist is an ordered collection, and m3u is only an export (#118).** `Playlists` listed a
  directory of `.m3u8` files and `Saved` named one, so a **path was authoritative for what a list contained** — the same
  failure `roots` had for catalogs (#94), surviving in the one corner that refactor did not reach. Save a playlist here,
  open it on another machine, and it resolves nothing.

  A playlist is now `CollectionNamed { ordered: true }` plus one entry per recording, which is SKOS's
  `OrderedCollection` — a subclass of `Collection` adding a member _list_ where the parent has a member _set_. It is not
  a second store to keep in step with anything: view 3's list of playlists is a question asked of the fold.

  What that buys, beyond syncing: an entry is a **recording**, so a playlist survives a file being moved, being replaced
  by a better encode, or being on the NAS today and here tomorrow. Where the `.m3u8` said "the library does not know
  this path", the answer is now "no copy of this on this device", which is a better answer and a different one.

  **`persist_open` is a diff, not a rewrite**, and this is what the fractional index was for. Retracting every entry and
  re-adding the list would be correct and would cost two facts per track on every edit — a hundred-track playlist losing
  one row would append two hundred facts. Instead a greedy alignment walks the desired list against the existing one,
  keeps an entry's key where the recording still matches, mints a key between neighbours where it does not, and retracts
  what is left over. `y`, `p` and `D` then cost one fact each. A wholesale reorder degrades to retract-and-add, which is
  honest: nothing about the old keys describes the new order.

  **`:export <path>` is what m3u is for now.** One-way by construction — the reading half of `m3u.rs` is deleted, so an
  exported file going stale costs nothing and cannot re-key a list by location. `Playlists`, `Saved` and `file_stem` are
  gone with it, and `Ui::saved` with them.

  One test could not survive the change and was deleted rather than adapted:
  `an_unreadable_playlist_is_listed_rather _than_hidden` made a directory named `broken.m3u8` and asserted the row read
  "unreadable". There is no file to fail to read, so the state it guarded cannot be reached — a collection exists as
  facts or it does not.

  **What is still a file:** nothing about playlists. What is still _wrong_: `view::Fresh` from the `:collect` entry
  above is still overlaying the fold, and every fact the UI writes that a reader takes through the seam will want the
  same patch until the librarian can be refolded in place. Filed as its own issue.

- **2026-09-05** **The loop returns a reason, and `Command::Rescan` stops being a stub (#120).** The librarian was owned
  by `main` and lent to the event loop for its whole life, so nothing inside could rebuild it — the borrow outlives
  every command. Two symptoms, one cause, and the second only arrived with `:collect`:

  1. `Command::Rescan` notified "rescan lands with `:rescan` in the app" and did nothing, so the only way to take in a
     change on disk was to quit and run `gmuse --rescan`. The launch-does-not-scan entry above named this as the gap it
     was opening, and it stayed open for a milestone.
  2. A fact the UI wrote was invisible to any reader going through `Listing`. `:collect Dance` filed its facts correctly
     and `:catalog dance` showed nothing, because the librarian's fold was as old as startup.

  `ui::run` returns `Stop::{Quit, Refold, Rescan}`. `main` wraps it in a loop: the borrow ends, the fold is redone, and
  the loop resumes. **The state the `Ui` holds is untouched** — only what it reads _through_ is replaced — which is why
  this is a handful of lines rather than a rewrite.

  `Refold` and `Rescan` are separate on purpose. A refold redoes the log and leaves the disk alone, which is the common
  case and the cheap one; a rescan walks the roots first and then refolds. Conflating them would make `:collect` cost a
  filesystem walk.

  **What asks for one is the interesting part.** `:collect` does, because a catalog resolves membership through
  `Listing`. `:rate` does **not**, because a rating is read from `Env::listened` — the live log — so nothing the
  librarian would say has changed, and refolding on every rating would be a whole-library rebuild for an answer that was
  already right. That distinction is asserted rather than left to be rediscovered.

  **`view::Fresh` is deleted.** It overlaid the one stale answer and was a knowing patch: the next fact the UI wrote
  that a reader took through the seam would have wanted a second one, and then there are two producers for one row
  again. What survives from it is the better part — `Rows::of_collection` and `Stack::resolve` take `&dyn Listing`
  rather than `&Librarian`, which is what the seam was for.

  **And `Listing::at` is gone with it.** It turned a locator into a row — the one question on the seam that started from
  a path — and existed for exactly one caller: reading an `.m3u` back. Nothing reads one now (#118), so **nothing on
  this seam takes a location as an argument**. That is the rule `library-api.md` states, and #94 could only half enforce
  it while `at` was there.

- **2026-09-05** **A saved view is a fact, and a bare word names nothing (#119).** Two storage mechanisms for one idea:
  a collection was a fact in the log and a catalog was a table in `config.toml`. `data_dir` is documented as "where
  everything _you_ have decided about your music lives", and a named selection sitting beside the colour scheme does not
  sync, does not merge, has no provenance, and is invisible to the library everything else routes through.

  `ViewSaved { view, name, source }` stores **the source line**, not a parsed structure. It is diffable, it is what you
  typed, and it round-trips through the same parser the `:` line uses — so an operator added to the language needs no
  fact added here. `a_stack_round_trips_through_its_label` pins that, and it is load-bearing: if the round trip ever
  breaks, a saved view means something other than what was typed, silently, on reload, long after the edit.

  **By-name/by-value stopped being a fork.** #98 framed it as a hard choice, but a by-value saved view _is a stored list
  of recordings with a name_ — which is a collection, and `:collect` already writes one. So: definitions are late
  binding, collections are lists, and freezing one into the other only goes that way. Cycle detection then has exactly
  one place to live, because a collection is always a leaf.

  **Two things must be loud, and neither can be an empty result.** A view naming one that has gone, and a view that
  reaches itself, both look exactly like a query that matched nothing. `Resolved::problems` carries them out the way
  `demoted_at` already carried demotion, and the notification _replaces_ the "0 tracks" count rather than being
  overwritten by it — the first draft reported the problem and then said "view: saved=\"nope\" (0 tracks)" over the top
  of it. A loop is named with the path that closes it (`a → b → c → a`), because a stack overflow says nothing about
  which two definitions disagree.

  **`in=` and `saved=`, because a bare word names nothing.** `jazz` could be a collection, a saved view or a genre
  concept, and `filter.rs` had already settled the principle — `any` is an explicit pseudo-field "so 'search everything'
  is explicit rather than a bare word with implicit meaning". Three namespaces resolved by guessing would be that rule
  broken where it matters most, so `:view jazz` is an error that names all three spellings.

  Two bugs the tests found, both worth recording because both were invisible:

  1. `saved="mornings"` starts with `save`, so the `:view save <name>` prefix test swallowed a term that _named_ a view
     as the verb that _saves_ one. The space is load-bearing.
  2. `set_catalogs` kept `Where` terms and dropped everything else, which was correct while a catalog and a filter were
     the only two kinds of term — and silently discarded `in=` and `saved=` on every rebuild the moment they existed. It
     keeps everything that is not a catalog now, and the catalog noun being special-cased there is the last of it.

  **Still to come on #119:** `Catalogs`, `Active`, `Spec` and `[catalogs.*]` are not deleted yet — a catalog is now
  provably a saved view with a second name, and removing it touches config, the session, the cache and view 6. Kept
  separate so the mechanism lands proven rather than mid-rewrite.

- **2026-09-05** **The catalog is deleted: it was a saved view with a second name (#119).** #94 dissolved the _operator_
  — `active_catalogs = ["a", "b"]` became `a or b` — and left the _noun_ standing, because deleting it was not on that
  checklist. Going through what a catalog actually added over a saved view: several active at once is `+`, membership
  from a rule is a predicate, a rule and a collection together is `&`, switching by name is `saved=`. The only residue
  was "which are on persists across sessions", which is **one remembered source line**.

  The config made it worse than it looked. `[catalogs.jazz] collection = "Jazz"` was **two independent strings for one
  thing, free to drift** — the exact failure #90 talked us out of one layer down, reintroduced a layer up. And it sat in
  `config.toml`, which §13's own `data_dir` entry says is the wrong place: a decision about your music beside the colour
  scheme does not sync, does not merge, has no provenance, and is invisible to the library everything else routes
  through.

  Gone: `src/catalog.rs` entirely (569 lines), `Catalogs`, `Active`, `Spec`, `Catalog`, `CatalogTable`, `Ui::catalogs`,
  `Loaded::catalogs`, `Loaded::active_specified`, `Command::Catalog`, the `:catalog` verb, `What::Catalog`,
  `Stack::set_catalogs`, and `sync_catalog_terms`.

  **What replaced each piece.** View 6 lists saved views and its checkbox pushes or drops a term, so there is no second
  state to keep in step with the stack — `active_catalogs` was always the stack said another way. `active_catalogs`
  becomes `view = 'saved="dance"'`, one line. The cache remembers that line instead of a set of names, and
  `active_specified` is gone with it: it existed because `["all"]` was an explicit choice indistinguishable from the
  default once folded into an `Active`, and a source line has no such ambiguity — `Some("")` is not `None`.

  **One behaviour deliberately changed.** A catalog name that named nothing was _refused_, on the rule that a bad name
  must not half-apply. A saved view that names nothing now **applies and reports**. The difference is late binding: a
  catalog was a closed set in the config and could be checked as it was typed, where a saved view may be defined by the
  time you look again — and refusing the term would mean the reference could never be repaired from the line. So the
  term goes on, the result is empty, and the _reason_ is said, which is the one thing an empty library cannot say for
  itself.

  **An old config says where its keys went** rather than doing nothing. `[catalogs.*]` and `active_catalogs` are still
  parsed, for the sole purpose of producing a migration message that names the replacement — silently ignoring them
  would leave somebody with an empty view 6 and no idea why.

- **2026-09-05** **The picker, and one input holding two languages (#54, #119).** `<leader>ff` over the library,
  `<leader>fg` over the groups you have made. `nucleo-matcher 0.3` joins the tree — budgeted in §2 since the start and
  sanctioned by the 2026-08-28 entry, so this spends a decision already taken rather than making a new one.

  **The design question was the input.** §7 says the filter language has no bare words — `ferrell` alone is an error —
  and a picker inverts that, because bare text _is_ the interface. Both live in one line: whatever parses as a predicate
  narrows the candidates, and what trails it is the needle.

  The split is found by **longest parsing prefix**, not by scanning from the left: predicates are written left to right
  and a single forward pass would stop at the first word that parses on its own, leaving the rest of an expression in
  the needle. `year>=2015 & genre="techno" burial` keeps its whole predicate.

  **What makes the implicit split honest is that the title says which half is which** — `tracks · filter: … · fuzzy: …`.
  That is this project's existing habit rather than a new licence: an inferred state is fine when it is _visible_, which
  is what the pending-key indicator, the mode line and which-key already do. A silent guess is what §7 refuses. And a
  half-typed filter — `genre=` — is **named** rather than ranked as text, because a needle matching nothing looks
  exactly like a filter matching nothing.

  Three smaller decisions, each of which had an alternative:

  1. **Ranked over the rendered row**, not the raw fields — what you see is what you match, the rule the completion menu
     already follows. So the format template decides it.
  2. **`Enter` goes, it does not play.** `gp` and `x` are already different keys; a picker that played would be a third
     answer to a question the keymap has settled.
  3. **A tie is broken by the order the candidates arrived in**, so equally-good matches stay in library order rather
     than shuffling under the cursor between keystrokes.

  **Groups are a second picker rather than a merged list** (#119). "Is this a list somebody made, or a rule that
  recomputes" is the thing you most need to know before choosing one, and one ranked list would hide it — so a
  collection and a saved view are both there, each saying which it is.

  `/` is untouched. It is vim's in-list search and moves the cursor inside what you are looking at; the picker searches
  everything and takes you there. Vim has both, for the same reason.

  One thing the verb-table audit caught: `find` parsed its argument in a nested match inside `command`, and the audit
  scans that function's source for match arms — so `tracks` and `groups` read as two more verbs. It is a helper function
  now, below `split_word` and out of the scanned range, which is the shape `playlist_spec` already had.

- **2026-09-05** **"In no crate" is a question, and the folder proposes rather than files (#119).** The last two pieces,
  and both turn on the same rule: a folder is evidence somebody left, read once and discarded.

  **`collected` is a filter field**, so `!collected?` finds what nobody has filed. It follows `rating`'s rule rather
  than `play_count`'s — in nothing is **missing**, not nought, because a track nobody has decided about is different
  from one decided to belong to nothing. And it is a **count**, not a flag: collections overlap on purpose, so
  `collected>1` finds the pop song that is in Dance when you are DJing with it and in General the rest of the time.
  Counting _distinct collections_ rather than entries, because a playlist holding one track twice is one collection that
  holds it.

  The alternative was `-in="Dance" -in="General"`, which is expressible today and does not generalise: it needs every
  crate named, so adding one silently breaks the query.

  **A scan reports what landed in a folder and belongs to nothing**, and `gmuse shelf unfiled` turns that into a
  proposal. It is the review queue, not a rule — a scan that filed by folder would make the filesystem authoritative
  again, which is exactly what #90 and #94 took away, and moving a file would silently re-file it. The proposals go to
  stdout and the summary to stderr, so a redirect gets only the facts.

  Verified on the real library: **0 proposals**, because everything on this disk is already in `Dance` or `General`. The
  14 rows in no collection are held on the NAS, so a scan of this disk cannot see them and correctly says nothing about
  them.

  A file sitting directly in a root gets no suggestion — there is no folder to suggest — which is asserted rather than
  left to be discovered by somebody with a stray `.wav`.

- **2026-09-05** **`:freeze` — a question becomes an answer somebody kept (#95).** It reported and wrote nothing:
  _"froze {name} — {n} tracks; `:playlist save <name>` keeps it"_. Two gestures, because the first had nowhere to put
  the answer. A name is what it was missing.

  **The crossing already existed as `:collect`**, so the work was not the materialisation but the **provenance**.
  `CollectionFrozen { collection, source }` records what a crate was frozen from — a separate fact rather than a field
  on `CollectionNamed`, because naming a list and saying where it came from are two claims and one of them is optional:
  a hand-built crate has no source. It also avoided rewriting the meaning of the 6,311 `CollectionNamed` facts already
  on disk.

  That single field is what makes three things possible, and none of them worked without it: _"this crate was
  `rating>=4`, in March"_ is answerable; **drift is visible**; and re-freezing is a keystroke rather than a retype.

  **Re-freezing re-runs the recorded source, not what is on screen.** Refreshing a frozen crate from whatever view
  happens to be open would silently make it a different crate — the one thing recording the source exists to stop. The
  report is the **drift** (`good: +1 −0`) rather than the total, because the count is the boring half and nobody
  re-froze it to learn that.

  `:collect` records a source too, always. There is no reason to file a view into a collection and _not_ record the
  query that produced it, so the two verbs differ only in emphasis: `:collect` is about the filing, `:freeze` about the
  crossing, and `:freeze` on a name that is already frozen is the refresh.

  **Order comes from the view's shape**, as it already did for `:collect` — a sequence keeps its running order, a set
  has none to keep. #95 asked whether freeze should copy the order; the answer is that **the sort you were looking at is
  not copied**, because a sort is a presentation (#97) and storing it as entry positions would turn a display choice
  into a fact about the list. A rule _is_ frozen ordered, because a rule carries a sort and what was on screen is what
  freezing keeps — and that order is now a fact that will diverge from the rule's, which is the point.

  Not a third kind of noun, as the issue insisted: a list that "reduces as you play it" is
  `in="keepers" & play_count=0`, and it is a question.

- **2026-09-05** **`levels` is a value on the view, and the numbered views are cmus's shape (#97).** §9–§10 said two
  axes were conflated: **views 1 and 2 are one set drawn two ways**, and the way was hardcoded. `ViewSpec.levels` was
  already the switch in the design; it was not a switch in the code.

  `Shelf::grouped(librarian, members, levels, smart)` takes the two fields to group by, and `Tree::of` is **gone** —
  artist-then-album is what `grouped` does when nobody says otherwise, so the pair is a default rather than a shape.
  `:view levels genre,album` is a grouping that could not be expressed at all before.

  **Presentation, not membership.** Regrouping redraws and never refilters: the rows are the same either way, which is
  the distinction §9 exists to draw and is asserted rather than assumed.

  **A correction, made the same day.** This entry first said the tree had to "ask the model rather than the tags",
  because `Listed` carried both `album` and what was then called `tagged_album`, and it claimed a test had caught the
  difference. Both were wrong, and the question "shouldn't that all be in the library?" is what exposed them.

  Nothing here reads a file. Both fields come from the fold, and they differ **only** in that `album` substitutes a
  stand-in where nothing named one. The special case written for it was a no-op — removing it broke no test, which was
  checked rather than assumed, and the test named above never failed at any point.

  What the episode actually found is that **the field name lied.** `tagged_` said "off the file" about a value that is a
  fold like every other, and the person writing the grouping code believed it. Renamed to `album_if_named` and
  `title_if_named`. Tags are a serialisation the library consumes at import and never consults again — `library-api.md`
  says exactly that — so a field name contradicting it is not a cosmetic problem.

  **The multi-valued precondition, honoured** — `tree-view-design.md` §6 named it before it could happen. A row has one
  artist and many genres, so grouping by genre files a copy under **each**, which is right: you want a techno track
  under `techno` and under `electronic`. All three call sites §6 listed are wired — counting is distinct
  (`Shelf::distinct`), enqueueing dedups (`artist_tracks`), and revealing finds every node (`Shelf::everywhere`) —
  because without them "a subtree reports 42 tracks and enqueues 37". `Levels::outer_is_multi` is a **declared** answer,
  not a guess.

  **`levels` is now the single answer to "how is this grouped".** `:view 2` sets `Levels::Flat` and `:view 1` sets the
  default pair, and setting the value picks the drawing — so the two can no longer disagree, which they could while the
  view was a mode and `levels` did not exist. That is §9's first sentence made true: **views 1 and 2 are one set drawn
  two ways.**

  **The `View` enum is down to what genuinely differs.** Two of its six members were not views:

  - **`Sorted` was `Tree` drawn flat** — one set, two drawings, and `levels` says which. `2` still works and means
    `levels: flat`.
  - **`Filters` was a picker wearing a tab** — `6` opens the groups picker now, which ranks over the whole set instead
    of listing one you scroll (#54, #119).

  What is left is `Tree`, `Playlist`, `Queue` and `Guide`: three things that hold genuinely different rows, and prose.
  `Context::SortedLibrary` and `Context::Filters` went with them, and neither had a binding.

  **Three behaviours had to be told apart by hand**, and each was a place the flat drawing is not a tree: `}`/`[[` read
  album dividers rather than tree depths, `a`/`y` add the row under the cursor rather than a subtree, and the cursor and
  scroll stay separate per drawing — switching between flat and grouped does not scramble the other's position. Two of
  the three were caught by tests rather than by reading, which is the argument for having done it as its own pass.

  **One test caught the change itself.** `gt_steps_through_the_views_and_wraps` counted a lap as `ALL.len() - 1` because
  it started one view in; with four members instead of six it landed on the guide. It counts a full lap from where it
  starts now, which is what it meant.

  **Splits, which close #97.** `^wv`, `^ws`, `^ww`, `^wo` — vim's four that matter, on vim's prefix. What `g<Tab>` does
  not cover: it moves _between_ two lists, and a split is for digging in one while collecting into the other.

  **`ui.view` is the focused pane**, and that is the whole design. Every other `match ui.view` in the file already sends
  keys where they should go, so nothing but the drawing had to learn a split exists — focus swaps by swapping which view
  is `ui.view`, and closing keeps the focused one.

  Two limitations, both deliberate and both stated to the user rather than discovered:

  - **You cannot split a view against itself.** The cursors are per view _kind_, not per pane, so two panes on one list
    would move together. Giving a pane its own cursor is the general fix and is a bigger change than the feature
    warrants — `g<Tab>` already covers "the same list twice".
  - **The second pane starts on where you were.** Not a choice offered, because the case is "these two lists I have been
    going between", and that is exactly what `previous_view` holds.

- **2026-09-05** **Quantification is explicit, because the language had both and said neither (#93).** §7's complaint
  was concrete and it was already true here: `genre="rock"` meant _some_ genre is rock and `genre!="rock"` meant _none_
  is — **one syntax, two quantifiers, chosen silently.** A row carries many genres, both readings are useful, and a
  reader could not tell which they were getting.

  `any(f)` and `all(f)` say which. Written as a call because it reads aloud — "all of its genres" — and because `any`
  alone is already the search-everything pseudo-field; the parenthesis is what tells them apart, and it has to be read
  as a _token_, since the grammar already has grouping parens and `any` and `(` arrive separately.

  **The defaults do not change, and that is the decision.** Making both default to `any` would have made `genre!="rock"`
  true of a file tagged `Rock; Blues Rock`, which is not what anybody means. So the asymmetry stays, and what changes is
  that it has a name (`Quant::Unsaid`, resolved by the operator) and the other reading is sayable. A test asserts each
  bare form against its explicit equivalent, so the two cannot drift.

  **`all` is not vacuously true on nothing.** Formal logic says every element of the empty set satisfies anything; a
  listener asking "all of its genres are rock" about an untagged track does not mean yes. `!genre?` is the question they
  would be asking and it is a different one — the same reasoning that already makes a missing tag match nothing,
  including `!=`.

  **Closure, and one meaning per character.** `genre*="electronic"` is it or anything narrower at any depth; `genre+` is
  strictly narrower. SPARQL's spelling, and an earlier draft used `+` for both and justified it by the borrowing — which
  stops being true the moment the meaning changes.

  **It walks up, not down.** The question is whether one of a row's own genres sits under the concept asked about, and a
  row has a few genres where a taxonomy has thousands of descendants. So `Listened::broader` climbs from each value,
  which also makes the glob case fall out: an ancestor is tested against the pattern like anything else.

  **The bound is not optional** (#79). A hierarchy is curated by hand, so a loop is a question of when rather than
  whether — `Techno` narrower-of `Rock` narrower-of `Techno` is two edits nobody would make together and somebody
  eventually will. A visited-set stops the cycle and a depth of 16 is the backstop beside it, the same pair
  `view::Stack` uses for saved views. Asserted against the real fold, not a fixture: a cycle terminates and each concept
  is visited once.

  The marker goes **inside** a quantifier — `any(genre*)`, not `any(genre)*` — because closure is about the field and
  the quantifier is about how many of its values must satisfy the test. The parser reads it after unwrapping the
  brackets, which is what makes both spellings of the field name work.

  **Measured, and it was slow.** #93 asked for this before it was discovered, and it was right to: a closure pass over
  20k rows at depth 12 took **60 ms**, against §2's 0.92 ms for the flat language. Sixty times, for something that runs
  on every keystroke.

  The fix is a memo owned by the **compiled filter**, because the cost is per _label_ rather than per row — a library
  has a few hundred distinct genres over thousands of tracks, so the second occurrence is a hash lookup. **7.5 ms**
  after, and the perf test's bound is six times that: loose enough not to fail on a loaded machine, tight enough that
  losing the memo fails there rather than in somebody's hands.

  The memo lives as long as the filter is in use and cannot go stale against a refold, because a refold compiles a new
  filter.

  **Correlated paths, and the bracket is what correlates (#93).** `genre[basis=discogs & confidence!=confirmed]` — the
  review queue, and the last thing #93 asked for.

  The open question was whether two clauses on one path mean _one_ claim has both or two independent existentials.
  **Three traditions converge**: SQL is correlated by default because `WHERE` runs per joined row; SPARQL correlates
  through a variable naming the node; XPath has both and distinguishes them by where the brackets go — and its
  uncorrelated form is a famous pitfall precisely because it _looks_ correlated. The rule underneath all three:
  **correlation requires naming the thing being correlated.**

  Kimball answers the other half of the same shape. A multi-valued dimension is a bridge table, and his warning is
  double-counting — sum a fact across a bridge and it counts once per member — with a weighting factor as the remedy,
  chosen per report against the plain existential. That is `Shelf::distinct` versus summing nodes (#97): distinct is
  right for "how many tracks" and would be wrong for anything genuinely allocated. He also permits a designated _primary
  member_; gmuse refuses that for genre, because it is the "show the best one" collapse already rejected.

  **`|` inside a bracket is refused, and the proof is why.** ∃x(P∨Q) ≡ ∃xP ∨ ∃xQ, so `genre[a] | genre[b]` says
  everything `genre[a | b]` would; it is exactly ∃x(P∧Q) that does not distribute. So conjunction is the only thing the
  bracket is _for_, and refusing disjunction inside leaves one way to write each meaning rather than two.

  **The bracket does not breach the tree-shaped fragment.** It is a local existential scope, not a variable shared
  between branches — so "recordings whose remixer also produced something else I hold" stays unreachable, which is the
  line #93 drew on purpose.

  The dotted form `genre.basis = discogs` is **not** accepted, though #93 proposed it. It is the spelling that is
  ambiguous when repeated and that XPath's pitfall is made of, and every single-clause use of it is `genre[basis=…]`
  anyway — so refusing it is one rule instead of two, and the error names the bracket.

- **An edit to a view is an edit to the facts that made it true — and authorship, not ambiguity, decides whether it is
  offered** (#96). An earlier draft said a view is editable when the translation is unambiguous. That is too permissive:
  `year >= 2015` translates perfectly well — assert a year — and adding a row to it must not rewrite a release date. So
  every queryable field is classified once, in `Field::authorship` and `HistoryField::authorship`, into **observed**
  (nothing a person asserts), **observed but correctable** (a correction, never a drag), and **authored** (yes, and it
  names the fact). Only the last offers a control; the other two refuse by name in the status line, because a control
  that quietly does nothing is the failure this is against.

  **The confirmation names the fact, not the list**, and the typed `Assert` is what makes that honest: the sentence in
  the status line and the fact appended to the log are one value, so the affordance cannot describe a write it does not
  perform. An earlier pass of this said "not written yet" and would have shipped as a working feature.

  **Only an equality names a value, and only an exact existential clause names a concept.** `rating>=4` says four, five
  or nothing in particular; `genre*="electronic"` is satisfied by anything narrower, so writing the broad concept is a
  guess; `all(genre)="techno"` is not made true by adding techno to a row that has others. Each of those keeps the
  field's authorship — the refusal is about the value, not the field — which is what an earlier draft conflated, making
  a year read as observed.

  **Removal is an override, never a retraction.** The log is append-only and merges by union, so deleting a
  `ConceptAsserted` brings it back on the next sync and destroys the provenance saying where it came from. So
  `ConceptRetracted` carries a `basis` and the fold keeps `assertions_retracted` keyed to the strongest basis that ruled
  the pair out. `Source` is ordered weakest-to-strongest — `TagVariant, Inferred, MusicBrainz, Discogs, Manual` — and an
  assertion lifts a shadow only when it is at least as strong as the one that cast it. A re-import cannot undo a
  person's correction, and a tag cannot undo what a person ruled out.

- **`:edit` is a bigger window, not a translation — and the "opaque view" it was going to need does not exist** (#99).
  The third tier of `query-language.md` §11 writes the composed view out one term to a line, opens `$VISUAL`/`$EDITOR`,
  and reads it back. The document is **the same grammar as the `:` line** plus two things that are inert on one line: a
  newline is whitespace, and `#` starts a comment. Comments are carried on the term they were written above, so the
  round trip is lossless — a format that admits comments and then eats them teaches people not to write any.

  A term after the first still carries its sign. A line without one is an error naming the line, because an implicit
  operator is the guess that would make a view mean something nobody said.

  **The issue predicted that a core-edited query would have to go opaque**, withdrawing the droppable-term UI for the
  whole view. It does not. `A + (B & C)` is unreachable from a left fold, which is exactly what the editor is for — but
  a parenthesised sub-view is one `What::Group` term: droppable as a whole, and the affordance stands down only _inside_
  it, where the fold is no longer a list. The genuinely non-tree shapes the issue worried about — two paths meeting at a
  shared variable — are the ones #93 already refused at the language level, so no parseable query can reach that state.
  The control is withdrawn where it would lie and kept everywhere it would not.

  **`all` had to become a keyword**, which was a live round-trip bug rather than a new need. `normalise` puts
  `Everything` in front of a stack that begins by narrowing, so `:view -in="Christmas"` labels as `all -in="Christmas"`
  — and `all` was a bare word, refused by the #119 rule. Every stack starting with an exclusion failed to parse back,
  and a saved view of one reported as _missing_: the "a broken definition looks like a missing one" failure, caught by
  the thing built to prevent it.

  **Nothing is guessed when no editor is named.** git and crontab fall back to `vi`; this refuses and says which
  variable to set. Dropping somebody into a modal editor they did not choose is the same inference this project refuses
  everywhere else, and the fix is one line of shell.

  The loop returns `Stop::Edit` rather than shelling out under the draw, which is the door `Stop::Refold` already
  opened: the terminal belongs to the loop, so anything that wants it runs where the loop is not.

- **The two shapes a view could not express are `genre@1` and `derived[…]`** (#79, `domain-model.md` §5). Both were
  being faked: the `Broad; Style` tag convention exists because a level could not name a place in a vocabulary, and the
  derivation relations sat in `spine.rs` as types nothing minted.

  **A level is a field and optionally a depth.** `genre@1` is the broadest ancestor, `genre@2` the one under it —
  1-based from the top, as the issue writes it. A concept shallower than the level asked for **stands for itself**
  rather than vanishing, which is Kimball's answer for a ragged hierarchy and the only one that keeps every row in the
  tree. A depth on a field with no hierarchy is refused at the parse, and a depth on an axis whose hierarchy is _empty_
  is refused at the command — it would otherwise draw exactly the tree `genre` draws, which is a control that quietly
  does nothing.

  **`derived[…]` is #93's correlated bracket applied to the second relation.** One _edge_ must satisfy every clause, so
  "a remix at most two hops back" cannot be answered by a remix somewhere and something two hops away. A kind is
  compared for equality and a depth is ordered, which is the difference from the claims bracket and the reason it has
  its own parser. The walk is bounded at 16 whatever is asked: edges are rows and rows union, so two devices can author
  a cycle neither meant, and a render must answer rather than hang.

  **The two traversals do not reach into each other.** Recording derivations answer "which versions", work derivations
  answer "which arrangements". A cover is in neither: it realises the existing work, as a performance does.

  Three gaps closed on the way, each found by making the feature work rather than by looking for them:

  - **`narrows` catalogued no concepts.** An edge named two ids that no fact resolved, so `at_level` had an ancestor it
    could not put a name on and silently drew the specific term as the broad one. It now catalogues both, the rule
    `assert_concept` already followed. The visible consequence is that a broad term no hierarchy places is a backlog
    item like any other, which it should always have been.
  - **The librarian carried the hierarchy's edges but not its terms.** #116 brought the edges across so `depth` could
    walk; the labels stayed behind, and nothing noticed until something needed to _name_ an ancestor rather than count
    hops to one.
  - **`#[expect(dead_code)]` on the two derivation kinds retired itself** the moment a log fact used them, which is what
    #113 chose `expect` over `allow` for.

  **Many-valued dimensions do not sum, and the status line says so.** A track with three genres is under three nodes, so
  the count reads as "groups a track is under" and never as a partition. Only a facet declares itself multi-valued —
  artist is one value on a row in this model, so the issue's three-credited-artists example does not apply yet and the
  message does not claim it does.

  **Neither shape has a producer.** Nothing imports a genre hierarchy or a derivation edge, so both are the model ahead
  of its callers — the established shape here, and stated rather than left to be discovered.

- **Curating the genre hierarchy, and the three bugs only real data found.** #79 made `levels genre@1` expressible;
  nothing produced an edge, so on this library it drew exactly the tree `genre` drew. `Proposal::Narrowed` and
  `NarrowingRetracted` are the producer's route in, and `gmuse-tools genre hierarchy` is the pass. On 6,485 rows the
  library's 239 genre terms collapse to **27** at level 1: sixteen broad terms, the no-genre stand-in, and ten terms
  deliberately held back for review.

  The curation itself was already half-written — `propose.IMPLIES` mapped a style to a broad term so a tag string could
  carry the hierarchy. `IMPLIES` is **derived** from the taxonomy now rather than kept beside it: the same judgement
  written twice drifts, and the term a tag gets and the term the model climbs to have to be one.

  **A term nobody has placed goes to a review file, not to a guess.** Eleven survive there and each is a real judgement
  — `Gospel` is Folk and Blues and Funk depending on the record, `Lo-fi` is a production aesthetic rather than a place
  in a taxonomy, `Tom Waits` is an artist name in a genre field. Writing down _why_ a term is unplaced is what stops a
  later pass rediscovering it as an oversight and picking one.

  Three bugs, none of which any test would have found, because each needed a taxonomy with real shape:

  - **A self-edge reached the log.** `in_facet` normalises a label before minting, so `Non-Music` and `Non-music` are
    one concept — and the guard compared _strings_. It compares ids now. The mirror of it also bit: the tool's
    punctuation-stripping key treated `Hip Hop` and `Hip-hop` as one concept when gmuse does not, so a real edge was
    dropped as a self-edge and `Hip-hop` sat unplaced at level 1. Two identity questions, and they are not the same
    question.
  - **A shortcut edge breaks level numbering.** `Stoner Rock` named both `Metal` and `Rock`; `Metal` is already under
    `Rock`, so the shortcut put `Stoner Rock` one hop from the top and `genre@1` reported `Metal` as a broad term. A
    second parent is for a genuinely second lineage, never for restating the first.
  - **"Already in the log" is not "still true".** The apply skipped facts already present so that replaying a file could
    not count a play twice. But assert/retract pairs fold to a set: a record may be there _and_ have been taken back —
    two lines up in the same file, or a week ago in the history. Judging by presence left whichever came first as the
    last word, and a pass that meant to move an edge silently lost it. Paired facts are judged against the **fold** now,
    with only the last word about a pair in a batch surviving.

  **`at_level` returns every ancestor, not one.** A concept may be narrower-of two things — `Bossa Nova` is Jazz and
  Latin — and picking one is an arbitrary tie-break that decides which node a row appears under. Genre is a many-valued
  dimension whose counts already do not sum, so both is the consistent answer. It also returns the _vocabulary's_
  spelling rather than the tag's, because asking for a level is asking for the codelist's terms.

- **The two-instance round trip covers every fact family now, not two of them** (#69). The acceptance test for the
  no-coordinator bet was written when the model had a rename, a genre and a play in it. Everything added since has its
  own merge rule, and a merge test that exercises two of them proves very little — the live log is 40% collection
  entries, and not one crossed a sync in a test.

  One assertion carries it: the two folds are **byte-identical** after merging both ways. Collection entries, a
  collection's name, a saved view, a rating, a hierarchy edge and a derivation now all go through it, plus #96's
  precedence — a tag re-import on one device against a person's retraction on the other, which has to come out the same
  whichever direction the sync ran.

  **Two findings about ordered collections, and they are different.** Filing one track into an _unordered_ crate on two
  devices is one row: every entry takes `OrdKey::first()`, so the key is the recording and the rows union. Appending to
  an _ordered_ one is not: a fractional index is computed from what the device can see, so two machines each holding a
  different track compute different positions for the same new one and it lands twice. The copies still agree — this is
  not a convergence failure — but "append the same track on both laptops" gives a playlist holding it twice. Pinned by a
  test so it is a known shape rather than a surprise.

- **`:view` was 1.3 seconds and none of it was the query** (#121). `Shelf::grouped` built the tree over the _whole_
  library and filtered afterwards, so narrowing to 786 rows cost exactly what drawing all 6,485 did — and every `:view`,
  `:filter` and `:view drop N` paid it again. On this library `in="Dance"` now takes **1.2 ms**, a thousandfold less,
  and `all` takes 8.9 ms rather than 1.26 s.

  Three causes, and measuring first said which order to fix them in — the whole point of the measurement rule. The build
  loop was 236 ms and the sort was **1.03 s**, four fifths of the cost, so the sort came first:

  - **`sort_by` asks for the key once per comparison.** `sort_within` called `row_of` inside the comparator, and
    `row_of` builds a whole `Row` — cloning the title, resolving the album and who the recording files under — then
    `to_lowercase` allocates again. `sort_by_cached_key` asks once per row.
  - **The key used one field of the `Row` it built.** Taking the title straight off the spine is what turned a 116 ms
    narrowed draw into 1.2 ms; the rest of `row_of`'s work was never wanted.
  - **The tree walked the library rather than the members.** It asked `listings` per recording, allocating a `Vec` each,
    before anything was tested for membership — a ~100 ms floor no amount of narrowing could get under. It takes the
    member set now, and a member _is_ a copy, so the listing is built rather than looked up. `Librarian::listings` had
    no other caller and is gone.

  **A benchmark is part of the fix, not a nicety.** The resampler has had one since the beginning; the library path had
  none, so the only signal that drawing a view had become slow was somebody noticing that it felt slow.
  `just bench-shelf` prints resolve, copies and shelf separately — kept apart because they fail differently, and a
  single end-to-end number would have hidden which one went wrong. It asserts a **generous** bound: what it has to catch
  is not a 20% drift but the shape of this bug, where narrowing cost what drawing everything did.

- **A cold scan is disk-bound, and now it says so at 3.2 GiB/s instead of 1.7** (#122). The first scan of this library
  took 24.7 s for 6,500 files; the second takes 27 ms, because the mtime fast path was never the problem. What costs is
  reading a file that changed, and there is one reason: `audio_id::of` demuxes and hashes **every audio packet**, which
  is the durable identity and not negotiable without re-minting the library. The tag read beside it is 24 ms against 1.3
  s.

  So it is I/O over 42 GiB, and it was single-threaded on a ten-core machine. The walk now decides what to read and the
  reading fans out over `std::thread::scope` — no dependency, because §12 says threads and channels and the work is
  known before it starts. **24.7 s → 13.1 s.**

  **That is the disk, not the threads.** 42.1 GiB in 13.1 s is 3.2 GiB/s, which is an NVMe at full tilt; the serial
  version managed 1.7 GiB/s because one request at a time does not fill the queue. More threads would buy nothing, and
  the only remaining lever is hashing less — which means a new `AudioId` scheme and re-minting every id in the library.
  Worth writing down so the next person does not go looking.

  **Lanes are strided, not chunked.** A library is not evenly expensive: an album of FLACs sits together in the walk, so
  contiguous chunks hand one worker every large file on the record while the rest finish early.

  **The order out is the order in**, because `arrived` feeds move detection and the report is something a person reads.
  Each worker writes into its own slot rather than pushing to a shared vec, so nothing depends on which thread finished
  first — pinned by a test with more files than the parallel threshold.

- **Rebuilding the librarian was 467 ms, and four fifths of it was one mistake made five times** (#123). `Stop::Refold`
  runs after `:collect` and `:add-view`; `Librarian::draw` materialises every row once at the end of the fold so a view
  can borrow rather than compute (#86). Drawing one row asked five questions and four of them were **linear scans of a
  whole collection**:

  - `Reference::credits_on` filters a `Vec<Credit>` by subject, and `drawn_of` reached it **four times per row** — once
    or twice through `files_under`, then again for `artist` and for `album_artist`.
  - `Encodings::of` filters every encoding by recording, allocates a `Vec` and sorts it — once per row, only to ask
    `!is_empty()`.
  - `Vocabulary::depth` walks the edge set per hop, called once per assertion: 26,490 calls about ~250 concepts.

  **467 ms → 95 ms**, and `draw` itself 333 ms → 21 ms.

  **The fix is not "index it".** A scan is the right answer for one row: building a map of 13,000 credits to ask about
  one album is worse than walking them. So `Lookup` names the choice — `Direct` for a caller drawing one row, `Built`
  for a pass over the library — and the call site picks. Both arms end at `line_of`, which stays the only place that
  decides how a credit line reads, so what differs is how the credits were _found_ and never what they say. Measured
  after: a single `row()` is 43 µs, unchanged.

  Two attempts got this wrong before it was right, and both were caught by measuring rather than by reading. The first
  indexed only `files_under` and moved nothing, because three of the four scans were further down in the `Drawn`
  construction. The second made the single-row path build the index — correct, and a regression on the UI path, where
  `row()` is called per frame.

  `just bench-fold` prints ingest, catalogue and from_catalogue separately, beside `just bench-shelf`. The fold had no
  measurement, which is why nobody noticed.

- **Quitting is `:q` and not a key.** cmus binds `q`, and this did too. A single unmodified letter sitting between `n`
  and `p` is one slip from ending a listening session — and it is the one action here with nothing behind it to undo.
  The session is restored on the next launch, so nothing is _lost_, but "nothing is lost" is not the same as "nothing
  happened". Two keystrokes and a colon is what vim charges for the same thing, and `:q` was already an alias.

  `q` is left unbound rather than given to something else: a key somebody's fingers know as _quit_ should not quietly
  become a key that does a different thing.

- **A claim's basis does not survive the seam, and the info panel is what noticed** (#125). `Librarian::concepts` is the
  collapsed form — distinct labels, best first — which is what a row draws and which drops the basis and the confidence.
  Those are exactly what #96 made the model keep, so the panel carries `Librarian::claims` beside it: every assertion,
  retracted ones included and marked.

  **A retracted claim has no asserter behind it.** The fold's `retract` sweeps every asserter of a pair out of
  `assertions`, so a concept somebody ruled out leaves a retraction and nothing else. Saying `manual` asserted it would
  be a lie; dropping the row would lose that anybody decided. `said: Option<(Source, Confidence)>` is the third answer,
  and the shadowed assertion is still in the log where the facts section will show it — the claims section is the state,
  the facts section is the history.

  **Scanning is right here, and that is not a contradiction of #121 and #123.** Those were the same mistake twice: a
  linear walk, correct for one question, run once per row. `about` answers about _one_ focus when a panel opens, so
  walking the credits and the entries is the cheap way and an index would be the expensive one. Measured rather than
  assumed — 66µs for a recording on the real library, 175µs on a synthetic 6,500-row one — and guarded by a test, since
  "this one is fine to scan" is exactly the sentence that stops being true.

  **Credits are links, not claims, and that is a gap rather than a decision.** `Catalogue::credits` is keyed by source;
  `reference::Credit` drops it at the fold. Carrying it means changing a serialized type, so it is recorded here rather
  than smuggled into this issue.

- **The info panel is view 8, and `K` opens it** (#126). vim's `K` is "look up the thing under the cursor", which is
  exactly what this is, and it was unbound. Provisional on purpose: the issue said to decide once the panel exists and
  can be lived with, and a week of using it is worth more than an argument now.

  **It opens on the recording, not the copy.** The row under the cursor is an `Id<Encoding>` — the bytes — and almost
  everything worth reading is about the performance, so opening on the encoding would put the useful screen one hop away
  every time. The copy is under `copies`, one keystroke away, and the section is named for what it is rather than hiding
  that a row is three things.

  **`escape` is `view-last`.** Leaving the panel is going back to where you came from, and that mechanism already
  existed — a second one would be a second thing to keep in step.

  **Folds are keyed by section title, not by position**, so they survive following a link (#127): "I never want to see
  the fact log" is a standing preference, not a fact about the page you happened to be on.

  The panel joins `current_len`/`current_cursor`/`set_cursor`, so `j`, `k`, `gg` and `G` work in it without a line of
  motion code — which is the payment for views being a value rather than a special case.

  **A row with no recording says so.** A file nothing has identified is ordinary (§1) and has no performance to
  describe; an empty page would read as "nothing is known", which is a different claim from "there is nothing here to
  know it about".

- **The info panel is a browser: `enter` follows, `^o` comes back** (#127). The graph is traversed rather than depicted
  (`info-panel.md` §1), and traversal needs somewhere to have come from.

  **`^o` rather than `h`, because this is a jump list and not a tree.** An artist can be arrived at from a recording,
  from a release group or from a concept, and there is no parent to climb to — `h` would promise one. vim already spells
  that distinction and borrowing the spelling is cheaper than inventing one.

  **The breadcrumb is the stack, not a rendering of one.** If the two could disagree, one of them is lying about where
  the reader is. Elided in the middle past four steps, keeping the first and the last: where you started and where you
  are are the two that matter.

  **A cycle is ordinary and is not prevented.** A recording leads to a genre which leads back to the recording; there is
  nothing wrong with that path and nothing stops it. What is bounded is the path's _length_ — 32, oldest forgotten, as
  vim's jumplist does — and that is a memory bound rather than cycle detection. Confusing the two would refuse a walk
  somebody meant to take.

  `enter` on a header folds instead of following, and a fold is keyed by section title, so hiding the parts you never
  read holds as you walk rather than resetting on every page. A row with nowhere to go says so: silence would read as a
  broken link rather than as a fact.

- **The fact log, filtered to one thing — and the filter has no per-variant match** (#128). A claim's basis says who; a
  fact says when and on which machine. Whether a record names a focus is answered against **the fact's own wire form**:
  it mentions the id or it does not. That is true of every variant there is and every variant there will be, which is
  #106's rule for a stale reader applied to a reader of the log — and it is why there is no list here to fall out of
  step. Ids inside a fact are resolved to names the same way: a lookup, not a match.

  **What is kept is what was recorded, not what a scan re-derives.** Ingesting 6,500 files mints some 160,000 facts
  every launch with this launch's stamps; showing those beside the handful somebody wrote would drown the interesting
  ones in noise that says only "the disk was walked".

  **Withdrawals are recognised by their names, and that is a stated limit.** They spell themselves three ways —
  `Retracted`, `Removed`, `Dropped` — and a fourth would go unmarked. The cost is cosmetic: an unmarked withdrawal still
  shows, under its own name, which reads as one to a person. A hand-kept list of variants would go stale in the same
  place and fail worse.

  **Building a hidden section cost 8% of a core.** The panel redraws every 200ms whether or not anything moved, and the
  page was rebuilt each time — including the fact log, which is folded away by default. Three fixes, each measured: the
  breadcrumb asked `about` for every step when it wanted only a name (17ms → 8.8ms), the history became something the
  caller asks for rather than something always built (→ 0.16ms folded), and the page is cached on `(focus, history)` and
  dropped in `refresh_library` (→ 1.2ms open). Safe because the page is a pure function of those two and the library,
  and the library changes only on a refold — which is the door every other projection already goes through (#84).

- **`:info` names a thing in the grammar the line already has** (#129). `in=` and a facet key are what a view term
  spells, so this reuses `source` rather than growing a second dialect to keep in step. The shape settles at the
  boundary and the _name_ is resolved later, which is §12's rule split at the right place: which field was written is a
  fact about the text, and which artist it means is a fact about the library.

  **Exact first, then containment.** An id is minted from the exact credited string, so `artist="Stephan Bodzin"` lands
  without a search — but nobody types that, and `artist="Bodzin"` has to work.

  **It does not choose between several.** Four artists contain "Bodzin" on this library and it names them rather than
  picking; that is #119's rule with the same shape of answer. The issue wanted the picker offered here, and the picker
  cannot do it yet: it has no artist source, and its `Chosen` has no focus variant. Saying which is the affordance for
  now, and it is what a shell does for an ambiguous completion.

  **`Chosen` and `Focus` should not be one type**, which the issue asked to check. `Chosen::Saved` is a _definition to
  apply_, not a thing to describe — #96 already ruled that a saved view is edited by editing what it names — so merging
  would force `Focus` to grow a variant with no page. They answer different questions: what to do with what you picked,
  against what to say about a thing.

- **The picker offers the choice a name could not settle** (#130). `:info artist="Bodzin"` matches four things here and
  will not choose; choosing between things is what a picker _is_, so it opens one over them.

  **`Chosen::Look(Focus)` rather than making the two types one.** #129 checked and said no: `Chosen::Saved` is a
  definition to _apply_ and has no page, so a merge would force `Focus` to grow a variant nothing could describe. The
  picker learns to hand back a focus without becoming one — which is the smaller change and the truer one.

  **A handed-over set is ranked, never re-sourced.** `Source::Among` holds its candidates rather than going and getting
  them, because `find` already has them: a second search could disagree with the first, and then the list you were
  offered would not be the list that was found. `Found::Several` carries the focus beside each name for the same reason.

  **And it has no predicate half.** The picker's split — a filter that narrows, a needle that ranks — is about _rows_,
  and these are not rows. Reading part of the input as a filter would silently drop it, so for this source the whole
  line is the name.

- **`Enter` accepts the match, and the tree paints them.** Two search bugs, both of which the code already claimed not
  to have.

  `incsearch` moves the cursor to the match while you type, and `Enter` then ran the search _again_ from where it had
  landed — so `/bur` previewed the first Burial and `Enter` jumped to the second. The comment at that line already read
  "the preview is over; where it landed is where we stay". It records the search and keeps the cursor now, which is what
  vim does and what the comment said.

  And `list::highlighted` says of itself that it is one helper "so the tree and the flat lists highlight" — the tree was
  the one that never called it, so `/` in the library view moved the cursor and painted nothing. The guide had been
  promising "every match is painted where it matched" for as long as it has existed.

  **A match under the cursor is not painted, and that is not a third bug.** ratatui's `highlight_style` paints the
  selected row end to end and wins over any span style, which the helper's doc has always said. A test whose needle
  matched only the row the search had just moved to therefore proved nothing — worth knowing before writing the next
  one.

- **`/` searches the whole library and opens the way to what it finds** (#131). It searched the _visible_ rows, because
  `Tree::rows` is the drawn list and `search_rows` used it — so `/malibu` with the artists shut reported no match, and
  you had to have already opened the thing you were looking for. A closed fold still holds its rows, exactly as a closed
  fold in vim still holds its lines and `foldopen+=search` opens them.

  The comment above that arm already said `/` searches "the lot — which is what a single buffer should do", and the
  guide had been promising it for as long as it existed.

  **Revealing is one operation whatever the levels are.** The tree is three deep whether it groups by `[artist, album]`
  or `[genre, album]` (#97), so opening the way to a row is the same two folds either way — which is the payment for
  levels being a value.

  **Abandoning closes what the search opened.** `<Esc>` already put the cursor back; "the search never happened" has to
  include the folds, or `/` becomes a way to unfold the library by accident.

  **And it needed a cache, measured rather than assumed.** Searching 190 visible rows was free; searching 7,178 and
  folding 6,485 haystacks on every keystroke of `incsearch` was **8.6ms a letter**. The rows and their haystacks are a
  pure function of the shelf — `all_rows` ignores the folds by definition — so they are built once and dropped in
  `refresh_library`, the door every other projection goes through. Lent rather than cloned out, because copying 7,178
  rows to read them once put half the cost back: 8.6ms → 3.1ms → **1.5ms**.

- **Which searches a library wants, decided** (#131). Five questions look like searching and only some of them are:

  | question                   | how it is asked                                 |
  | -------------------------- | ----------------------------------------------- |
  | find it, wherever it is    | `/`, over the whole view, opening the way to it |
  | find anything and go there | `<leader>ff`, fuzzy and ranked (#54)            |
  | show me only these         | `:view`, `:filter` — membership, not a cursor   |
  | find a thing to read about | `:info <name>` (#129)                           |

  **"Search only what I can see" gets no key, and the reason is that `/` already searches a _view_ rather than the
  library.** `ui.library` is the composed shelf, so `:view in="Dance"` then `/burial` searches the crate — narrowing
  first is how you scope a search, and the query language says it better than a second key could. What is left over is
  "search only the folds I happen to have opened", and fold state is a browsing convenience rather than a statement
  about what you care about. Nobody means it as a scope.

  **What `/` looks at depends on where the words are.** A flat list searches artist, album and title, because a row
  there is the only place they appear. The tree searches a track by its **title alone**: artist and album have rows of
  their own, so searching them again made `/malibu` match the album _and_ each of its ten tracks, and `n` walk nine rows
  that matched for a reason nothing on them showed.

  That also settles the painting question this raised: in the tree every haystack is now drawn text, so a match always
  paints, and the "matched but invisible" case is gone rather than merely handled.

  **`/` works in the info panel now.** A fact log is exactly the sort of thing to search, and the panel already had rows
  and a cursor. The guides are prose and scroll, so `/` there would want to search _within_ a document — a different
  motion, and not this one.

- **2026-09-06** **Disposability is a fact, not a format or a folder (#132).** `EncodingDerived { from, to }` says these
  bytes were made out of those. It is deliberately not `RecordingDerived`, which is a musical act — a remix is a
  different performance, and a transcode is the same one encoded again.

  **The question it answers is what has to be backed up.** A derived copy can be made again from the thing it came from;
  an acquired one cannot, whatever its format. Lossless-versus-lossy does not answer that and the spec that proposed it
  was wrong to try: a purchased 320 kbps MP3 is a master, and this library holds lossless files that were upsampled from
  lossy ones — which is why `Verdict::Suspect` exists at all. Only provenance answers it, so provenance is recorded.

  **`from` must be an encoding this library holds**, checked when the proposal is applied. A file the seller derived
  from a master nobody here has ever seen is not regenerable by anybody here, and an edge claiming otherwise would make
  a file that needs keeping look disposable — the one direction in which being wrong loses music. **No edge means "not
  known to be regenerable"**, which is the safe default and the answer an unimported history gives.

  Nothing records _how_. What came out is already described — its format, its bitrate, its length — and the recipe
  belongs to whatever policy makes derived copies rather than to the claim that one was made.

  The encoding page says `keep  yes — nothing here can remake it` or `no — it can be made again`, and a copy in a
  recording's list is marked `· derived`. That is the whole user-facing surface: the storage layer #132 designs is what
  will act on it.

- **2026-09-06** **The scope-creep doctrine is withdrawn; §1 is a list of decisions, not a wall.** The old rule — "scope
  creep is the named failure mode, anything on the out-of-scope list stays out even when a crate makes it trivial" — was
  costing more than it saved. It turned "does this belong here" into a reflex, and the questions it was suppressing
  turned out to be the substance of the project: what a claim is, what a copy is, what is derived from what.

  **This is a library manager as much as a player and its scope is explorative.** Named in as directions: musical key
  beside `bpm`; harmonic and tempo-aware recommendation; embeddings and audio fingerprinting for suggestion, held by the
  library model rather than bolted beside it.

  What survives is what was ever load-bearing: **local first**, and **every dependency recorded here with a rationale**.
  The second is the mechanism that actually kept the tree small — the slogan was taking credit for it. The no-async rule
  stays, and is unaffected, because it is a code-style invariant rather than a feature decision.

  §1's "no recommendations from anyone else's server" is untouched. The objection was to a _service_ deciding what you
  hear, never to your own library being able to answer the question from files you hold.

- **2026-09-06** **A release is one primary type and a set of secondary ones (#133).** `ReleaseType` had `Album`,
  `Single`, `Ep`, `Compilation`, `Live`, `Other` and no way to be written — every release group in the library claimed
  to be an album, and `is_compilation` was `dead_code` because nothing could ever be one.

  **The flat enum was wrong, and the tags say so.** Reading all 6,500 files: 1,084 carry `album/compilation`, 231
  `album/live`, and 28 carry three at once — `album/compilation/live`, `album/compilation/dj-mix`. A single enum has to
  pick one of those three and throw the other two away. So `ReleaseType` is now the primary type alone
  (`Album`/`Single`/`Ep`/`Broadcast`/`Other`) and `ReleaseSecondary` is a `BTreeSet` beside it, which is exactly how
  MusicBrainz models it and exactly what Picard writes.

  A **`BTreeSet` rather than a `Vec`**, because two devices reading the same tag must write the same record: ordered, it
  converges without comparing.

  **A word this build does not know is dropped rather than guessed at.** `Other` inside a set says nothing a query can
  act on, and the file still carries the word for a build that learns it. Nothing in this library hit that path — all
  6,500 files parsed to known types.

  **The field is `release_type`, not `kind`**, which `derived[kind=remix]` already spends on the derivation kind. Two
  meanings for one word in two scopes is how a reader stops trusting either. It matches the primary type _or_ any
  secondary, because somebody asking for their compilations means an `album/compilation` too — a filter reading only the
  primary would answer "album" for all 1,084 and find none of them.

  **`Album` stays the default and is a default, not a claim.** Almost everything is an album, so it is the cheapest
  wrong answer there is; `release_type="album"` therefore means "an album, or nothing said".

- **2026-09-06** **A level that groups one thing is not drawn (#135).** An album row that opens onto exactly one track
  is a keystroke that reveals nothing. The track takes its place, at its indent, with no fold — because there is nothing
  left to open.

  **Always on, and the libraries settle it.** 3 of 495 albums here hold one track; on the dance collection it is 466 of
  490, where the tree was 434 artists, 367 of them one track deep, three levels of navigation to reach one file. A
  setting would have to be evaluated by somebody who had already seen both, and the well-formed case barely moves.

  **The collapsed row carries the album name when the name says something.** Holding one track of _London Zoo_ is not
  the same as holding a single and the row has to say which; a release named after its own lead track — `Fake I.D`
  holding `Fake I.D (Original Mix)` — does not, because the title already begins with it. That prefix test is a
  _display_ question, about whether text is already on screen, and not a fact standing in for a fact.

  **One answer, asked by the drawing and by the search alike.** #131 established that a haystack is drawn text, so a
  match always paints; a collapsed album whose name was searchable but invisible would put that straight back. Both call
  `stands_for`.

  The open questions from the issue, answered: a collapsed level has **no fold**, so `zR` does not count it as something
  still shut; its **seat is the artist**, so a cursor falling out of a fold lands there; and the motions walk it like
  any other row — `deep_fixture` now holds two collapsed rows on purpose so `][` meets one.

- **2026-09-06** **A partial album needs a denominator, and there are two of them (#134).** The library knew every track
  it held and had no idea whether that was all of them: `Medium` carried `position` and `format` and no count, so
  "records I only have part of" was not a question the filter language lacked a verb for — there was no number to
  compare against.

  **Two counts, because one is not enough.** `Medium::tracks` is the obvious half. The half a naive version misses is
  `Release::media`: a two-disc set whose second disc has never been seen has **no medium row at all**, so counting held
  tracks against the discs _present_ answers "all of them" for half a record. On this library that is _All Eyez on Me_ —
  19 tracks held, complete against what it can see, partial against what the pressing says it is.

  **`Held` is three answers and the third is the point.** `Unknown` when either count is missing, for the whole release
  rather than the quiet disc, because a release is complete only if all of it is. 440 of 6,485 files here sit on records
  nothing counted; 468 on records held in part; 5,577 complete. Reading unknown as complete is the one wrong answer
  worth engineering against, since it is the one that stops you noticing what is missing.

  **Read from the tags, both shapes.** ID3 writes `3/12` in one frame and Vorbis writes the halves apart, so `total`
  takes the second half of whichever carries a pair. A `0` from a tagger that fills every field is nobody saying, not a
  claim that the record is empty.

  Asked as `release_held="partial"`. `Observed` authorship rather than `Correctable`: it is counted, not claimed, and
  the totals behind it are what a person would fix.

  **What it does not do is display.** An album row reading `(3 of 12)` is the obvious payoff and a separate design
  question — whether partial albums sort, fold or draw differently is its own issue, now that they are countable.

- **2026-09-06** **A record held in part says so, and the counting moved out of the row (#137).**

  **The measurement came first and it found a regression I had just written.** #134 asked `held_of` per drawn row, and
  `held_of` scanned the spine — so the fold went from **27 ms to 124 ms** the moment the bench fixture carried counts to
  scan. It had not shown up because the fixture had no totals, which is the same blindness as a bench that never runs
  the path. The fixtures now carry totals, because 88% of the real library does. Indexed per release in one pass at fold
  time, `from_catalogue` is **20 ms** — under where it started — and `held_of` is a lookup. Third time this shape has
  bitten: #121, #123, now this.

  **Drawn only where it is short.** `12 of 12` would be on 5,577 of 6,485 files, and a number that is always there stops
  being read. An uncounted record draws nothing rather than `?`: 440 files sit on those, and 7% of rows carrying a mark
  for "no answer" is a worse trade than the silence.

  **A tree node is a release group; the count is about a pressing.** Copies off two editions have no single answer — the
  original and the reissue are different lengths — so a group spanning pressings is marked mixed and says nothing.
  Printing one pressing's total beside a count of copies from both can read as more tracks held than the record has.

  **#135's collapsed row keeps the count.** One track of twelve is precisely the row worth flagging, and it is the row
  whose album header was taken away — so `stands_for` carries `London Zoo  1 of 12` where it used to carry the name
  alone. What is drawn stays what is searched.

  Not done here, and deliberately: whether a _release group_ is complete across its pressings. `Held` is answered per
  pressing on purpose (#134), and "I have the album but not the deluxe edition's bonus disc" is a different question.

- **2026-09-06** **A key is a claim on an axis, and it is stored as a position on the circle of fifths (#136).**

  **Claim, not field**, and the exclusion in `facet.rs` — "a measurement stays a field" — does not catch it. That rule
  is about tempo and bitrate: continuous, answering to `<` and `>`. A key is one of twenty-four names and answers to
  neither. What decides it is that **key detectors disagree**, which is the defining property of a claim here and the
  same reason genre is one. As a field it would be last-write-wins and a rescan would silently overwrite a correction,
  which is the failure `library-is-the-truth` exists to name.

  It cost one line in `facet::ALL`, exactly as that list promises, and `key="Am"`, `!key?` and the info panel's
  who-said-it all came with it.

  **Stored as a wheel position, not a note name.** `Key` is a number 1–12 and a mode — the Camelot coordinate — because
  that turns the question it exists for into arithmetic: compatible keys are the same number in the other mode and one
  step either way, so `compatible()` is `n ± 1` rather than a table nobody can check. The circle of fifths stays a
  **function**; writing it as hierarchy edges would fill a store built for contestable claims with rows nobody could
  ever retract.

  **Three notations, one value, parsed at the boundary.** The tags carry musical (`Cmin`), Camelot (`8A`) and open key
  (`4m`) — one file's `TKEY` says `4m` while its own comment says `F#m`. All 39 distinct spellings across the 592-file
  dance collection parse, and the enharmonics agree, so `A#m` and `Bbm` cannot become two concepts for one key. What
  reaches the log is the conventional spelling.

  **Musical notation on screen.** Camelot is `camelot()` away and open key `open_key()`; a setting to prefer one can
  arrive with the harmonic surface that wants it, rather than as a switch nothing yet reads.

  Detection is not here, and that is why `key` is optional rather than required. Noted alongside: `Recording::bpm` has
  existed since the spine was written and nothing has ever set it — the same unwired state `ReleaseType` was in before
  #133.

- **2026-09-06** **`import` exists, and a scan can offer instead of assuming (#62).** The mutation API asked for
  add/remove/move/replace. `library-api.md` §2.1 had already dissolved three of them by working the cases — a move
  cannot lose anything because identity is the audio, a removal is not a holdings question, and a relink is `apply`.
  What was left was `add`, which is `import`, and it is now a command.

  `populate::reconcile` is the scan §3 describes: hash the audio, hold what the library knows, **report what it does
  not**, and read no tags at all. Tested by the property that matters — retagging a file behind the library's back
  changes nothing, where the old pass would have taken the new title as the truth.

  **Library operations now run before the lock**, like the remote client. `gmuse shelf import` was refused while a
  player was running, which is exactly when somebody would want it: it opens no device and writes only the log.

  **The inversion is not finished, and the log says why.** This library's log holds 15,633 records in 11 fact kinds —
  `RecordingAcquired`, plays, ratings, identifications, merges, the genre hierarchy — and **no `EncodingCatalogued`, no
  `ArtistNamed`, no `ConceptAsserted`**. The catalogue is re-derived from tags at every launch by `bridge::librarian`,
  which ingests the whole scan into a scratch instance. So the log holds _decisions_ and the tags still hold
  _description_, and §2's "a file's tags change → nothing" is true of `reconcile` and not yet true of a launch. `import`
  is what makes finishing it possible: it is the operation that puts description into the log. Filed as its own issue,
  because it changes what the player shows on the first launch after it lands.

- **2026-09-06** **The log is the catalogue, and a launch reads no tags (#140).** `library-api.md` §2 has said since it
  was written that a file whose _tags_ change causes gmuse to do nothing. That was true of no launch:
  `bridge::librarian` ingested the whole scan into a scratch instance, so a retag outside gmuse became the library's
  answer at the next scan. Now it merges the log and asserts holdings, and nothing else.

  **The evidence for how far off it was.** Before this, the log held 15,633 records in **11 fact kinds** — acquisitions,
  plays, ratings, identifications, merges, the genre hierarchy — and no `EncodingCatalogued`, no `ArtistNamed`, no
  `ConceptAsserted`. Every genre in the library came from a tag, re-read at every launch. The log held _decisions_; the
  tags held _description_.

  **The migration is one import and it was measured.** 6,500 files, 6,047 confirmed, **164,328 facts** in 37 s, taking
  the log to 179,962 records. The librarian then folds in **0.1 s** — the fold was never the cost; re-deriving was.

  **A fixture now imports rather than merely holding.** `fixture::from_holdings` means "these files are in the library",
  which is a declaration, so it makes one. That is the whole reason the change touched a hundred tests and none of their
  assertions.

  **The launch offers what it does not know**, counted in **encodings and not recordings** — two copies of one track are
  two files and one recording, and subtracting the second from the first reported 175 strangers on a library that had
  none.

  What this does not yet do: `scan::scan` still reads tags into `Holdings::seen`, because that is where the audio hash
  is taken. Nothing reads those tags any more, so the cost is a cold scan's and not the library's, and
  `populate::reconcile` (#62) is the shape that stops paying it.

- **2026-09-06** **A holding is its audio id, and the tags cannot be reached from one (#141).** `Held` carried a whole
  `Scanned` — title, artist, album, genres, key, every tag the boundary reads. After #140 the only field anything read
  was `seen.audio`; the rest was read off disk for every changed file and discarded.

  **The reason to narrow it was not the 1.7 seconds.** Measured over 6,500 files: hashing alone 16.5 s, hashing plus
  tags 18.3 s, and a full rescan went 6.81 s → 6.09 s. The reason is that "tags are read once, at `import`" was enforced
  by nothing but the absence of a caller — the tags sat on every holding at every launch, so the next thing wanting an
  artist name in the scan layer would have found one, and #140's retag-is-invisible property would have quietly stopped
  being true. It is enforced by the type now.

  **The tags moved to `Holdings`, not to a `cfg(test)` field on `Held`.** The first attempt put them on `Held` behind
  `#[cfg(test)]`, which immediately asked _production_ to fill a field it no longer has. A synthetic fixture still has
  to carry what it means by "this file has these tags", so `Holdings` keeps them — in a test build, `#[serde(skip)]`,
  written only by `take` — and a fixture reaches the library the way a real file does, through `import`.

  **What a scan can still promise.** `a_file_whose_tags_will_not_parse_is_still_held` used to assert such a file got a
  title. It cannot now, and the promise it makes instead is the one a holding is _for_: the audio read, so the file has
  an identity and a rescan can skip it. Ingest being total is asserted where ingest happens.

  `scan::Report::identified` is gone with it. It counted files carrying a MusicBrainz id by reading `seen.identity`, and
  a scan that reads no tags cannot answer it — nor should it, since the log knows what has been identified and
  `shelf query --unidentified` is where to ask.

- **2026-09-06** **The content-addressed store is withdrawn; one readable tree instead (#132).** `storage-layout.md`
  specified a `Store/` keyed by encoding id with a hardlinked `Browse/` over it. Asked what that bought over copying two
  folders, the answer did not survive the question.

  **The store was protection against rename churn** — rendered paths move files whose bytes never changed, and #132
  called that "exactly the event #62 exists because we cannot reliably infer". **It is inferred now.** Identity is the
  audio hash (#35), the scan matches by hash and the locator follows (#141), history hangs off the recording (#81); a
  file renamed _and_ moved keeps its row, its rating and its plays, and there is a test named for it.

  So the store defended against an event that had become free, and charged an unreadable tree, a second tree to
  compensate, link machinery, and a NAS copy that is either unreadable or needs a third mechanism. That the design
  survived #141 by a day is the lesson: a decision outlives the constraint that produced it unless something asks.

  **What it did buy is now a render-time check** — Unicode normalisation, case-insensitive collisions, reserved names,
  the 255-byte cap. Those were `Browse/`'s problem, where being wrong was free; they are the real tree's problem now,
  and the renderer verifies before it writes and reports rather than mangling.

  **The NAS copy is `rsync`**, which is what prompted the question: 42 GB of audio and 46 MB of log, readable from any
  machine, because it is the same tree. The log is copied rather than hosted — local-first means an unmounted NAS is a
  degraded library, not an empty one.

  Non-audio assets stop being a problem with it: art and rip logs sit beside the album, where every other tool looks.

- **2026-09-06** **The storage tree may use structural facts only (#132).** Asked to design the _desired_ layout rather
  than describe the current one, the rule falls out of #26: the folder tree was doing three jobs and is qualified for
  one. Storage may use **who made it and what release it is on**; never genre, era, catalog, tempo or how a file was
  acquired. A tree has one dimension and description has many — a file is Jazz _and_ Christmas — so a folder must pick
  one and lie about the rest.

  That deletes every root on disk: `Classical/`, `Jazz/`, `Christmas/`, `Experimental/`, the era and BPM-band
  directories, `Singles/`, **and the `Dance`/`General` catalogs**, which are curated collections a recording may be in
  several of and which are already 6,311 facts in the log. Keeping them was the inconsistency in the previous draft:
  rejecting `Classical/` as a taxonomy in a directory and then keeping one.

  **Both hard cases dissolve.** `Music/Classical/` holds five naming conventions in twelve folders — composer, surname,
  composer-and-performer, ensemble-and-conductor, a compilation title — and under this rule the library answers "who is
  this credited to" once and all five render alike. A one-track dance release gets an album folder like anything else,
  and #135 already collapses those in the tree.

  **Rendering the real files is what corrected the design.** Some NAS files carry only ID3v1, whose fields are 30 bytes:
  `Acoustica: Alarm Will Sound Performs Aphex Twin` is `Acoustica: Alarm Will Sound Pe` in the tag while the _folder_
  holds the full name. A renderer reading tags would replace good names with stumps. Beatport files carry chart
  positions in `track`, so `37 Long Chicken` on a two-track release. And stripping trailing dots for Windows turns
  `Manhattan Research, Inc.` into `Inc`.

  So: **consume the path first (#143), identify, and only then render — from the library, never from the tags.** A file
  the renderer is not confident about is reported and left where it is, which is safe precisely because the path is not
  the binding.

- **2026-09-06** **The renderer proposes and refuses; it does not mangle (#132).** `gmuse shelf render` says where each
  file belongs — `{file_under}/{album}/{disc-}{track} {title}.{ext}` — and moves nothing. Splitting proposal from
  movement is the whole safety property, and it is affordable because **the path is not the binding**: a file in the
  wrong place costs nothing, so refusing is always available.

  Checked before a name is used: forbidden and control characters, reserved Windows names, a trailing dot or space, the
  255-byte component cap, and a case-insensitive collision — APFS and SMB fold case, so finding that out during a move
  is finding out too late. **A trailing dot is stripped and an internal one kept**, because `Manhattan Research, Inc.`
  is a name and the dot in `D.T.` is meaning.

  **Two copies wanting one path are both refused.** Picking a winner moves one file onto the other's name, which is how
  a file disappears.

  **The first run on the real library found two problems it did not cause.** Fourteen refusals are one album under two
  capitalisations — `Strange Time to Be Alive` against `Strange Time To Be Alive` — which are one directory on APFS and
  two release groups in the fold, because a group id is derived from the raw title. And 400 files across 26 albums would
  file under `Tom Waits and Crystal Gayle`: Tom Waits' whole discography, renamed by a joined credit from _One from the
  Heart_. That is exactly the hazard `Scanned::credit_is_joined` documents, already in the data.

  **NFC normalisation is stated as absent rather than half-done.** macOS hands out decomposed names and Linux does not,
  so the same album can be two directories; doing it needs `unicode-normalization`, and a dependency needs a decision.
  Decided 2026-09-07, below — the dependency was taken.

- **2026-09-07** **The renderer composes on write, and `unicode-normalization` is the dependency that lets it (#151).**
  Measured on the real library, and the split is total: all 48 non-ASCII names are NFC on APFS and NFD on the SMB share.
  Both filesystems are form-_insensitive_ on lookup, so nothing was unreachable — but `readdir` hands back the form the
  filesystem chose, not the one that was written, so the two devices observe byte-different names for one file.

  **What it was not is worth recording.** Minting is untouched: `Basis::seed` seeds from an MBID, an external key or an
  `AudioId`, never a name, so a recording mints the same id whatever the share spells. The damage was confined to paths
  — `plan` compared them as raw bytes, so a file already correctly placed under a decomposed name was proposed for a
  move on every run, a rename onto itself forever, and two names differing only in form passed the collision check and
  then landed in one directory.

  **The dependency, against this project's own hand-rolling precedent.** `text.rs` rejected `unicode-normalization` for
  search folding and that decision stands — it was measured against a tag cache holding `ö ü í ´ ’ ＂ ⧸ ？` and nothing
  else, and `flatten` strips marks entirely, which turns `Björk` into `Bjork` and is right for search and catastrophic
  for a path. Rendering is a different question: correct NFC needs the canonical composition table, the library will
  meet names outside any table small enough to hand-write, and being 95% right here produces exactly the silent
  directory-splitting the change exists to prevent. So the table is bought rather than approximated.

  **On-disk names are left alone.** The renderer proposes NFC and the ordinary move carries it; nothing is rewritten to
  chase a form. The share will decompose again on write, which costs nothing — it is form-insensitive, and §8 already
  holds that the NAS is a copy and not a source of truth.

- **2026-09-06** **A joined album credit does not rename the artist it starts with (#145).** `shelf render` proposed to
  file 400 files across 26 albums under `Tom Waits and Crystal Gayle` — his whole discography.

  The guard existed and could not help. `credit_is_joined` is read from the **track** artist id, and on _One from the
  Heart_ the track artist is Crystal Gayle alone with no slash. The album artist id is `c3aeb863…/cefd9d3d…`, whose
  primary half is Tom Waits, and the album artist _string_ is the joined credit — so the album path wrote the joined
  string as Tom Waits' name, and every release group crediting him read it back.

  So `album_credit_is_joined` is its own field, read from the release-artist id, and the wording goes on the credit as
  `as_credited` where a sleeve's phrasing belongs. The outcome after repair is exactly right: 388 files under
  `Tom Waits`, and _One from the Heart_ under `Tom Waits and Crystal Gayle`, which is the credit rather than the artist.

  **It surfaced only because paths are rendered now.** A wrong `file_under` was a column nobody counted; it is 400 files
  proposing to move into a wrongly-named folder.

  **And repairing it broke something worse, which is the part worth remembering.** `populate` published holdings, and
  `HoldingsAsserted` supersedes per device — so importing one folder replaced the whole library's holdings with those
  400 files and left 6,085 unopenable. Cataloguing is told which files to take in and **nothing about the rest**, so it
  cannot say what the set is; `reconcile` publishes, because only a walk of every root knows. `shelf import` and
  `shelf query` now do both.

- **2026-09-06** **A release group's fallback id normalises, like every other named thing (#147).** `Id::of_name` goes
  through `normalise`; the release-group fallback went through the raw string, so `Strange Time To Be Alive` and
  `Strange Time to Be Alive` minted two albums.

  **The reason is not sync, and the first version of this entry said it was.** An id is a UUIDv5 over `(kind, seed)`;
  nothing is transmitted, every machine recomputes it, and two machines reading one file always agree. Divergence needs
  the _seed_ to differ, and the seed is a tag on a file — so the unit is the **file**, not the machine. This happens on
  one disk, when an album is acquired twice and the two copies are tagged by different tools.

  The justification is simply that **capitalisation is not part of an album's identity**. `Luna` and `luna` are one
  album, and every other named thing already mints through `normalise`.

  **The album is the only identity that free text can split**, which is why it is the only one that needed this: an
  encoding is minted from the audio hash and a recording from an id or the bytes, so neither can be moved by a title.
  The release group falls back to the credit and the title as text when no MusicBrainz id is there.

  **It was not what caused the case that found it.** `shelf render` refused four Early James files, and those are two
  groups because one copy carries MusicBrainz ids and the other does not — the capitalisation was riding along. Hashing
  the folder settled it: 10 of 12 tracks byte-identical and deduped correctly, 2 genuinely different. That duplicate is
  #146; the minting defect was real anyway and is fixed here.

- **2026-09-06** **Import is idempotent, and a contradiction is refused rather than resolved (#148).**

  **Idempotent first, because it was not.** `apply` deduplicates against what the log already says and `import` did not,
  so running it twice recorded every fact again under a new stamp: **111,879 repeats in 207,450 records, 54% of this
  library's log**, written by re-importing a few times in one afternoon. Scoped to what one call added, deliberately — a
  blanket rule would be wrong, because `RecordingPlayed` carries no timestamp and two plays of one track _are_ one fact
  told apart by their stamps. An ingest produces no plays, which is what makes it safe there.

  That is what makes the rest affordable: a file that agrees now writes nothing, so re-importing is the ordinary way to
  take a correction in rather than something to avoid.

  **A contradiction is two files, one set of bytes, two albums.** Both refused, neither imported. Every rule for picking
  a winner — first, newest, best-sourced — is a guess that is wrong sometimes and invisibly so, and what it leaves
  behind is a release group nothing points at. `Catalogue::placed` is one slot per encoding, so the loser's release,
  medium and track are folded and unreachable. This is §7's rule rather than a new one: a library that accepts anything
  and sorts it out later fills with entries nobody will ever fix.

  **Compared within the pass, not against the log, and the first version got that wrong.** A recorded placement
  disagreeing with a file is usually the file being _better_ than it was — it gained a MusicBrainz release-group id, or
  the id rule changed under it. Checking against the log called **all 480 files** of #147's renumbered albums liars. Two
  files in one pass claiming one encoding for two albums cannot be explained that way. Telling them apart _across_
  passes would need the log to record which file a placement came from, and it does not.

  On this library it refuses 14 files: the Early James duplicate, and a Thomas Brinkmann one the renderer could not see
  because those copies share their encodings.

- **2026-09-06** **The library is one readable tree, and the migration is done (#132, #146, #149).**
  `shelf render --write` moved 6,471 files in 19 seconds and pruned 619 folders. 6,500 audio files before and after, 42
  GB before and after, and `shelf import` reporting every file still known — which is the check that counts, because
  identity is the audio and anything the library could not find would have said so.

  **The survey ran first and earned its place** (#143): of 6,500 files, **14 rows held something the tags did not** —
  thirteen Thomas Brinkmann tracks whose filenames name `Variation1`…`Variation13` where the tag says `[untitled]`, and
  one series in a filename. Everything else was the reverse, the tag being the better record and the filename its
  sanitised form. None of the 82 truncation suspects were confirmed by a folder, so the ID3v1 hazard that made the
  ordering mandatory did not bite here — it was measured on the NAS, and that is where it would have.

  **Three things the run found that no test would have.** Cover art did not follow its album, leaving 13 orphaned images
  (#149) — fixed, and only from a folder no audio remains in, or two albums under one directory would have the first
  one's move drag the second's cover away. A scan walked into dotted directories, so `.departed/` would have handed back
  every retired file on the next pass. And `shelf import` with nothing to import returned without flushing, throwing
  away the walk that had just learned where everything moved to.

  **The Early James duplicate is resolved** (#146). Ten of twelve tracks were byte-identical; the two that differed
  differ in the _audio_ — 43,288 bytes, same duration, same bitrate, same size, both measuring clean — so quality could
  not choose and provenance did: the MusicBrainz-tagged copy is the better described. Twelve files retired to
  `.departed/`, which is a directory standing in for a tombstone until #62 writes one.

  What is left in the old tree is Fiona Apple's 444-byte album title, which no filesystem will hold and which wants a
  shorter name in the library rather than a rule.

- **2026-09-06** **A path too long is shortened at a word, not refused (#150).** Refusing was right while nothing
  shortened — a name cut mid-word is one nobody can search for, and this library had one: Fiona Apple's album folder
  ended `…to Batter When`, four words into a sentence.

  But the refusal could not be the final answer, because **the title is not a mistake**. _When the Pawn…_ is 444
  characters of poem, written in reply to a magazine profile, and it held the Guinness record for the longest album
  title. MusicBrainz carries it whole and the library is right to. No filesystem will, so the **path** gives way and the
  `…` says it did.

  Renaming the album was the other option and it is the wrong one: the library would then be wrong about what the record
  is called, to make a directory fit. The path is a rendering; renderings are what bend.

  The cut lands on a space and never mid-character, and the track number and extension come out of the name's budget — a
  title that just fits would otherwise make a filename that does not. That emptied the last of the old tree: `General/`
  and `Dance/` are gone, 191 artist folders remain, and `shelf render` reports **0 refused, 0 blocked**.

  A bonus nobody asked for: the old filenames were truncated too, so the migration restored `04 Love.mp3` to
  `04 Love Ridden.mp3` from what the library knew.

- **2026-09-06** **A dot-prefixed album is an album (#150).** `worth_entering` skipped any directory beginning with a
  dot — the usual convention, added so a walk would not step into `.departed` and hand back everything `remove` had
  retired. It also took sixteen tracks out of this library, because `...Baby One More Time` is a Britney Spears record.

  **How it surfaced is the part worth recording.** The missing sixteen looked like _duplicate paths_ — 6,483 files
  against 6,467 holdings — and were reported as such. Going to remove them is what found the cause. Read the other way,
  sixteen files would have been retired for being copies of nothing.

  The skipped directories are **named** now rather than matched, because a rule that cannot tell a hidden folder from a
  record has no business guessing, and this library holds exactly one record that proves it. `.departed` is the only one
  that matters for correctness; the rest hold no audio and are skipped to save the walk.

- **2026-09-07** **The library holds what a record is, not only what it can play (#155).** A slot with a recording and
  no encoding is how this model has always said _"this track is on the record and I do not have it"_, and nothing had
  ever written one: `import` writes a slot and its copy together, so the distinction existed and was never exercised.
  `Proposal::Tracklisted` writes them, and the row it produces is drawn under its album, at its number, dimmed.

  **What made it safe is that the ids are a function of what a tool sends**, as `Identified` already was. A release
  MusicBrainz id mints the release, the release and a disc number mint the medium, and a recording id mints the slot —
  so a tracklist lands on the medium `import` already made rather than beside it. Where the files carry no album id
  `import` mints the release from the _group_ instead, the ids disagree, and the proposal is refused as stale. **That
  refusal is the safety property**: a phantom disc catalogued beside the real one would be silent, and this is loud.

  **Two things had to change to keep an old answer true.** `held_index` counted _slots_ to say how much of a pressing is
  here, which was right only while a slot implied a file — cataloguing a tracklist would have made every record read
  `Complete` the moment its length was learned. It counts filled slots now. And `advance` stopped at the first row it
  could not open; it steps past them and says how many, because a record you are three tracks short of should still play
  the fifteen you have.

  The cost the issue named is real and is now paid: **a wrong release invents slots for tracks that are not on your
  edition**, where before it merely mistagged files you own. That is the price of a `close` match accepted by hand, and
  it is why `ingest survey` produces a file a person reads.

- **2026-09-07** **An album is merged at the group, and nowhere else (#156).** A release group's id is a function of
  what the tags said — `mb_release_group` where there is one, artist-and-title where there is not — so learning an
  album's MusicBrainz id _changes its identity_, and a release, a medium and every slot are minted from it in turn. One
  tag edit mints a whole second album and `import` moves the copies to it. `Fact::ReleaseGroupMerged` says the two are
  one, and `shelf duplicates` is how you find them, because nothing else prints a group id.

  **Groups only, and that is the decision the evidence forced.** Of 50 albums held twice here, 48 are one live album
  beside a dead identity and would take any answer. The two that are not settle it in opposite directions: _Barton
  Hollow_ is a standard edition of 14 tracks and a deluxe with 4 more, so the right answer is **one album holding two
  releases** — merging downward would erase the level #134 exists to hold. And `AC/DC — High Voltage` is the 1975
  Australian record and the 1976 international one, sharing a name and a folder: **correctly two**, and a rule that
  merged on title would have folded them. So the command reports and a person decides.

  **The merge exposed a second duplicate underneath, and that is worth recording.** Applying 48 of them made
  `shelf render` want to move 298 files: an album merged out of two entries brings both its credits, and where one was
  tagged with a MusicBrainz artist id and the other was not, **the same person is two ids** — so the line read
  `Anderson .Paak, Anderson .Paak`, and `file_under` is that line. `line_of` deduplicated by artist id, which is the
  same rule one step too early; it dedupes by rendered name now. Two people who really share a name collapse in the
  line, and that is the right trade: printing one name twice is unreadable however it happened, and the credits keep
  both.

  What this does **not** do: the media under a merged group stay separate, so the 384 slots on discs no file came off
  are still in the log. #155 stopped drawing them, and merging releases and media is a further step nothing yet needs.

- **2026-09-07** **A re-import cannot repair a name, and that is why naming is a proposal (#154).** `ArtistNamed` is
  last-writer-wins over one key, so an artist renamed by a joined credit stays renamed: the right name is already in the
  log, `apply`'s replay guard drops it as a duplicate, and `shelf import` on the affected folders reports
  `0 new fact(s)` and changes nothing. **Only a new assertion can outrank the wrong one**, and until now nothing could
  make one — `Instance::rename_artist` wrote the right fact and had no caller.

  So the guard learned the rule the paired facts already taught it: **"already in the log" is not "still true"**. A name
  is judged against what the fold currently says, not against the history, exactly as a hierarchy edge is. Saying a name
  that is already the answer still writes nothing.

  **The report offers a repair only for the exact signature of the fault** — an earlier name, followed by a joiner,
  followed by somebody else. The looser rule tried first, "offer whichever earlier name carries no joiner", was wrong on
  half of this library: `MGMT` against `The Management`, `Bush` against `Bush X`, `Antigone` against `ANTIGONE`,
  `Sidney Carter` against `Mrs. Sidney Carter` are four artists with two real names, where which to use is a decision
  and not a defect. Narrowed to the prefix it offers exactly the four that are the bug.

  Applied here: `Bob Dylan`, `Dolly Parton`, `Flume` and `Hobart Smith` got their names back, 276 files moved out of
  folders named after a credit, and the library reached **6,648 settled, 0 would move** for the first time.

- **2026-09-08** **A path is evidence on the weakest rung, and never the answer (#143).** `paths survey` had produced
  394 findings that nothing consumed. Asking why turned up the reason it has no `apply` column: **every one of them is
  on the NAS and not one is a file the library holds.** It is a pre-import report, and the decisions it feeds are
  tagging decisions made before `shelf import`, not facts applied after.

  `Source::Path` sits **below `TagVariant`**, the weakest rung there is. A path claim is recorded, queryable, and while
  any tag says anything it does not render. The tempting alternative — put it above, because `storage-layout.md` §4
  found the path is _better_ evidence on the ID3v1 files — is wrong for the same file set: those DJ-pool names carry a
  chart position where a track number goes. A ladder cannot encode "better on these files and worse on those", so where
  the path really is the better record it is settled by MusicBrainz or by a person, not by a rung.

  **Only the 144 mastering markers were easy, and they were easy because nothing contests them**: the path knows
  something the tags never claimed. The other 250 are disagreements, and they wait — which is the rule, not a shortfall.
  A first pass takes what needs no adjudication and leaves what does.

  **It had to be read at ingest, not proposed after.** `shelf render` names a placed file from the library, so `_pn` is
  destroyed by import: 147 marked files on the NAS and in staging, **0 in the library**. A proposal would have had
  nothing to propose against. And the branch that had to be fixed was the one for files whose tags will not parse —
  exactly the files whose name is the only evidence there is.

- **2026-09-08** **An archivist, not a completionist (#162).** A derivation names both ends, and the far end is
  routinely something the library does not hold: you own the remix, not the original. The question that settles what to
  do about it is whether an unresolved reference is _a gap to fill_ or _a fact about the world_, and it is the second.
  Recording that a remix was made out of an original is the archival act; treating it as a work list would turn every
  relationship into a shopping list.

  **It is not the same absence as #155's greyed row**, and keeping them apart is what makes both coherent. A track on a
  record you own part of is _missing_ — you hold the album and the row is a gap in it. A recording something of yours
  merely _refers to_ is not missing at all.

  What makes this cost nothing: `draw` builds rows from `RecordingCatalogued`, so a recording named only by
  `RecordingIdentified` has no title and **is never drawn**. The reference is traversable, nameable in the info panel
  and invisible in the browse, by construction rather than by a rule. And `Id::of_basis` is convergent, so the id a
  reference was minted with is the id an acquisition would mint — the edge resolves itself if the thing ever arrives.

  `Ref<T>` said this all along — _"a reference is valid when its target is absent; resolution is an optimisation, not a
  precondition"_ — and the first `RecordingDerived` arm refused on exactly that. The refusal is gone.

- **2026-09-12** **A commit that does not parse is listed under `Other`, not dropped (!384).** `cliff.toml` set
  `filter_unconventional = true`, so a badly typed subject did not fail loudly — it vanished from `CHANGELOG.md` without
  a word. Measured: **44 of 383 commits** were invisible, dated 2026-09-03 to 09-07 and interleaved with well-formed
  ones, so this was a live leak and not old pre-convention history. One of them is `8b84fd3`, which CLAUDE.md already
  names as an accepted cost.

  Every one of them is merged onto a protected `main`, where a subject cannot be corrected — rewriting one is a
  force-push by definition. So the 44 could only be surfaced or left invisible. They are surfaced.

  **This is not the preprocessor that was rejected, and that rejection still stands.** A preprocessor rewrites one
  _named_ subject so it pretends to parse; the catch-all names nothing and claims nothing, it declines to throw away a
  commit it cannot type. The 44 stay untyped in the changelog, which is accurate, because they _were_ untyped. What
  changed is that the failure is visible instead of silent. The parser sits last in the list, so it can never shadow a
  typed rule above it: `Other` holds exactly the 43 unconventional non-initial commits and no well-formed one was
  misrouted into it. The forty-fourth is `10ef69d`, `feat/a row carries its genres`, which reaches `Features` because
  `^feat` matches a slash as happily as a colon.

  **The cause was a missing gate, and that is the part worth keeping.** `pre-commit` ran `just fmt-check` and `pre-push`
  ran `just ci`; nothing had ever looked at a subject line, which is the whole reason 44 got through. A bucket without a
  gate only licenses more of them. `.githooks/commit-msg` is that check, at the one moment a subject is still free to
  fix — run over all 383 historical subjects it rejects exactly the 45 unconventional ones and accepts all 338
  well-formed ones, with no disagreement in either direction.

  **And the merge-skip rule never worked.** It matched `^\w+(\(.*\))?!?: merge `, hunting merge commits by typed
  subject. This project is fast-forward only and has **no merge commits at all** — zero in 383 — so the rule could never
  do its job, and its one hit in all of history was `feat(model): merge by redirect` (#68), a real feature it silently
  dropped. It now matches what git actually writes for one.

  Net: **every commit now appears except the initial one**, against 338 of 383 before. That one stays out by name rather
  than as a side effect of a filter that was throwing away 44 others with it.

- **2026-09-12** **cmus is the origin and no longer the authority, and the transport spec is what made that explicit.**
  This brief was titled "a cmus-shaped music player" and the README called itself a reimplementation of cmus's
  interaction model. That was true when the question was what a terminal player should feel like, and it has quietly
  stopped being true in three directions: the library is an append-only fact log rather than a tag reader (#81, #140),
  the player is one view onto an engine other clients drive (`gmuse-engine-architecture.md`), and the transport
  vocabulary is now derived from DJ software rather than from cmus's bottom row.

  `transport-keymap-spec.md` is where that last one becomes undeniable. It rebinds `v` — cmus's stop — to next-track, on
  the grounds that stop is not a verb at all once cue exists (D-10, D-14), and it says in as many words that the known
  cost is a cmus user pressing `v` and getting something else. A document that is willing to break cmus muscle memory on
  derived grounds is not fitting a shape; it is using a reference.

  So the framing changes and the reference does not. "cmus does it this way" remains evidence — it is in the prior-art
  table beside Mixxx and Serato — and it stops being a reason on its own. Nothing about the existing bindings changes as
  a consequence of this entry; what changes is that changing one no longer needs to argue with the title.

  Recorded here because §1 is the list of decisions and this revises how the whole of it is read, which is exactly the
  kind of change CLAUDE.md says must not happen on a whim.

- **2026-09-12** **A deck is selected on the wire and resolved at the boundary; the player is a deck plus a queue**
  (#224, `transport-keymap-spec.md` O-4, O-7, O-8).

  **`DeckSel` goes in and `DeckId` comes out.** The wire and the keymap accept `Focused | Other | Id | All`; the engine
  resolves it against control-state focus at the parse boundary, and nothing downstream — control state, the RT
  schedule, the performance log — ever sees anything but a concrete `DeckId`. That is §12's "parse at the boundary"
  applied to a selector rather than to a string.

  It reconciles two things that looked opposed. D-5 is right that focus must be engine state: two views each tracking
  their own make "the focused deck" resolve differently per sender, with no good failure mode. §7.5's sketch is right
  that everything downstream carries `DeckId`. They are the same design seen from either side of one resolution point.

  **Two arguments settled it, and neither was in the original spec.** §15 phase 8 exits on "record a mix, then replay
  its performance log to identical audio", and §12.3 logs every _applied_ `PerfEvent` — so a recorded `Focused` replayed
  under different focus produces different audio, and replay determinism forbids an unresolved selector in anything
  persisted. And §5.1 forbids the MIDI input thread from going through the control thread or the socket, so a focus
  living in a view is unreachable from a controller at all: D-5's concern is not only about two TUIs disagreeing.

  Consequences: focus is control state (§6) and needs a `decks` event so views can render it, and `All` resolves to a
  set applied as **one** act rather than a loop — D-7's idempotence requirement is about exactly that.

  **The player is a deck plus a queue, and `audio::State` is not touched yet.** D-9 argues for two running states plus
  `Armed`, and the argument is about a deck. The shipped `audio::State` is about the single player stream, is read by
  the render step every buffer under I2, and carries a meaning a deck has no concept of: "stopped" is the queue not
  advancing. §7.8 already models the two separately — "transport state for the main player _and each deck_".

  So `RunState { Paused, Armed { at }, Running }` is a new deck-side type arriving with quantize in phase 6, and
  `audio::State` changes when something can actually produce an `Armed` — not before. Changing an RT-adjacent enum for a
  state nothing can reach is the opposite of how this project has earned its other changes.

  D-14's unification survives intact: cue is one verb across both because the default cue point is the track start,
  which is a statement about the _verb_, not about the run-state enum.

- **2026-09-12** **A view attaches and playback stops being its own** (#219, I11, I15). `gmuse --attach` takes no audio
  device and no player lock: it loads by recording, learns what is playing from the `playback` topic, and its going away
  is not a stop. Two things had to move for that, and both were guards rather than features.

  **A session restore had to learn not to run.** Restoring loads, seeks and pauses — three commands that, aimed at an
  engine mid-track, destroy exactly what attaching preserves. The guard is `Transport::outlives_this_view`, on the seam
  rather than a flag in `main`, so the next restore path inherits it instead of having to remember. The view's own half
  — the view number, the open playlist, the settings — still comes back, because that is not playback.

  **And `current` had to be followed rather than owned.** An attached view does not choose every track: the engine
  splices the next one (#232) and another view's keypress is just as real. `Transport::playing` answers over a socket
  and is `None` in process, which is not "nothing playing" but "you chose it, you know". The local default being `None`
  is why `outlives_this_view` guards the follow as well — without it, every ordinary launch would have `current` cleared
  on the first tick: audio still playing, status line saying nothing was on, and no test failing.

  **What is not built is the write half, and it is named out loud rather than shipped quietly.** An attached view's
  `Desk` points at the same log the engine's does, so a rating appended here would be two appenders on one file — which
  §4.4 gives to the engine alone. So `record` refuses and says so, with the issue number in the message; the launch scan
  does not run; `:rescan` goes over the wire. Its library is the fold it did at launch and does not follow the engine's
  appends, because nothing subscribes it to `library` yet. #236 is both halves, and it is why `--attach` is opt-in:
  making it the default would trade a working library manager for a working detach.

- **2026-09-12** **`aaa_mode` becomes `Scope`, because the three values are tree depths and not music** (#234).

  `AaaMode` was `All | Artist | Album`, and `next::pool` narrowed the candidate pool with it — through `Shelf::locate`,
  which returns the **outer and inner keys of the grouping tree**, and `artist_tracks(&SortKey)` /
  `album_tracks(&SortKey, &SortKey)`, which take generic sort keys. The tree is grouped by
  `Levels::By { outer: Level, inner: Level }` where `Level { field, at }` is any query field with an optional hierarchy
  depth, and `levels` is user-settable.

  So the name was already wrong, demonstrably: group by `genre,year`, set `aaa_mode=album`, and the player keeps playing
  **within the current year**. That is the mechanism working correctly, and nothing about the setting's name would let
  anybody predict it. The three values are the three depths of a two-level tree — the whole pool, the outer key, the
  inner key — which is what `Scope { All | Outer | Inner }` says.

  **The rename now, the query-defined pool later.** Level-narrowing cannot reach a dimension that is not currently
  grouped — "shuffle within this genre" while grouped by artist and album is inexpressible as a tree lookup — and a pool
  defined by a query can. But the two are not interchangeable: `Scope` is _relative to what is playing_ and a query is
  absolute, so the query form is `album = album_of(current)` re-resolved on every track change. That is strictly more
  machinery than a map lookup into a tree that is already built, so the cheap form stays and the general one arrives
  beside it (#235).

  `:set aaa_mode=…` survives as an alias, because a config file on disk should not stop working over a rename. The wire
  spelling does not: `settings.set_aaa_mode` shipped hours earlier in #232 and nothing depends on it yet, which is the
  one moment renaming a protocol method is free. §7.4's method names are public API precisely so that this is not true
  later.

  This is the second thing the 2026-09-12 cmus entry was written for, arriving the same day: an inherited name on a
  mechanism that outgrew it. The reference stays useful; the vocabulary stops being binding.

## 14. Prior art worth reading

Read these for the audio plumbing. None of them implement cmus's library model — that part is ours.

- **Trix** — <https://github.com/RIZAmohammadkhan/TerminalMusicPlayer> — ratatui + Symphonia, gapless via next-track
  pre-decode, dedicated decode worker with a ~750ms buffer, container-level seeking. Closest to this design; read first.
- **termusic** — <https://github.com/tramhao/termusic> — the most mature Rust local-library player. Note: client/server
  split over gRPC (more machinery than we need) and tui-realm rather than raw ratatui (an abstraction we're avoiding).
  Its backend format-support table is a useful reality check.
- **TuneTUI** — <https://github.com/Sqble/tui_music> — smaller, readable end-to-end.
- **cmus itself** — <https://github.com/cmus/cmus> — the behavioural specification. Consult for "what does it do when…",
  never for structure.
