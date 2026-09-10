# rmpc

<https://github.com/mierak/rmpc> — MPD client TUI, Rust + ratatui, 3.3k stars, actively developed. Self-described as
"heavily inspired by ncmpcpp and ranger/lf".

**The furthest thing here from our construction and the closest to our _surface_.** It is a client for a daemon it does
not own, so its whole model is the opposite of ours — it holds no library, decides no identity, and asks MPD what is
true. None of that is relevant. What is relevant is that it has spent 3.3k stars' worth of attention on the two
questions we are also asking: how a person drives a TUI by keyboard, and how much of the screen a config file gets to
decide. Read for §6 and the tree view; nothing else.

The comparison is unusually clean because we independently arrived at the same shape in three places, which makes the
places we did **not** worth reading closely.

---

## 1. Where we already agree, arrived at separately

Worth stating first, because it means the disagreements are informative rather than noise.

|                     | rmpc                              | gmuse                                   |
| ------------------- | --------------------------------- | --------------------------------------- |
| multi-key sequences | a trie, `gt`, `zz`, `<C-w>k`      | `Seq`, `g t`, `^f`                      |
| scoped bindings     | per input mode                    | per `Context`, falling back to `Common` |
| help                | built from the live binding table | `show_help`, built from `bind::sorted`  |
| a command line      | `:` → `CommandMode`               | `:` → 63 commands                       |
| bindings in config  | `keybinds: { global: {...} }`     | `bind common j down`                    |
| vim motions         | `gg`, `G`, `<C-d>`, `<C-u>`       | the same, plus counts                   |

Its `ShowHelp` is the same idea as ours down to the grouping by category. Its trie is our `Seq` lookup with `Prefix` /
`Exact` / `None`. We have which-key on a delay and it does not — that one is ours.

---

## 2. The context menu — the idea worth taking

`<C-x>` opens a modal built **for the row under the cursor**, composed of typed sections:

```rust
MenuModal::new(ctx)
    .width(80)
    .input_section(ctx, "New playlist", |sect| { … })
    .select_section(ctx, |sect| { for pl in playlists { sect.add_item(…) } … })
    .list_section(ctx, |sect| { sect.add_item("Cancel", …) })
    .build()
```

Four section kinds — `input`, `select`, `list`, `multi_action` — each with its own focus behaviour, stacked into one
modal, built fresh per invocation from what is actually possible here.

**The idea is already on this folder's list** — [README](README.md) §5, "popups as a first-class UI concept", from
spotify-player's actions menu, and the example given there is _"add this album to which playlist"_, which is the very
case rmpc's `select_section` renders. What rmpc adds is not the idea but the **construction**, and the construction is
the part that was missing: a menu assembled from typed sections rather than a list of strings.

Why it matters here: we have 63 commands and a binding table, which is excellent once you know it and offers nothing at
all when you do not. `:` requires knowing the command exists; which-key requires knowing the prefix. Neither answers
_"what can I do with this row"_, and that question gets asked constantly by anyone who has not memorised the manual —
including the person who wrote it, six months later.

