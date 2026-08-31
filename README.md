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

```sh
just listen               # play with session logging on
just log                  # what the last session recorded
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
  -v, --volume <VOLUME>      initial fader position, 1.0 is unity
      --no-resume            do not restore or save the session
  -L, --library <DIR>        library root to scan, repeatable  [env: GMUSE_LIBRARY]
      --rescan               re-read every file, ignoring mtimes
      --no-cache             do not read or write the library cache
      --cache <PATH>         where the library cache lives      [env: GMUSE_CACHE]
      --repl                 use the line prompt instead of the TUI
      --smart-artist-sort    sort artists ignoring a leading The/A/An  [default: true]
```

The TUI opens on the library tree. Views are nvim-style tabs: `gt`/`gT` step, `3gt` jumps to view 3, and `g<Tab>`
returns to the last one. `j`/`k` move and take counts (`3j`), `gg`/`G` go to top/bottom, `12G` to a row. `l` opens the
node under the cursor and `h` closes it, `zM` closes everything, `tab` swaps pane in view 3, `q` quits.

**View 1 is one pane holding all three levels** — artists, their albums, and the tracks of whichever albums you have
open. Several artists and several of their albums can be open at once, and the whole thing reads as one buffer: `j`,
`3j`, `gg`, `G`, `/` and the relative gutter all work over it uniformly.

`h` on something **already closed** closes its parent and moves there, so repeated `h` walks up and out without needing
a notion of "level".

The `z` family is vim's, whole: `zo`/`zc`/`za` on one fold, `zO`/`zC` with the subtree, `zR`/`zM` for all of them,
`zr`/`zm` a level at a time, and `zx` to close everything **except** the folds holding the row you are on — the answer
to "collapse all this, but keep my place". `zt`, `zz` and `zb` put the cursor's row at the top, middle or bottom of the
pane, and `gp` jumps to the playing track, opening whatever was hiding it and disturbing nothing else.

`zv` is deliberately not bound: the cursor here always sits on a row the tree drew, so it could never do anything.
`zr`/`zm` a level at a time. That last pair is the one worth learning — one `zr` puts every artist's albums on screen,
another puts every track there, and `zm` walks back out. It works because the levels are **homogeneous**: every row at
the first level is an artist and every row at the second is an album, so "collapse to artists" is a real operation. A
file explorer cannot offer it, because "three directories deep" is not a category.

**Motions that use the shape rather than counting rows.** `}` and `{` step to the next row at the level you are on, `]]`
and `[[` always mean artist, `-` goes up to what a row is filed under, `]z` to the end of its group, and `zj`/`zk` to
the next and previous openable row. They mean the same thing in a flat list, where the album dividers _are_ the
structure — so `}` is the next album in view 2, and `-` the top of the one you are in. One idea rather than a second set
of keys.

A level change **holds the cursor still on screen** and opens the tree behind it. nvim does not do this — it keeps the
cursor on its buffer line and lets it slide down the window as the folds above it open — but here the cursor is the
subject rather than the top of the window. Lists also draw vim's `~` past their last row, which is what makes holding
the cursor possible near the end.

**`Enter` confirms.** On an artist or an album it opens or closes; on a track — where there is nothing left to open — it
plays. `<leader>x` plays what is under the cursor from anywhere, so the two are not redundant: `Enter` acts on structure
until structure runs out, then acts on the thing itself.

**Start a chord and it tells you what can follow.** Press `z`, or the leader, or `g`, and the bottom panel lists every
key that extends it and what each one does. It is read off the binding table, so a `:bind` appears in it at once.

It waits 300ms first, so a chord you already know is never interrupted by a panel you did not need.
`:set which_key_delay=0` shows it immediately, and any number of milliseconds sets the wait. This is not vim's
`timeoutlen` and does not change what a key _means_ — no key here is both a command and a prefix, so nothing is ever
waiting to find out. The delay is only about when to draw, and it is charged once per chord: once the panel is up, going
deeper redraws it at once.

