# spotify-player

<https://github.com/aome510/spotify-player> — Spotify TUI client, Rust + ratatui, MIT.

Everything it does over the network is out of scope. Read for its **command vocabulary** and its **UI structure**, which
are the richest of the three.

## Prefix namespaces — the idea to take

Bindings are sequences, and the first key is a namespace:

| Binding     | Command       |
| ----------- | ------------- |
| `g n`       | NextTrack     |
| `g p`       | PreviousTrack |
| `g A`       | GoToArtist    |
| `g B`       | GoToAlbum     |
| `M-enter`   | ResumePause   |
| `C-c C-x /` | Search        |

`g` is "go to". That is how a command set grows past what single keys can hold — and ours is already at 40-odd bindings
on a flat alphabet, with filters, playlists and remote still to come.

`g A` / `g B` in particular have a direct meaning here: **jump to the artist or album of the track that is playing**,
from anywhere. cmus has no such motion and it is an obvious thing to want after ten minutes of shuffle.

It also supports **count prefixes** (`5j`) and lets a binding be set to `None` to remove it — we have `unbind`.

Both need the pending-prefix state machine in issue #1, which is why chords and counts are one piece of work.

## Layout and UI structure

- **Playback window** at top _or_ bottom, configurable, with a **format string** for its contents:
  `"{status} {track} • {artists} {liked}\n{album} • {genres}\n{metadata}"`. That is §7's format-string language arriving
  in M8, applied to more than one line.
- **Two library panels** with configurable width percentages (`album_percent`, `playlist_percent`, both 40 by default).
  We hardcode a 40/60 split.
- **`page_size_in_rows`**, default 20. We hardcode 10.
- **Relative line numbers** for lists and popups, optionally, for `5j`-style motion.

Three settings we currently hardcode, each now a one-line `SettingKey` since settings became data.

## Pages and popups

Its UI is organised as _pages_ with a _popup_ system layered over them, rather than our flat "seven views, one at a
time". The popups carry **actions**: a context menu of secondary commands on the item under the cursor.

This is the answer to a question we cannot express today — "add this album to _which_ playlist" — and it generalises:
sort order, jump-to, remove-from. Worth introducing when a second playlist exists.

A help popup is the same machinery, which is how eilmeldung's `?` overlay would land too.

## Not taken

Streaming, OAuth, image rendering, audio backends behind feature flags. All settled or excluded by §1–§2.
