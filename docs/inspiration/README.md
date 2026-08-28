# Inspiration

Projects worth learning from, and what specifically to take from each.

cmus remains the _behavioural_ specification — see [the design brief](../design/music-player-tui.md) §14. This folder is
for everything else: architecture, configuration, and the parts of a modern TUI that cmus predates.

**On licences.** Copyright covers expression, not ideas. Reading a GPL project and adopting its _architecture_ — "two
levels of theme table", "saved queries as pseudo-folders" — creates no obligation. Copying its code does. gmuse is
GPL-3.0-or-later by choice, not by contagion; nothing here has been copied.

| Project                     | What it is                     | What we took                                                                   |
| --------------------------- | ------------------------------ | ------------------------------------------------------------------------------ |
| [eilmeldung](eilmeldung.md) | TUI RSS reader, Rust + ratatui | TOML config holding command strings; two-level theming; saved queries as views |
| [russ](russ.md)             | TUI RSS reader, Rust + SQLite  | Non-eager refresh; the SQLite-vs-cache question                                |

## Open questions these raise

- **Should the library live in SQLite rather than a postcard cache?** russ puts everything in SQLite. That would make
  M8's filter language a query translation rather than a scan, and make saved views cheap. Against: it is a real
  dependency, and 20k tracks currently scan in 0.86s and load from cache instantly, so nothing hurts yet.
- **Should scanning be eager?** We rescan every startup. russ refuses to do anything on a timer and makes the user ask.
  Ours is fast enough not to notice, but the principle is sound for a library on a network mount.
- **Multi-key chords** (`g g`) and **count prefixes** (`5j`) need the same pending-prefix state machine in the key
  handler — one piece of work, not two. Tracked as an issue.
- **Themes as separate switchable files** rather than a table inside `config.toml`. Right once there is more than one
  theme; not before.
- **Popup action menus** for the thing under the cursor. cmus has no such concept, and it is the natural answer to "add
  this album to _which_ playlist" once there is more than one.
