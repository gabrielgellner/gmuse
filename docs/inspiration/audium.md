# audium

<https://github.com/takashialpha/audium> — local-library music TUI, Rust (edition 2024) + ratatui, Linux only.

Closest to us in construction — Symphonia, no FFmpeg, threaded audio, "zero `unsafe` blocks", one binary and no
background process — and furthest from us on what a player is allowed to write.

## Storage, and an independent confirmation

Its layout is split by _lifecycle_, using XDG for what XDG is for:

|                    |                                                   |
| ------------------ | ------------------------------------------------- |
| `$XDG_CONFIG_HOME` | configuration                                     |
| `$XDG_STATE_HOME`  | state                                             |
| `$XDG_DATA_HOME`   | a small JSON index — filenames and playlists only |

and **"all writes use atomic rename operations for crash safety."**

That is the recommendation in the Decision Log entry on SQLite, arrived at independently: separate stores for things
with different lifecycles, and temp-then-rename rather than a database for durability. It is worth knowing that the
conclusion is not idiosyncratic.

We are already partway there — the audit log is in `$XDG_STATE_HOME/gmuse/`, the cache in `$XDG_CACHE_HOME`, config in
`$XDG_CONFIG_HOME` — so the per-track user state of #13 and #14 has an obvious home, and it is not the cache.

## Session resumption — the idea to take

_"The queue, current track, position and playback modes are restored on the next launch, paused."_

We restore nothing. Reopening gmuse loses the queue, where you were in a track, and which modes were on. This is state,
not config, it belongs in `$XDG_STATE_HOME` beside the audit log, and **paused** is the right detail — resuming playback
unbidden on launch would be obnoxious.

Cheap, and the sort of thing only noticed by its absence. Taken — issue #16.

## Themes

Fifteen truecolor themes and two console themes, **live preview** while choosing, and automatic fallback to 16-colour
mode on limited terminals.

**Declined, 2026-08-29** — broad terminal support is not a goal here and the ANSI-name palette already degrades by
design. Left in for the record. Two things we lack. **Detecting terminal colour support** and falling back deliberately
— our ANSI-name palette already degrades gracefully by design, but nothing checks, so a truecolor theme on a 16-colour
terminal is whatever ratatui does. And **live preview**: view 7 lists settings, so a theme applied as the cursor moves
over it is a small change to a view we already have. (trollstov detects light-vs-dark for a different purpose — between
them that is a complete story about not guessing the terminal.)

## Where it goes the other way

**Metadata editing is a headline feature**: title, artist, album _"read from the files' own tags, and edits are written
straight back to them."_

That is the second project here to take the opposite position from §1's "tag editing (reading only)" — trollstov carves
out ratings, audium writes everything. Neither is an argument that §1 is wrong; both are evidence that it is a **trade**
rather than the obviously correct answer, and #14 should say so. Our reason holds — this project does not want to be the
thing that corrupted someone's library — but it is a choice, not a law.

It also **copies imported audio into `$XDG_DATA_HOME/audium/music/`**. We scan roots in place and never move a file.
That difference is worth stating explicitly somewhere: gmuse does not own your music directory.

## A corroborated rejection

audium uses **rodio** over cpal. Its feature list has free seeking, speed control and three loop modes — and **no
gapless playback**. The Decision Log's reason for going to cpal and Symphonia directly (_"rodio's `Source` abstraction
blocks gapless, precise seek, and controlling buffer depth"_) predicted exactly that gap.

## Smaller things

- **`?` for grouped help.** Four of five projects here. Our view-7 switch is now clearly the outlier.
- **Playback speed**, with pitch-preserving speed on its roadmap — so the current control shifts pitch. Not something we
  want, but a reminder that "speed" and "pitch-preserving speed" are different features.
- **Filtering** is playback-level here, not a view. Another data point for M8's shape.

## Deliberately not taken

- **Lyrics**, plain or LRC, with an overlay and an editor. §1, explicitly.
- **Copying music into an app-owned directory.**
- **Writing tags** — for now; see #14.
