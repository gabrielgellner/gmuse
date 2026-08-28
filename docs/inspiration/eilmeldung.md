# eilmeldung

<https://github.com/christo-auer/eilmeldung> — TUI RSS reader, Rust + ratatui, GPL-3.0.

## Layout and navigation — the strongest ideas here

Three columns: feed list, article list, article content.

`h`/`l` move between panes; `j`/`k` move within one. Straightforward, and already better than our `Tab`, which works for
two panes and stops scaling at three.

The genuinely novel part is what it does **without changing focus**:

> "Navigate to the article list and use `C-j`/`C-k` to move down/up in the feed list and use `M-k`/`M-j` to scroll the
> article content down/up."

So from one pane you can drive all three. The modifier says _which pane_, the letter says _what to do_. Applied to
gmuse's tree view: from the track pane, `C-j`/`C-k` would step album to album, refilling the track list, without a `Tab`
over and a `Tab` back each time. That is exactly the motion browsing a library is made of, and it is the single idea
here most worth stealing.

## Keybindings

| Key         | Action                 | Note for us                                            |
| ----------- | ---------------------- | ------------------------------------------------------ |
| `h` `l`     | move between panes     | we have only `Tab`                                     |
| `?`         | show all keybindings   | a popup, not a view switch — ours is view 7            |
| `:`         | command line           | same as ours                                           |
| `/`         | search                 | same as ours                                           |
| `c f`       | create feed            | a `c` prefix namespace                                 |
| `z`         | zen mode               | hide the chrome; interesting for a player left running |
| `1` `2` `3` | toggle display _scope_ | note: **not** view switching, as ours are              |

`?` and `z` are both worth having. `?` conflicts with our reverse-search binding, which would have to move.

## Config and theming — already adopted

Its keybindings live in TOML but the **values are command strings**:

```toml
"j"   = ["down"]
"o"   = ["open", "in articles read", "in articles nextunread"]
```

That reframed our config question from "rc **or** TOML" to "what holds the commands", and is why `config.toml` holds a
command language rather than replacing it. A list value is a macro, which we now have.

Its two-level theming — a palette of named colours, and styles naming a semantic role — is what `ui/theme.rs` is
modelled on. Both landed in !9.

## Not taken

Its `[icon_set]` and `[border_theme]` granularity is more configurability than this project wants yet.
