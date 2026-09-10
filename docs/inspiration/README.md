# Inspiration

Projects worth learning from, read for three things only:

1. **Navigation and layout** — how panes are arranged and moved between
2. **Keybindings** — anything richer than cmus's flat single keys
3. **TUI architecture** — how the interface is structured internally

One entry, [foobar2000](foobar2000.md), is here for a fourth reason and none of the first three: it is not a TUI, and
its interface and engine are irrelevant. It is read for its **query and title-formatting languages**, which are the most
developed of their kind, and as a place to look when a power-user question comes up that this project has not reached
yet.

The player and audio path are settled: see [the design brief](../design/music-player-tui.md) §1–§5. Nothing here is read
for storage, streaming, or audio backends. cmus remains the _behavioural_ specification; this folder is for the parts of
a modern TUI it predates.

**On licences.** Copyright covers expression, not ideas. Adopting "a `g` prefix for go-to commands" or "two levels of
theme table" creates no obligation. Copying code does. gmuse is GPL-3.0-or-later by choice; nothing here is copied.

| Project                             | Read for                                                                                       |
| ----------------------------------- | ---------------------------------------------------------------------------------------------- |
| [eilmeldung](eilmeldung.md)         | Three-pane layout; **driving other panes without leaving focus**; config and theming           |
| [spotify-player](spotify-player.md) | **Prefix namespaces** (`g n`, `g A`); pages and popups; configurable layout                    |
| [russ](russ.md)                     | Two-pane `hjkl`; modal editing (and why not)                                                   |
| [spotatui](spotatui.md)             | **Presentation as config** — table columns, format templates; and a scope-creep mirror         |
| [trollstov](trollstov.md)           | Tags as the only store; **portable ratings**; light/dark terminal detection                    |
| [rmpc](rmpc.md)                     | **A context menu built from typed sections**; template fallback chains; a layout tree declined |

## What gmuse should take, roughly in order of value

**1. `h`/`l` to move between panes, not just `Tab`.** Both RSS readers use them; we bind neither. `Tab` is fine for two
panes and stops scaling at three, and the tree view already has two with more coming.

**2. Drive other panes without changing focus.** eilmeldung's best idea: from the article list, `C-j`/`C-k` move the
_feed_ selection and `M-j`/`M-k` scroll the _content_. Applied here, that means stepping album-to-album from the track
pane without a `Tab` round trip each time — the exact motion browsing a library is made of.

**3. A `?` help overlay.** eilmeldung, russ, spotatui and audium all have one — four of five, and we are the outlier. We
have view 7, which is a whole view switch and loses your place. A popup is faster. Note `?` is currently bound to
reverse-search, which would have to move.

**4. Prefix namespaces for a growing command set.** spotify-player's `g n` / `g p` / `g A` (go to artist) / `g B` (go to
album), eilmeldung's `c f`. cmus's flat single keys are already crowded at 40-odd bindings, and this is how you add 50
more without running out of alphabet. Needs the chord machinery in #1.

**5. Popups as a first-class UI concept.** spotify-player's "actions" — a menu of secondary commands on the item under
the cursor. This is the natural answer to "add this album to _which_ playlist", which we cannot express today.
[rmpc](rmpc.md) §2 has the construction to build it with — a menu is a stack of _typed sections_ (input, select, list,
multi-action), each knowing how it takes focus — and names why it matters more to us than to a player: the librarian's
verbs (merge, retire, relate, withdraw a credit) are invoked rarely and precisely, which is the profile a menu serves
and a keybinding does not.

**6. Configurable page size and pane widths.** We hardcode a 10-row page and a 40/60 split. spotify-player makes both
settings, and it costs a `SettingKey` each now that settings are data. spotatui goes further and makes them adjustable
_at runtime_, persisting — which is how anyone actually finds the right value.

**6b. Track-list columns as config.** spotatui's `tables:` section makes which columns appear, in what order, how wide
and under what heading all data. Ours are hardcoded. This is also where a rating (#14) or a play count (#13) would go
without either needing a layout decision baked into code.

**7. Vim-style relative line numbers** as an option, for `5j`-style motion. Pairs with counts in #1.

## Deliberately not taken

- **Modal editing** (russ). Right for an app whose main text entry is a URL; wrong for a player driven by single keys.
  We have a prompt line, which is the useful half.
- **Anything network** — streaming, OAuth, feed sync. §1 rules it out.
- **Image rendering.** §1 excludes album art in any form.
