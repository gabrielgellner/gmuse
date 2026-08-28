# spotify-player

<https://github.com/aome510/spotify-player> — a Spotify TUI client in Rust on ratatui, MIT.

The most feature-complete of the three, and the least directly applicable: it is a network client for a streaming
service, where §1 rules out network sources and album art outright. Read it for the shape of its _edges_ — how it is
driven from outside, how its config scales — rather than its middle.

## It validates M9

> A remote control socket (`cmus-remote` equivalent) for media keys and status bars — §1

spotify-player does exactly this and goes further: a CLI (`get`, `playback`, `search`) that speaks to a socket, **JSON
output for scripting**, and a **daemon mode** where the playback thread runs with no UI at all. Its thread split — UI /
playback / client socket — is the same shape as our UI / decoder / callback, with the socket added.

Two things worth stealing when M9 comes round:

- **JSON output.** `gmuse-remote --status --json` is far more useful to a status bar than parsing a formatted line, and
  it costs one serde derive on state we already have.
- **Daemon mode falls out of the socket.** If the socket can drive everything, running without a UI is not a separate
  mode, it is just nobody attaching one. Our `PlayerHandle` is already independent of the UI, so this is mostly free.

It also does **MPRIS on Linux and OS media-key events on macOS/Windows**, which §9 lists as "MPRIS if wanted".

## Themes as separate, switchable files

Its themes live in their own file rather than inline in the main config, and can be switched by name. Ours are a
`[theme]` table inside `config.toml`, which is right while there is one theme and wrong as soon as there are three.
Worth revisiting when a second theme exists — not before.

## Count prefixes

`5j` moves down five. That needs the same pending-prefix state machine as eilmeldung's `g g` chords, so the two are one
piece of work in the key handler, not two.

## Popups for context actions

A popup menu of what can be done to the thing under the cursor. cmus has no such concept, and it is a real answer to a
question we will hit: where does "add this album to a playlist, but which one?" live, once there is more than one
playlist.

## Not applicable

- Image rendering (Kitty/iTerm2/Sixel protocols) — §1 excludes album art in any form, deliberately.
- OAuth, token caching, `librespot` — no network sources.
- Audio backends behind compile-time features. We drive cpal directly and have exactly one optional feature (`opus`),
  which is the whole point of §2's dependency discipline.