It is also the natural surface for the things this library does that a player does not: **merge these two artists**,
**retire this heading**, **this is a moniker of that** (#174), **relate**, **rate**, **withdraw this credit**. Those are
verbs a person invokes rarely and precisely, which is exactly the profile a menu serves and a keybinding does not. A `:`
command for each is a command nobody will ever type twice.

The construction to copy is the section trait, not the modal: a menu is a stack of sections, each knowing how it takes
focus, so a menu for a track and a menu for an artist share everything but their content.

**Taken** — worth an issue. The dependency is that it wants the info panel's data (`info-panel.md`), because a menu for
a row and a panel about a row are answering the same question at different lengths.

---

## 3. `CopyToClipboard` — an action whose argument is a menu

```ron
"Y": CopyToClipboard(kind: Modal([
        ("Displayed value", (content: DisplayedValue, all: false)),
        ("Artist - Title",  (content: Metadata([Property(Artist), Text(" - "), Property(Title)]), all: false)),
     ])),
"y": CopyToClipboard(kind: Content(content: DisplayedValue, all: false)),
```

Two bindings, one action, and the difference is whether the argument is **fixed** or **asked for**. `y` copies the
obvious thing; `Y` opens a small menu of renderings.

Generalises past the clipboard: it is a way to have one verb with a common case bound tight and the rare cases reachable
without another keybinding. We have the opposite pressure — 63 commands and growing — and this is a shape that adds
capability without adding vocabulary.

**Taken as a pattern**, not as the clipboard feature. (Clipboard itself: we have none. Copying the path of the row under
the cursor is obviously useful and obviously cheap. Small issue.)

---

## 4. The property language — where theirs is ahead of ours

Ours is `format::Template`, sixteen fields, with `[ — {album}]` disappearing as a group when empty. Theirs:

```rust
struct Property<T> {
    kind: PropertyKindOrText<T>,   // Text | Property | Sticker | Group(Vec<Property>) | Transform
    style: Option<Style>,
    default: Option<Box<Property<T>>>,   // ← recursive
}
```

Three things there we do not have:

**A `default` that is itself a property**, recursively. So "album artist, else artist, else `Unknown`" is expressible in
the config rather than compiled in — which is precisely the fallback `file_under` hardcodes and `tree.rs` opens by
apologising for. Our version of that rule is a decision the model makes; theirs is a decision the theme makes. Ours is
right for filing and theirs is right for _display_, and we currently have no way to say the display one.

**A per-property `style`.** We style by `Role`, which is the right rule — the UI must never name a colour — but it means
a template cannot say "the year dimmer than the title" without a new `Role`. Their `style` on a property is the escape
hatch; ours would have to stay role-shaped to keep `theme.rs` the only place colours live. Worth thinking about; not
obviously worth taking.

**`Transform`**, of which `Replace { content, replacements: BTreeMap<String, Property> }` is the interesting half: map a
resolved value to another property. Renders a rating of `1` as `♥`, a `repeat` of `off` as nothing at all. That is a
codelist-to-glyph mapping living in the theme, which is where it belongs and where we have nowhere to put it.

**Partly taken.** The `default` chain is the strong one and it is small. `Transform::Replace` matters the moment the
info panel renders a `Source` or an `AliasKind` and wants a symbol rather than a word.

---

## 5. Declarative pane layout — read, and declined for now

Tabs are a recursive split tree in the config file:

```ron
(
    name: "Queue",
    pane: Split(direction: Horizontal, panes: [
        (size: "35%", pane: Split(direction: Vertical, panes: [
            (size: "100%", borders: "LEFT | RIGHT | TOP", border_symbols: Rounded, pane: Pane(AlbumArt)),
            (size: "7", borders: "ALL", border_title: [(kind: Text(" Lyrics "))], pane: Pane(Lyrics)),
        ])),
        (size: "65%", pane: Split(direction: Vertical, panes: [ … ])),
    ]),
)
```

Sizes as `"35%"` or `"7"`, borders as a flag set, inherited border symbols so a nested box's corners join its parent's.
It is genuinely impressive and it is the reason their example config is 11 KB.

**Declined, and the reason is a decision we have already made.** We have `Split`, `Only`, `OtherPane` as _commands_ —
layout is something you do, and `:view` is what persists. Making layout a config tree would mean two ways to arrive at
the same screen, and the config one cannot be undone with `u`. `tree-view-design.md`'s `ViewSpec` is our answer to
"which shape am I looking at" and it is a value in a view, not a section in a file.

What is worth stealing is smaller: **`border_title`** — a pane naming itself in its own border, as a property list so it
can render live data. We draw pane titles nowhere.

---

## 6. Small things, mostly free

**`zt` / `zz` / `zb`** — recentre the cursor at top / middle / bottom. We have `scrolloff` and no recentre; these are
three lines each and are muscle memory for anyone out of vim. **Take.**

**`Select` / `InvertSelection`** on `<Space>` / `<C-Space>`, with every subsequent action taking the selection. That is
#29, and it is worth noting they made the selection a property of the browser rather than a mode — no visual mode, no
state to leave.

**Config hot reload** (`enable_config_hot_reload`, a `config_watcher.rs` on the file). We reload nothing; editing a
binding means restarting. Given our defaults already go through the user's parser, this is cheaper for us than for them.
**Take.**

**`rmpc keybind <key>`** over the IPC socket — the remote injects a _key event_, not a command. We have
`--remote <CMD>`, which is better for scripting because a command is stable and a keybinding is a user's preference.
Theirs is better for one thing only: driving the UI in a demo or a test. Worth knowing; not obviously worth having.

**`on_song_change` / `on_exit`** hooks running an external command. We have a socket and no hooks. The daemon half of
their project (`rmpcd`, Lua plugins, last.fm scrobbling, playcount) is the same idea grown up, and is out of scope for
us — no async, no Lua, and `layers.md` says which direction the tools may call in.

**`scrolling_line.rs`** — marquee a line too long for its pane. Sixty lines. We truncate. Cosmetic, but it is the
difference between seeing a long title and not.

**`virtualized_table.rs`** — render only visible rows. We already care about this (#121, #123) and it is worth reading
theirs before we write ours.

**`max_fps`** on the draw loop. We redraw on event. Theirs is the right shape for a visualiser and ours is right for a
player that should idle at nothing; noted and not taken.

**`ignore_leading_the`** is our `smart_artist_sort` (#116), independently.
**`browser_song_sort: [Disc, Track, Artist, Title]`** as an ordered list of keys is our `sort.rs`. Both confirmations
rather than ideas.

---

## 7. What we have that they do not

Not vanity — it is the check that we are not about to adopt something into a hole we filled differently.

**which-key on a delay.** They have a trie and no prefix hint; a half-typed `<C-w>` shows nothing.

**A binding table whose defaults are parsed from rc-file text.** Ours cannot ship a default that the user's own parser
would reject, because it is the same parser. Theirs are Rust literals.

**Counts.** `3j`. Their actions take no count.

**`:view` as a saved, composable value** — narrowing, levels, and a stack. Their tabs are a config list of fixed
layouts.

**A model.** Every question here is "what does MPD say"; ours is "what does the library know, and who said so". That is
why nothing in their data layer is worth reading and everything in their input layer is.

---

## 8. Taken, in order of ratio

1. **Context menu with typed sections** (§2) — the biggest gap, and the natural home for the librarian verbs.
2. **`zt` / `zz` / `zb`** (§6) — trivial, and missed daily by anyone from vim.
3. **A recursive `default` on a template field** (§4) — small, and removes a hardcoded display fallback.
4. **Config hot reload** (§6) — cheaper for us than for them.
5. **An action whose argument can be a menu** (§3) — a way to stop growing the command vocabulary.
6. **`border_title`** (§5) — a pane that names itself.

Declined: declarative pane layout (§5), `max_fps` (§6), the plugin daemon (§6), per-property colour (§4, unless it can
stay role-shaped).
