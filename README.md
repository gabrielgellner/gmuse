# gmuse

***G***abriel's ***MUS***ical ***E***xperience.

A [cmus](https://cmus.github.io/)-shaped terminal music player for a local music library, written in Rust with a
pure-Rust audio path.

This is a reimplementation of cmus's _interaction model_ — its views, its next-track semantics, its command and
keybinding layer — not a port of its C. See [`docs/design/music-player-tui.md`](docs/design/music-player-tui.md) for the
full brief, the invariants, and the decision log.

## Status

Early. The audio path works; there is no UI yet.

| Milestone                                 | State                                    |
| ----------------------------------------- | ---------------------------------------- |
| M1 — sound out of the speakers            | ✅ done                                  |
| Resampler                                 | ✅ done (pulled forward — Opus needs it) |
| M2 — the audio API (`PlayerHandle`)       | next                                     |
| M3–M9 — library, views, commands, gapless | not started                              |

Today the TUI browses a scanned library across five views, and `--repl` gives a line prompt that can actually play files
— load, play, pause, stop, seek, volume, status. The two meet in M5.

## Building

Requires a Rust toolchain (edition 2024, MSRV 1.85) and **`cmake`**, which builds the bundled libopus:

```sh
brew install cmake        # or your platform's package manager
just ui ~/MusicLibrary    # scan a library and open the TUI
just ui                   # later runs reuse the cached library
just play ~/path/to/a/track.mp3   # line prompt, which can actually play
```

`just --list` shows the rest.

To build without Opus support and drop the only C dependency:

```sh
cargo build --release --no-default-features
```

## Formats

MP3, FLAC, Ogg Vorbis, WAV, AAC/M4A, ALAC via [Symphonia], and Opus via [`symphonia-adapter-libopus`] (bundled libopus,
statically linked).

## Design notes

Three threads, and the split is not negotiable — it is what determines whether playback is glitch-free:

```
UI thread            decoder thread              audio callback (cpal, RT)
─────────            ──────────────              ─────────────────────────
render/input   cmd   demux, decode, resample     drain the ring
               ───▶  push frames to rtrb    ───▶ write to the device
               ◀───                              update atomics only
               evt
```

The real-time callback may only read the ring buffer, write samples, and touch atomics. No allocation, no locks, no
syscalls, no panics. Everything expensive — decoding, sample-rate conversion — happens on the decoder thread.

### Sample-rate conversion

The device is opened at the file's own rate when it offers one, so the common case does no conversion. Otherwise audio
is resampled, because output devices advertise disjoint rate sets: a Bluetooth sink may offer only 16k and 44.1k while
every Opus file is 48k.

Quality is selectable (`GMUSE_RESAMPLE=fast|balanced|high`, default `balanced`). `balanced` uses rubato's FFT resampler,
which is both cheaper and cleaner than a long sinc for the fixed ratios a music player deals in.

### Cost

Steady-state playback, measured on an M5 MacBook Air:

|                            | % of one core |
| -------------------------- | ------------- |
| gmuse, Opus + FFT resample | 0.55%         |
| gmuse, MP3, no resample    | 0.65%         |
| `afplay` (macOS native)    | 0.75%         |
| `ffplay -nodisp`           | 1.00%         |

The target is to stay at or below `afplay` — this is meant to run in the background while you work.

## CLI

```
gmuse [OPTIONS] [FILE]

  -d, --device <DEVICE>      output device, matched by substring on the id  [env: GMUSE_DEVICE]
  -r, --rate <RATE>          force the device sample rate                   [env: GMUSE_RATE]
  -q, --resample <QUALITY>   fast | balanced | high                         [env: GMUSE_RESAMPLE]
  -v, --volume <VOLUME>      initial volume, 0.0-2.0
  -L, --library <DIR>        library root to scan, repeatable  [env: GMUSE_LIBRARY]
      --rescan               re-read every file, ignoring mtimes
      --no-cache             do not read or write the library cache
      --cache <PATH>         where the library cache lives      [env: GMUSE_CACHE]
      --repl                 use the line prompt instead of the TUI
      --smart-artist-sort    sort artists ignoring a leading The/A/An  [default: true]
```

The TUI opens on the library tree. `1`-`5` switch views (library, sorted, playlist, queue, browser), `j`/`k` move,
`space` expands an artist, `tab` swaps pane, `E`/`Backspace` expand/collapse all, `q` quits.

Playback: `Enter` or `x` plays the selection, `c` pauses, `v` stops, `b`/`z` are next/previous, `←`/`→` seek 10s,
`<`/`>` seek 60s, `-`/`+` adjust volume. Modes: `s` shuffle, `r` repeat, `R` repeat-one, `C` continue, `m` cycles
all/artist/album. `a`/`y`/`e` add the selection to the library/playlist/queue, `D` removes.

`:` opens the command line, `/` and `?` search, `n`/`N` repeat the search. View 7 lists every setting and binding.

## Configuration

`$XDG_CONFIG_HOME/gmuse/rc` is a list of the same commands the `:` line takes, run at startup:

```
# ~/.config/gmuse/rc
set shuffle=true
set aaa_mode=album
bind common ^n player-next
unbind common q
```

A bad line is reported with its number and skipped; the rest of the file still loads.

The library is scanned on startup and cached at `$XDG_CACHE_HOME/gmuse/library.postcard`, so later runs need no
`--library`. Rescans are incremental by mtime: 20,000 tracks scan cold in 0.86s and rescan in 0.08s.

At the prompt: `load <path>`, `play`, `pause`, `toggle`, `stop`, `seek 90`, `seek +10`, `seek -10`, `vol 0.5`, `status`,
`help`, `quit`.

[Symphonia]: https://github.com/pdeljanov/Symphonia
[`symphonia-adapter-libopus`]: https://crates.io/crates/symphonia-adapter-libopus
