# spotatui

<https://github.com/LargeModGames/spotatui> — multi-source music TUI, Rust + ratatui, MIT (a fork of spotify-tui).

The odd one out here: it began as a Spotify client and has grown into a general player with YouTube, Subsonic/Navidrome,
Qobuz, internet radio, local files, synced lyrics, a visualiser, Discord Rich Presence, an AI DJ, and a Lua plugin
system. Almost all of that is §1's out-of-scope list, item by item.

Read it anyway, for one thing it does better than anything else here: **presentation is configuration**. That is exactly
M8's territory, and it has already made the mistakes.

## Table columns as data — the idea to take

```yaml
tables:
  songs:
    - { id: liked }
    - { id: title, width_percent: 40 }
    - { id: artist, header: "Band", width_percent: 30 }
    - { id: album }
    - { id: length }
```

Each column takes an optional `header` (rename), and `width_percent` **or** `width` — not both. Separate tables for
`songs`, `album_tracks`, `recently_played`, `albums`.

We hardcode our columns. This says the whole track list — which columns, in what order, how wide, called what — can be a
config table, and it costs nothing structurally now that settings are data. It is also the natural home for a rating
column (#14) and a play count (#13) without either needing a UI decision baked into code.

## Format templates, and a design question for §7

```yaml
format:
  playbar_status: "{state} ({device} | Shuffle: {shuffle} | Repeat: {repeat} | Volume: {volume}%)"
  window_title: "{title}{artist}"
```

Placeholders are **named** — `{state}`, `{volume}`, `{shuffle}` — where §7 currently specifies cmus-style
`%a - %l - %02n. %t`. That is a real choice we have not made explicitly:

- `%a` is terser, cmus-compatible, and has an established vocabulary users may already know.
- `{artist}` is self-documenting, needs no lookup table, and leaves room for names longer than one letter — which
  matters the moment there are fields like `replaygain` or `play_count` with no sensible single letter left.

Worth deciding in M8 rather than inheriting. Note also that templates apply to **more than one place** — the playbar and
the window title here, and spotify-player uses one for its multi-line playback window.

## The mistake to avoid

Its `playbar_status_source` template has placeholders that "arrive pre-composed with their own `| Shuffle:` /
`| Repeat:` prefix," unlike the equivalent Spotify template.

That is a leaked abstraction: the placeholder carries layout, so the template author cannot control spacing or ordering,
and two templates for the same job behave differently. It is what happens when "hide this when absent" is solved by
baking the separator into the value.

§7 already has the better answer — **conditional groups for missing fields** — and this is the concrete reason that
feature is not optional. If we ship format strings without it, we will end up here.

## Layout as config, and resizable at runtime

```yaml
behavior:
  sidebar_position: left # left | right | hidden
  playbar_position: bottom # bottom | top
  sidebar_width_percent: 20
  playbar_height_rows: 6 # 0 hides it
  small_terminal_width: 150 # responsive breakpoint
```

`{` / `}` resize the sidebar and `(` / `)` the playbar **at runtime, persisting to state**. spotify-player made pane
widths config; this makes them adjustable while you look at them, which is how anyone actually finds the right value.

The responsive breakpoint is the part we have nothing like: a defined behaviour for a terminal too small for the layout,
rather than whatever ratatui does when the constraints stop fitting.

## Corroborations

- **`?` for an in-app help menu.** Three of four projects here now. Our view-7 switch loses your place; `?` is currently
  bound to reverse-search and would have to move.
- **A source picker (`d`) that scopes both the sidebar and search to the active source.** That is our catalogs, reached
  independently — and it confirms that scoping _search_ to the active set, not just the browse tree, is expected.
- **Listening history as a normal feature**, not an exotic one. Supports #13.
- **"A config typo never blocks startup"** — invalid values fall back with a warning. Exactly our `ConfigError`
  collect-and-continue behaviour, arrived at independently.

## Deliberately not taken

- **Lua plugins.** A scripting runtime is a dependency, an API surface, and a security boundary. §1 does not list it
  only because §1 did not imagine it.
- **Everything network** — Spotify, YouTube, Qobuz, Subsonic, radio. §1, explicitly.
- **Visualiser, lyrics, Discord presence, AI DJ.** §1 names the first three.

## The cautionary half

This project is the clearest illustration in this folder of the failure mode §1 exists to prevent. It is a well-built,
actively developed TUI that started as one thing and now has six playback sources, an AI feature and a plugin system.
Nothing about that is wrong for its authors — but the gmuse brief opens by saying scope creep is _the_ named risk here,
and this is what the far end of that road looks like from the outside.

Take the config system. Leave the rest.
