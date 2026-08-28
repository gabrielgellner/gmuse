# russ

<https://github.com/ckampfe/russ> — TUI RSS reader, Rust, GPL-3.0.

The spare one. Two panes, few keys, no configuration file to speak of. Useful mostly as a check on the other two: almost
everything here is something we already do, which is reassuring about the shape we picked.

## Layout and navigation

Two panes — feeds left, entries right — and selecting an entry expands it to scrollable content.

`hjkl` does double duty: vertical movement within a pane, horizontal movement between panes. Same conclusion as
eilmeldung from a much smaller design, which is a decent argument that `h`/`l` should not stay unbound in gmuse.

`ctrl-u` / `ctrl-d` page while reading. We use `C-f` / `C-b`, from cmus.

## Keybindings

| Key                 | Action                                         |
| ------------------- | ---------------------------------------------- |
| `hjkl` / arrows     | navigate panes and scroll                      |
| `Enter`             | open the selected entry                        |
| `?`                 | show/hide help                                 |
| `q` / `Esc`         | quit                                           |
| `r`                 | refresh feed _or_ mark read, depending on pane |
| `ctrl-u` / `ctrl-d` | page up/down                                   |

`?` for help again — that is two of three projects, against our view-7 switch.

`r` doing different things in different panes is the same idea as our per-context bindings, reached without a binding
table: the key is one command that reads the focused pane. Ours is more general and costs a line of config rather than a
branch.

## Modal editing — considered and rejected

Normal mode for browsing, insert mode (`i`) for typing a feed URL, `Esc` back.

Right for an app whose main text entry is a URL. Wrong for a player driven by single keys, where modes would mean a mode
indicator and a class of "why is nothing happening" bugs. Our `:` and `/` prompts are the useful half of this — they
capture input while open, then get out of the way.

## Non-eager by principle

> "It will not automatically refresh your feeds on a timer, it will not automatically mark entries as read."

We rescan the library on every startup. At 0.08s on a local disk nobody notices; on a network mount they would. Worth
knowing the principle exists, though it is a scanning decision rather than a UI one.
