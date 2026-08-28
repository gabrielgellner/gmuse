# eilmeldung

<https://github.com/christo-auer/eilmeldung> — a TUI RSS reader in Rust on ratatui, GPL-3.0.

Read for its configuration and theming architecture, which is what a modern TUI does that cmus does not.

## The idea that changed our config

Its keybindings live in TOML, but **the values are still command strings**:

```toml
"j"   = ["down"]
"g g" = ["gotofirst"]
"o"   = ["open", "in articles read", "in articles nextunread"]
```

TOML did not replace their command language — it became the container for it. That reframed our question from "rc format
_or_ TOML" into "what holds the commands", and the answer is that both belong: commands are imperative and need a
language, settings and themes are declarative and want structure.

A list value is a macro, which we get for free.

## Two-level theming

```toml
[theme.color_palette]
background = "black"
highlight  = "yellow"

[theme.style_set]
header   = { fg = "accent_primary" }
selected = { mods = ["reversed"] }
```

Styles name a _semantic role_ and reference palette entries rather than colours. That indirection is the whole trick: a
theme becomes data, and the widget code never names a colour. We had 21 hardcoded `Color::` literals before adopting
this.

## Saved queries as views

```toml
feed_list = [
  'query: "Today Unread" today unread',
  "feeds",
]
```

A labelled filter appears in the list as a pseudo-folder. This maps directly onto §7's filter language: once that lands,
`query: "Recently Added" added<1w` in the library tree is nearly free, and it is a genuine enrichment over cmus's
transient view-6 filters.

## Not taken

- **Multi-key sequences** (`"g g"`) need a pending-prefix state machine. Worth having; kept separate from the config
  change.
- Its `[icon_set]` / `[border_theme]` granularity is more configurability than this project wants yet.
