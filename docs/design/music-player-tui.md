# DESIGN.md — a cmus-shaped music player in Rust

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
- The four cmus views (library tree, sorted library, playlist, queue) plus browser, filters, and settings
- Gapless playback of MP3, Ogg Vorbis, FLAC, WAV, AAC/M4A, ALAC
- Sample-accurate seeking
- cmus's next-track semantics: queue precedence, `aaa_mode`, shuffle, repeat, continue
- The filter mini-language and the display format-string language
- Configurable keybindings via an rc file, `:bind` at runtime
- A remote control socket (`cmus-remote` equivalent) for media keys and status bars

### Out of scope — do not implement, do not add dependencies for

- Album art in any form (no sixel, no ueberzug, no kitty graphics, no `image`)
- Visualizers, spectrum displays
- Lyrics, synced or otherwise
- Podcasts, RSS
- Commercial and scraped streaming services: no Spotify, no YouTube, no yt-dlp
- Tag _editing_ (reading only)
- Any async runtime. No `tokio`, no `async-std`. Threads and channels.

Dropping these is a deliberate scope decision, not an oversight. They are the reason comparable projects carry 40+
dependencies.

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
nucleo-matcher    = "0.3"    # fuzzy search

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

**Membership is `roots` and `where`** (§7), settled 2026-08-30, either of which may be absent and which combine with
**and** when both are given. A catalog _is_ a named, persistent filter, which is what cmus's `fset` already is.

`roots` is **not** desugared into a path expression, which is what this section said before the implementation.
Desugaring would replace `Path::starts_with` — component-wise — with string globbing, and smart-case would quietly make
a lowercase root case-insensitive. Two optional keys describing one rule is not the "two mechanisms for one idea" that
warning was about.

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

pub enum Context { Common, Library, SortedLibrary, Playlist, Queue, Browser, Filters, Settings }

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
  remote.rs         unix socket server, cmus-remote-compatible commands
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

**M9 — remote socket, MPRIS if wanted.**

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
  a line has been typed. The prompt swallows every key while open, which is what stops `q` quitting mid-command.
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

  | cmus                           | gmuse today                    |
  | ------------------------------ | ------------------------------ |
  | `fset dance=…`                 | `[catalogs.dance] roots = […]` |
  | `factivate dance`              | `:catalog dance`               |
  | view 6 lists them, space/enter | view 6 is a stub saying "M8"   |

  **We had already built cmus's named filters and called them catalogs.** That is a good outcome: the concept was right
  and only the membership rule was narrow. So a catalog becomes a named filter expression, `roots` stays as sugar, and
  view 6 finally has a job.

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

---

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