**`space` is the leader**, and `<leader>?` shows the keys that work in the current view.

**View 8 is the guides** — short pieces on how to do things, as against which key does what. `<leader>?` answers the
second; the guides answer the first, which is the one you cannot find by pressing keys. Titles on the left, the guide on
the right, `tab` between them, and the ordinary motions scroll it.

Contexts for binding: `common`, `library`, `sorted`, `playlist`, `queue`, `browser`, `filters`, `settings`.

`<leader>` in a binding stands for whatever `leader` says, `space` by default:

```toml
leader = "space"

[keys.common]
"<leader> ?" = "help"
"<leader> p" = "playlist list"
```

A leader must be a key that is **not bound on its own** — a chord cannot extend one that is — which is why fold-toggle
moved off `space` to `o`. That is also the point: `?` keeps meaning reverse search, as in vim, and help lives behind the
prefix rather than taking a key it should not have.

**Marks fold, both ways.** `/loffler` and `/loeffler` both find Christian Löffler, and so does `/löffler` — a mark can
be typed around two ways and searching tries both. The same holds for `:filter` and for sorting, so he files under Lo
rather than after every artist beginning Lz. Typographic punctuation folds too: `don't` finds `don’t`, and because this
library stores Windows-illegal characters as fullwidth stand-ins, `Thereof / Sparing` finds the file whose name says
`⧸`. Smart-case is unaffected — a capital asks for case to matter, never for the mark back.

`:set relative_numbers=true` adds a vim-style distance gutter — absolute on the cursor line, distance elsewhere. Track
lists count **tracks**, so an album divider never shifts the number `3j` needs; the artist/album pane counts every row,
because an artist and an album are both jumpable. The playing track is emphasised in every list it appears in.

**Transport lives behind the leader.** `Enter` or `<leader>x` plays the selection, `<leader>c` pauses, `<leader>v`
stops, `<leader>b`/`<leader>z` are next/previous, `←`/`→` seek 10s, `<leader><`/`<leader>>` seek 60s, and
`<leader>-`/`<leader>+` adjust volume by 3 dB.

The prefix is deliberate. Navigating is what your hands are doing in these views, and reaching for the transport is
stepping outside that — so it costs a key, and the bare letters go to the folds and motions that browsing actually
wants. The letters themselves are unchanged, so there is one alphabet to learn rather than two.

Modes: `s` shuffle, `r` repeat, `R` repeat-one, `C` continue, `m` cycles all/artist/album. `a` adds the selection to the
open playlist, `e` to the queue, `A` to the library.

`dd` cuts a row and takes a count (`3dd`), `D` cuts to the end; `yy`/`Y` copy the same way, and `p`/`P` put after or
before the cursor. `u` undoes, `^r` redoes. vim's registers, so `dd` then `p` swaps two tracks, and `yy` — open another
playlist — `p` copies one across.

The view you were on, the queue, the open playlist, the current track, its position and the playback modes all come back
on the next launch, **paused** — including the shuffle seed, so a shuffled sequence resumes rather than restarting.
`--no-resume` starts fresh. Settings named in `config.toml` win over what was remembered; anything the config does not
mention keeps whatever you last set.

