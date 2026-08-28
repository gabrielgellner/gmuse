# DESIGN.md — a cmus-shaped music player in Rust

Working title: `rcmus` (rename before first commit).

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
- Network sources: no Spotify, Subsonic, YouTube, yt-dlp, no `reqwest`
- Tag _editing_ (reading only)
- Any async runtime. No `tokio`, no `async-std`. Threads and channels.

Dropping these is a deliberate scope decision, not an oversight. They are the reason comparable projects carry 40+
dependencies.

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

`~"artist=foo&bitrate>192"`, with `&` (and), `|` (or), `!` (not), parentheses, `=`/`<`/`>` comparisons, and bare words
matching any text field.

```rust
pub enum FilterExpr {
    And(Box<FilterExpr>, Box<FilterExpr>),
    Or(Box<FilterExpr>, Box<FilterExpr>),
    Not(Box<FilterExpr>),
    Cmp { field: Field, op: CmpOp, value: Value },
    Text(String),
}

impl FilterExpr {
    pub fn compile(&self) -> impl Fn(&Track) -> bool + '_;
}
```

**Compile once to a closure**, do not re-interpret the AST per track. Filters run over the whole library on every
keystroke in view 6.

### Format strings

`%a - %l - %02n. %t` etc., with width/padding specifiers and conditional groups for missing fields. Parse to a
`Vec<FormatAtom>` at config-load time and render against a `&Track`. Never re-parse per frame — the now-playing line
renders at whatever the UI tick rate is.

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

**M2 — the audio API.** Wrap M1 in `PlayerHandle` with play/pause/stop/seek/ volume and a `PlayerEvent` stream. Drive it
from a `main` that reads stdin lines. Verify: seek is sample-accurate and the position display doesn't jump on seek.

**M3 — store and scan.** `walkdir` + `lofty` into `Store`, postcard cache, incremental rescan by mtime. No UI. Verify
against a real library — measure scan time and memory on ~20k tracks.

**M4 — the views.** ratatui, views 1–5, navigation, `Tab`, `Space` expand. Read-only, no playback wired in. Verify: tree
renders correctly for compilations, multi-disc albums, and missing tags.

**M5 — next() and playback wiring.** Implement `next.rs` with its full test table, wire views to `PlayerHandle`. Verify:
every row in the test table, then listen for an hour.

**M6 — command layer.** `Command`, rc parsing, `:` line, `Bindings`, view 7. Verify: every default binding, plus
rebinding round-trips through the rc file.

**M7 — gapless.** Pre-decode next track into a second buffer, transition without draining to silence. Verify on a
continuous-mix album — this is the acid test.

**M8 — filters and format strings.**

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

1. **Gapless.** Needs encoder delay/padding handled (Symphonia reports it for MP3 and Vorbis) _and_ the next track
   decoded and resampled into a second buffer before the current one drains. Getting the transition sample-exact is the
   fiddliest part of the project.
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
