# russ

<https://github.com/ckampfe/russ> — a TUI RSS reader in Rust, "local-first, offline-first", GPL-3.0.

A useful counterweight to eilmeldung: where that one is richly configurable, russ is deliberately spare. Its interesting
choices are about _data_ and _when work happens_.

## SQLite as the store

Everything lives in a SQLite database (`feeds.db`, under the platform data dir), with `rusqlite` bundling SQLite itself.

This is the road not taken for our library. We use a postcard cache: a flat `Vec<Track>` serialised whole. The
comparison matters at M8, when the filter language arrives:

|             | postcard cache (ours)           | SQLite (russ)                                             |
| ----------- | ------------------------------- | --------------------------------------------------------- |
| Load        | whole file, instantly           | query on demand                                           |
| Filtering   | scan every track                | indexes, and the query language is nearly written for you |
| Saved views | recompute each time             | a view                                                    |
| Cost        | none — no dependency, no schema | a real dependency, migrations, a schema to keep in step   |

Nothing hurts yet: 20k tracks scan cold in 0.86s and load from cache instantly. But "filter runs over the whole library
on every keystroke" (§7) is a scan today, and would be an index lookup there.

## Non-eager by principle

> "It will not automatically refresh your feeds on a timer, it will not automatically mark entries as read."

Everything happens because the user asked. We rescan on every startup, which is fine at 0.08s on a local disk and would
not be on a network mount or a spinning drive. Worth making explicit rather than incidental.

## Modal editing

Normal mode for navigating, insert mode for entering a feed URL. We have a prompt line but no modes, and cmus is not
modal — so this is noted rather than adopted. It is the right answer for an app whose main text entry is a URL, and the
wrong one for a player whose main input is single keys.

## Platform data directories

russ uses `~/Library/Application Support` on macOS. We deliberately use the XDG layout everywhere, matching cmus and
keeping a dotfile-synced setup portable — a decision, not an oversight.