Loudness is matched from ReplayGain 2.0 tags when files have them — `set replaygain=auto` follows the play order, taking
the album figure in order and the track figure when shuffling. Analysis is deliberately somebody else's job: tag a
library with [rsgain](https://github.com/complexlogic/rsgain) (`rsgain easy ~/Music`) and gmuse reads what it wrote.
Untagged files play untouched, so the setting is inert until something is tagged.

`:filter` narrows the library to what matches an expression, and `:filter` with no argument clears it. Active catalogs
union; the filter intersects — so it narrows what you are already looking at, and it narrows what shuffle picks from
too, because §5 builds its pool from the library.

```
:filter genre="jazz"|genre="blues"
:filter artist="*ferrell*"&year>2020
:filter year>=1990&year<2000&(genre="folk"|genre="country")
:filter any="*live*"          # any = every text field
:filter filename="*.opus"
:filter !genre?               # tracks with no genre at all
:filter genre?&!year?         # tagged with a genre but no year
:filter play_count=0          # never played
:filter days_since_played<7   # heard this week
:filter rating>=4             # the good ones
:filter !rating?              # not judged yet
```

`&` `|` `!` and parentheses; `=` `!=` for text, `=` `!=` `<` `<=` `>` `>=` for numbers. A trailing `?` asks whether the
field is there at all — `genre?` has one, `!genre?` has none — and an empty tag counts as missing. Strings match by
**glob**, where `*` is the only metacharacter — `"Ferrell"` is exact, `"*Ferrell*"` contains, `"Ferrell*"` starts. `?`
is a literal question mark, and matching is **smart-case**: insensitive unless your pattern contains a capital.

Fields are the same names the format language uses:
`artist albumartist file_under album title display_title genre label path filename any` for text,
`track disc year duration bitrate samplerate channels` for numbers.

Four more come from the play history rather than the tags: `play_count`, `days_since_played`, `days_since_added` and
`rating`. Dates are exposed as _days since_ rather than as timestamps, so a query reads as English and the language
needs no date literals. A track never played has no `days_since_played` at all — `!days_since_played?` finds exactly
those — while `play_count` is `0` for them, because "never" is an answer. Unrated is likewise _missing_ rather than
nought stars — `!rating?` finds what you have not judged.

A rating is **1 to 5, or absent** — nothing carries zero stars, so both `:rate 0` and `rating=0` are refused, each with
a message naming what to write instead. That keeps _unrated_ and _rated badly_ as different questions.

`:rate 1`–`:rate 5` rates what is playing, or the track under the cursor when nothing is. `:rate none` clears it and a
bare `:rate` reports. **Ratings are gmuse's, not the file's**: §1 keeps tag editing out of scope, so they live in
`$XDG_STATE_HOME/gmuse/history.toml` and do not travel with the files.

`:playlist open <name>` fills view 3, from a saved rule or a saved file — they are one vocabulary.
`:playlist save <name>` writes view 3 out, `:playlist list` says what there is, and `:freeze` detaches a rule without
writing anything. See [Playlists](#playlists).

Typing while the completion menu is open **narrows** it; `Backspace` widens it again, and a line matching nothing leaves
the menu up saying `no matches` rather than vanishing. `C-n`/`C-p` or the arrows walk it, `C-y` accepts, `Esc` closes
the menu and a second `Esc` the line.

`:` opens the command line — `Tab` completes verbs and their arguments, including catalog names, setting keys and the
values each key accepts. `/` and `?` search, `n`/`N` repeat the search.

**View 7** lists every setting, catalog and binding, and `space` or `Enter` changes the one under the cursor — booleans
flip, named values cycle, exactly as `:toggle` does. `resample_quality` needs the device rebuilt so it says to use
`:set`, and a binding row points at `:bind`.

`label` is the record label, from `TPUB` / `ORGANIZATION` / `LABEL`. It has its own field because it is otherwise
homeless: a label sampler is a **compilation**, so its `albumartist` belongs to `Various Artists` (or, for a stray
single, to the track's own artist) — and a label written into an artist field turns up in your library looking like a
band. Bandcamp does exactly that, for want of anywhere better to put it.

Track lists are divided by album, and by disc within a multi-disc album. Turn it off with `:set album_headers=false`.

## Configuration

`$XDG_CONFIG_HOME/gmuse/config.toml`. Settings and theming are TOML tables; key bindings hold **commands**, the same
language the `:` line takes — so anything you can type at the prompt, you can bind.

```toml
startup_commands = ["view 1"]

[settings]
shuffle = true
aaa_mode = "album"          # all | artist | album
replaygain = "auto"         # off | track | album | auto

[keys.common]
"^n" = "next"
o = ["add-queue", "next"]         # a list is a macro
"M-f" = "seek +30"

[format]
# {artist} {album} {title} {track} {year} {duration} {genre} {label} … and more.
# `track` is how a row reads in the library tree, where it sits beside an
# artist row and so has to look different.
# [ ... ] disappears entirely if a field inside it is missing.
# {track:02} pads, {title:.40} truncates, {duration:>7} right-aligns.
now_playing = "{artist}[ — {album}] — {title:.40}"
track = "{track:2}  {display_title}  {duration}"

[theme.palette]
accent = "magenta"

[theme.styles]
border_focused = { fg = "accent", mods = ["bold"] }
```

## Catalogs

Separate bodies of music, so shuffling never crosses from a string quartet into a house track:

```toml
[catalogs.general]
roots = ["~/Music/General"]

[catalogs.electronica]
roots = ["~/Music/Electronic", "~/Music/Ambient"]

[catalogs.classical]
roots = ["~/Music/Classical"]

# A catalog is a named, persistent filter, so it can be an expression instead —
# or as well, in which case both must match.
[catalogs.jazzish]
where = 'genre="*jazz*"|genre="*blues*"'

[catalogs.recent-dance]
roots = ["~/Music/Electronic"]
where = "year>2015"

active_catalogs = ["general"]   # or ["all"]
```

A `where`-only catalog narrows what has already been indexed; it adds nothing to _scan_, so the files still have to be
reachable from `library` or from some catalog's roots.

`:catalog electronica` activates one, `:catalog folk pop` a subset, `:catalog +jazz` / `-jazz` adjusts, `:catalog all`
restores everything. **View 6** lists them: `space` turns one on or off, `enter` makes it the only one. The active set
shows in the status line and survives a restart.

Catalogs confine **browsing and shuffle**. Playlists and the queue hold tracks directly and cross catalogs freely — that
is the point of a playlist. Everything is scanned once whatever is active, so switching is instant.

Unknown tables, settings and styles are **errors**, reported with where they are — `keys.common.^n` — and the rest of
the file still loads. See [`examples/config.toml`](examples/config.toml) for every option, and view 7 in the app for
every setting and binding currently in force.

## Where your data lives

Three directories, divided by **worth** rather than by kind:

|                         | holds                 | recoverable?                    |
| ----------------------- | --------------------- | ------------------------------- |
| `data_dir`              | ratings, play history | **no** — it is what you decided |
| `$XDG_STATE_HOME/gmuse` | session, audit log    | yes — it is where you were      |
| `$XDG_CACHE_HOME/gmuse` | the scanned library   | yes — rescan                    |

### Nothing is ever rewritten

Ratings and plays are recorded as **events**, appended to a log, rather than as state in a file that gets rewritten:

```
<data_dir>/history.toml          the base, written once and never again
<data_dir>/events-<machine>.log  this machine's appends
<data_dir>/events-<other>.log    another machine's, arriving by sync
```

Reading is the base plus every log, folded in timestamp order. This matters the moment two machines share the directory.
With a state file, playing a track on one and rating one on the other means whichever file wins the sync takes
_everything_ the other machine did with it — the blast radius of a whole-file write is the library, not the track. With
logs, each machine appends only to its own file, so a sync has nothing to resolve: plays add up, and a rating is the
latest by timestamp, which at least gives both machines the _same_ answer.

Each machine's id lives in `$XDG_STATE_HOME/gmuse/machine-id` — deliberately outside the synced directory, or both
machines would read one id and collide exactly where it matters.

The log records **decisions, not interactions**: a play and a rating are things you meant, a seek is not. That is what
keeps its growth proportional to listening. A year is about 4 MB and folds in 5 ms; thirty years folds in 62 ms, so
there is nothing to compact.

`data_dir` defaults to `$XDG_DATA_HOME/gmuse` and is the one you want in a backup. It is a setting because that is a
personal answer:

```toml
data_dir = "~/MusicLibrary/.gmuse"   # ratings travel with the music
```

It cannot default inside the library, because roots may be several and on different disks. A history left in the old
`$XDG_STATE_HOME` location is **moved** on first run, and gmuse says so.

**Changing `data_dir` later does not move anything**, and gmuse will not guess — it cannot tell a moved directory from a
first run, or from another machine's data not having synced yet, and copying on a guess is how one library's ratings end
up attached to another. Instead it says what happened and what to do:

```
data_dir: changed to ~/MusicLibrary/.gmuse, but ~/.local/share/gmuse still holds your ratings.
Move them with `gmuse --migrate-data`, or by hand — both history.toml and events-*.log.
```

`--migrate-data` moves the base, **every** log and the playlists — `history.toml` alone would leave recent plays and
ratings behind.

A log whose name is already taken is the _likely_ clash, not a rare one: it is the same machine's, and gmuse writes one
at the new location on the launch that warns. Only the events the destination **lacks** travel, into a file of its own —
logs fold together by timestamp, which is what having several in a directory is for:

```
data_dir: moved 1 item(s) from ~/.local/share/gmuse
  recovered 6 event(s) into .../events-cc49b23-recovered.log (1 already had a home)
```

Taking the difference covers every case: a log **copied** forward rather than moved contributes nothing and is named;
one overlapping partly contributes its new half; a disjoint one contributes all of itself. Nothing is ever overwritten
and no play is counted twice, and the warning repeats until nothing is stranded.

Ratings are gmuse's, not the file's — §1 keeps tag editing out of scope — which is exactly why they need a home you
choose rather than one they are stuck with.

**Reorganising your library does not lose them.** A track is identified by its path, so a rename would ordinarily look
like one deletion and one unrelated arrival — ratings orphaned, play count back to zero, and the file reappearing in
`days_since_added<30` as if it were new. Instead the scan pairs departures with arrivals by size, duration and tags,
carries the history across, and says so:

```
library: 311 tracks (3 added, 0 updated, 308 unchanged, 3 removed, 0 errors) in 0.11s
  3 tracks moved, and their history moved with them
history: carried 3 across a move
```

An **ambiguous** match is declined rather than guessed — two copies of one track, or two untagged files of the same
length, keep their own history rather than risk attaching a rating to the wrong song.

## Playlists

View 3 has **two panes**, like view 1: the playlists on the left, the open one's tracks on the right, `tab` to swap. The
left pane lists rules first, then saved files, with a track count beside each; `Enter` opens one and moves you into it.
It rereads the directory on entry, so a playlist dropped in from another program appears.

Playlists are files, kept in `<data_dir>/playlists`:

```
:playlist new roadtrip      start an empty one
:playlist open roadtrip     open it
:playlist save mix          copy the open one under a new name
:playlist list              what there is
```

`a` adds the selection to **the playlist you have open**; `dd` cuts, `yy` copies, `p` puts. Every one of them is
**written to the file immediately**, the same rule ratings and plays follow: losing what you meant is worse than the
cost of a write. Adding with nothing open says so and points at `:playlist new`, rather than inventing a file you did
not ask for.

`u` undoes an edit and `^r` redoes it, per playlist — vim's undo is per buffer, and a playlist is a buffer, so undoing
here never reaches into a list you are no longer looking at. The queue keeps its own history too.

The register is **global**, as vim's are — that is what makes `yy` here, open another playlist, `p` there work, and it
is why reordering needs no key of its own: `p` puts _after_ the cursor, so `dd` then `p` exchanges a track with the next
one.

`open` takes a **rule or a file** without caring which, because both are play sources — see
[Smart playlists](#smart-playlists). A rule cannot be edited; `:playlist save <name>` copies its current contents into
an ordinary playlist, and `:freeze` detaches it without writing anything.

The session remembers which playlist was open **by name** — the tracks live in the file, so a copy could only go stale,
and reopening gets whatever the file says today.

The format is **M3U8**, not one of ours. A playlist is an ordered list of paths, which is exactly what M3U is, and M3U
is what everything reads — VLC, foobar2000, Plex, beets, cmus. We write `#EXTM3U` with `#EXTINF` lines and read `.m3u`,
`.m3u8` and cmus's `.pl`. The set of playlists **is** whatever the directory holds, so one dropped in from another
program simply appears.

Paths are written relative to the playlist file, so a library and its playlists move together. A playlist naming music
the library does not have loads short and says how many were missing, rather than quietly dropping them.

`#EXTINF` is write-only: emitted for other programs, ignored on read, because our own tags are better and a metadata
cache in a text file goes stale.

## Smart playlists

A saved rule that fills view 3. `:playlist <name>` loads one, `Tab` completes the names.

```toml
[playlists.fresh]
where = "days_since_added<30"
sort = "-added"                 # newest first

[playlists.neglected]
where = "play_count=0"
sort = "random"

[playlists.best]
where = 'rating>=4&genre="*jazz*"'
sort = "-rating, artist, album, track"
```

A catalog and a smart playlist can hold the same expression and still be different things, and the difference is not
that one is saved — both are. A catalog **narrows**: it hides everything outside itself, and shuffle cannot reach out. A
smart playlist is a **source**: it fills view 3 and leaves the library exactly as it was. _Show me only my jazz_ is a
catalog; _play my highly rated_ is a smart playlist.

Like any playlist, it crosses catalogs freely — that is the point of one — and shuffle, repeat and the queue treat it as
they treat any other list.

It is a **live query, not a snapshot**: the rule is re-read whenever something it depends on moves, so rating a track or
playing one changes what a `rating` or `play_count` rule answers without reloading it. That also means it cannot be
edited by hand — removing one track from a rule would need an exception list bolted onto the rule, which is how these
become unexplainable. `:freeze` turns the loaded one into an ordinary playlist, which _is_ editable, and view 3's title
says `playlist: <name>` while a rule owns it.

`sort` is a comma-separated list of field names, applied left to right, with `-` for descending. The keys are the names
the filter and format languages already use, plus `random`. Numbers compare as numbers, so track 2 precedes track 10.
Artist keys honour `smart_artist_sort`, so a sorted playlist agrees with the library tree about where "The Beatles"
goes. A missing value sorts **last in both directions** — a list whose first screen is blanks looks broken rather than
sorted.

Dates are the one place the two languages differ. A filter says `days_since_added`; a sort says `added`. `days_since`
reads well in a question and inverts in an order — ascending would mean newest first — so `-days_since_added` written
for "newest" would silently give the oldest and a plausible-looking list. Each language takes one spelling and rejects
the other by name.

## Licence

GPL-3.0-or-later. See [LICENSE](LICENSE).

Put your roots in the config so they survive a cache format change:

```toml
library = ["~/Music"]
```

Roots come from the config and from `--library`; the cache's remembered roots are a **fallback**, used only when neither
says anything, so scanning once with `--library` and running bare afterwards still works. A one-off `--library`
therefore stays a one-off rather than joining the set for ever. A root that lies under another is dropped before
scanning, so overlapping roots do not walk — or count — the same file twice.

The library is scanned on startup and cached at `$XDG_CACHE_HOME/gmuse/library.postcard`, so later runs need no
`--library`. Rescans are incremental by mtime: 20,000 tracks scan cold in 0.86s and rescan in 0.08s.

At the prompt: `load <path>`, `play`, `pause`, `toggle`, `stop`, `seek 90`, `seek +10`, `seek -10`, `vol 0.5`, `status`,
`help`, `quit`.

[Symphonia]: https://github.com/pdeljanov/Symphonia
[`symphonia-adapter-libopus`]: https://crates.io/crates/symphonia-adapter-libopus
