# The view model: what a view is, and what was wearing one

**Status: a review of a proposal, and mostly a record of decisions already made.** It exists because the proposal it
reviews is a good one that arrives second: the distinction it draws is the distinction #97 already drew, and four of its
five keys are already bound to something.

The proposal's claim is that views are tabs, that tabs imply separate places, and that most views are projections of one
library — so the vim metaphor is wrong and should be buffers, a quickfix list, a preview window and tabpages in that
order.

**The frame is right and the premise has expired.** This document says which parts are live, which collide with settled
design, and what is actually left to build.

---

## 1. The cut was already made, and it took two views with it

The proposal's test is whether a thing holds different rows or merely draws the same rows differently. `View`'s own
doc-comment applies exactly that test and records what it removed:

- **`Sorted` was `Tree` drawn flat.** One set, two drawings, and `levels` is what says which — "a value wearing a tab".
  `2` still works and now means `levels: flat`.
- **`Filters` was a picker wearing a tab.** `<leader>fg` ranks over the whole set instead of listing one you scroll
  (#54, #119), and `6` opens it.

What survived is `Tree`, `Playlist`, `Queue`, `Guide`, `Info`: things that hold genuinely different rows, plus a guide
that holds none.

So **"most views are projections of one library" was true before #97 and is not true now.** The projections are the two
that were removed. Applying the test again to the five that are left yields nothing — which is the outcome that says the
test was already run, not that it was never worth running.

The four-category frame is still worth having as vocabulary, and §9 keeps it. What it does not buy is a re-cut.

---

## 2. The selection already exists, and it is called `Focus`

The proposal introduces `Selection` as a typed entity reference: `Work(WorkId)`, `Release(ReleaseId)`,
`Recording(RecordingId)`, and so on.

That type is `crate::model::focus::Focus`. Eight variants — `Recording`, `Encoding`, `ReleaseGroup`, `Release`,
`Artist`, `Concept`, `Collection`, `Work` — each with its own id in the fold, minted for #124 and the info panel. Its
module doc is itself about not coining a fifth name for this:

> [`Subject`] is what a _proposal_ is about… [`Described`] is the two things a _concept_ can be asserted of. Neither
> means "anything the panel can show"… Three near-synonyms in one codebase is how a reader stops trusting any of them.

`domain-model.md` §8 makes this a rule, and it is the cheapest rule in the project to break: the term that already
exists is invisible from outside the module that defines it.

**So there is no `Selection` to add.** The UI already holds a stack of these in `ui.focus`, and the question this
document is really asking is who else may read it — which is §3.

---

## 3. One global selection is a reversal, not an addition

> There is one global selection… Every view reads from and writes to this selection. Switching views never loses it.

This is the one substantive proposal in the document, and it argues against a decision without knowing it is there.
`Ui::execute`'s split branch records the opposite:

> **The other pane starts on something else**, because a split showing the same row twice is not what anybody splits
> for. The cursors are per view _kind_.

Per-view-kind cursors are also why `tree_scroll`, `playlist_scroll`, `queue_scroll` and `guides_scroll` are four fields
rather than one: "switching views must not scroll the view you came from."

Both positions are defensible and they are about different things, which is what makes this worth writing down rather
than deciding quickly:

| what is shared             | argument for                                                                           | argument against                                                               |
| -------------------------- | -------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------ |
| a **cursor** (row, scroll) | switching back finds where you were                                                    | two panes on one row is a wasted pane; row _n_ of the tree is not row _n_ here |
| a **focus** (the entity)   | the info panel and a projection agree about what "this" means without being told twice | a projection has no row for most focus kinds — a `Concept` is not in the tree  |

**These are not the same sharing, and conflating them is how the proposal reads as a reversal.** A shared cursor is what
#97 rejected. A shared focus is not a cursor at all, and nothing has rejected it — the info panel's stack is already the
only thing that holds one, and every other view reaches it by pushing.

The live question is therefore narrower than the proposal's: **when the tree cursor moves, should the focus follow it,
and should a projection relocate when the focus changes under it?** The second half is the expensive one. "The tree view
expands to the recording's release" is a tree walk plus a fold expansion on every focus change, on a structure that is
`Holding` behind a trait and may be a socket away (`tree-over-the-wire.md`).

---

## 4. Four of the five keys are already bound

The keymap summary reads as a set of proposals. Most of it is a set of re-proposals, and one of them is already recorded
as _rejected for the reason the proposal does not mention_.

| proposed                | status                                                       |
| ----------------------- | ------------------------------------------------------------ |
| `1`–`n` direct jump     | **rejected, and the count prefix is the reason** — see below |
| `<C-^>` alternate       | **already exists as `g<Tab>`** (`view-last`)                 |
| `K` metadata popup      | **already bound** — `bind common K info` opens view 8        |
| `]b` / `[b` cycle       | **free, and it fits** — `]` and `[` are already prefixes     |
| `gq`, `]q` / `[q`       | free as keys; the feature behind them is §6                  |
| `<leader>p` detail pane | free as a key; the feature behind it is §7                   |

`bind.rs` gives the reason for the first directly: views are "nvim tab style, **because a count prefix needs the
digits**". `1`–`9` start a count and `0` extends one, with tests. The second is not missing but deliberately singular —
"`view-last` _is_ 'where you were', so there is no second mechanism". The third settled `info-panel.md` §6's open
question in favour of vim's `K` some time ago. The fourth is the one genuinely free key, and `]]`, `[[`, `][`, `[]`,
`[z` and `]z` are the company it would keep.

**The count conflict is the one to read twice.** "Direct jump: bare `1`–`n` (cmus-style, one keystroke, stable
positions)" is a real ergonomic argument, and it costs `3j`, `10k` and `5]]`. CLAUDE.md's rule applies exactly here:
cmus is evidence, not a decision. Reopening it means arguing that counts are worth less than a keystroke on a view
switch, which is an argument nobody has made yet.

---

## 5. `:view <name>` is taken by the query language

> Picker: `:view <name>` with completion.

`:view` is the catalog algebra — one verb and three operators, #94 and #28:

```text
:view dance                  set it
:view +jazz                  or       — widen
:view &year>=2015            and      — narrow
:view -genre="*christmas*"   and not  — exclude
```

A projection picker on that name would shadow a settled mini-language whose whole point was that the operator is not
baked into the noun. The projection picker, if it is wanted at all, is `<leader>f`-prefixed like the other two (#54,
#119) — and with five projections and `]b`/`[b`, it is not obviously wanted.

---

## 6. The queue is not an index, and it is not the UI's

> Independent state: an ordered list of recording refs plus a current index.

`playback-programme.md` is settled design and says otherwise on both halves. **The librarian owns the ordered
programme**; the engine holds a window of two or three `(token, path)` pairs and no ordering policy. An entry is named
by **token, not by position** — #271 is exactly that, and `f2949d6` ("a queue-pane edit names its entry by token, not
the page it is on") is the commit that removed the index the proposal reintroduces.

So the queue panel is a rendering of a page of the programme, obtained by asking the librarian. It holds a cursor and a
version, not a list and an index.

What survives the correction, and is genuinely new:

- **The quickfix framing is apt** for the panel's _behaviour_ — a list you toggle, step through, and that does not
  change which projection is active.
- **`]q` / `[q`** as next/previous **entry in the panel** is fine. As next/previous **track** it duplicates `z` and `v`
  from `transport-keymap-spec.md` §7, which exist, are DJ-derived, and take a deck as an operand. Two vocabularies for
  one verb is what that spec was written to prevent.
- **Saved queues as `:colder` / `:cnewer`** is the interesting idea in the whole proposal, and it has to be a librarian
  query rather than a UI stack, for the reason above. Unowned.

### View 4 is redundant, and cannot go yet — found 2026-09-23

Once the panel exists, view 4 and the panel are two placements of one set of rows — which is what §1's test removed
`Sorted` and `Filters` for. So the expectation was that #274 would retire it.

**It cannot, and the reason is not scope.** Every queue _edit_ dispatches on `ui.view == View::Queue`: `dd` removing by
token, `p` putting the register back, the move commands, the undo key, `set_cursor`, `current_len`. The panel is drawn
unfocused, so none of that is reachable from it — retiring view 4 today would **delete** queue editing rather than move
it.

A panel that can hold focus _is_ per-window projections. So this is #275's to unlock, and it is the first concrete thing
that refactor buys rather than merely enables — worth knowing, because §8 could otherwise read as a refactor whose
payoff is only in principle.

---

## 7. The detail view is a view because it needs the room

> Never a navigation destination. Side pane that tracks the selection live; toggleable.

`View::Info` is a view rather than an overlay, and the reason is recorded where it is defined:

> A view rather than an overlay because it needs the room and it will hold a navigation stack (#127); `Guide` is the
> precedent for a content view with its own scroll. Anyone who wants the list beside it has `^w v`.

"Never a navigation destination" would delete that stack — `^o` is bound in the `info` context precisely because you
arrive at an artist from a recording, a release group or a concept, and there is no parent to climb to. A pane that only
tracks a cursor cannot be jumped back through, because nothing was jumped.

**A live-tracking pane and a navigable page are both wanted, and they are two surfaces.** The pane is what `<leader>p`
would toggle; the page is view 8. What makes them one feature rather than two is that the pane's rows are the page's
rows — the same `About`, drawn shorter. That is worth building and it does not require demoting anything.

### They turned out to be one surface — #273, 2026-09-23

Not two, and the paragraph above is what was wrong with it. #275 gave a window its own view and #276 made the focus
follow the cursor; between them, a window showing view 8 beside a list **is** a live-tracking pane. So `<leader>i` is
one keystroke for an arrangement, and there is no second renderer to keep in step — the worry this section had was
answered by removing its subject rather than by being careful about it.

`<leader>i` rather than `<leader>p`, and `info-window` rather than `detail-pane`: `<leader>P` is already cue-preview,
and "detail" and "pane" were the proposal's words for things this codebase calls `info` and — since #275 — `window`.

What the half-width pane **did** expose is that a row's last column is the one that falls off, and the last column is
where a claim's source lives. `artist="Anderson .Paak"` clipped to `artist="Anderson` is a plausible query rather than a
visibly incomplete one. That inverts this document's own rule, it is now the ordinary case rather than a corner, and it
was #282 — fixed by budgeting the columns from the panel's width, with **the tail reserved and the earlier columns
giving way**, elided with `…` so you can see when something did not fit. Wide is byte-for-byte what it was, so view 8
full-screen is untouched.

---

## 8. Splits are built; window-local projections are not

> Future: Splits. With window-local projections over the shared selection, splits fall out naturally.

Splits ship. `Split`, `Axis`, `^w s`, `^w v`, `^w w`, `^w o` (#97), and every split in the UI is side by side — which is
what makes `body_height` one number for every pane.

What makes them cheap today is exactly what the proposal would change: **`ui.view` _is_ the focused pane**, so "every
other match in this file needs no notion of a split: the keys already go where they should." Window-local projections
mean `ui.view` becomes `windows[focused].view`, and every one of those matches acquires a notion of a window.

That is a real refactor with a real payoff, and it is the thing in this proposal that is neither built nor decided
against. It is also the prerequisite for §3's shared focus being worth anything: two panes that cannot hold different
projections have nothing to share a focus _between_.

### Built — 2026-09-23, #275

`Windows` owns the panes; `Window` owns `view`, `levels` and `previous`. `ui.view` and `ui.levels` are accessors onto
the focused one, so the 54 production sites that read them still read the focused window and did not have to learn
anything.

Three things fell out that the plan did not predict:

- **`Pane` was taken.** `tree::Pane` already means which _half of one view_ has the keys — the list or the tracks. The
  windows a body divides into are `Window`, vim's word, so the two ideas keep their own (`domain-model.md` §8).
- **`^w w` used to shuffle the screen.** The focused pane was always drawn first, so swapping focus swapped which view
  was where. With windows in a list, focus moves and the windows stay — which is what `^w w` does in vim, and was not a
  decision anyone made before.
- **The shared projection was keyed to the focused window**, so grouped-beside-flat worked or broke depending on which
  half your keys were in: a flat window asks for no grouping, and rebuilding for it left the other window drawing a tree
  with nothing to group by. `Windows::grouping` builds for the first window that wants a grouping instead. **Two windows
  grouped differently still share one projection** — that needs a projection each, and is #279.

What is _not_ done is retiring view 4 (§6), which needs the panel to hold focus rather than a window to hold a view.

---

## 9. The milestone: one focus, several windows

GitLab milestone 15, and the six issues it holds.

In order, because each is usable before the next exists — and the order is not a preference. Steps 1–3 are independent
and shippable in any order; **step 5 cannot be built before step 4**, because two panes that cannot hold different
projections have nothing to share a focus between.

1. **`]b` / `[b` cycle the projection** — #272. Two bindings, no new state. Free keys on a prefix that already carries
   `]]`, `[[`, `][`, `[]`, `[z` and `]z`.
2. **A detail pane** — #273 — toggled by `<leader>p`, drawing the same `About` as view 8 with fewer rows. §7 — the pane
   and the page are two surfaces over one model, not a demotion of the view.
3. **The queue panel is a page of the programme** — #274. `gq` toggles it; it holds a cursor and a version, not a list
   and an index. §6.
4. **`ui.view` becomes per-window** — #275. `windows[focused].view`, and every match that today needs no notion of a
   split acquires one. §8. The refactor the rest waits on — **including retiring view 4**, see §6.
5. **One focus, shared across windows** — #276 for the cheap direction, done: the panel follows the cursor. #281 for the
   expensive one: a projection relocating to the focus. The focus only — never the cursor, which #97 decided. §3.
6. **A projection picker** — #277 — under `<leader>f` with the other two (#54, #119). Speculative on purpose: five
   projections do not need one, and the set is expected to grow.

**`1`–`n` as a direct jump is not in this milestone.** It is #278, filed on its own and left low priority, because
reopening it means arguing that counts are worth less than one keystroke — and that argument has to be made before it
can be weighed, not after something is bound. §4.

The four-category vocabulary — projection, queue, detail, layout — is worth keeping as a way of asking "what is this
surface", even though applying it today re-derives the current set. **Layouts are the one category with no
implementation and no objection**, and the proposal's sharpest line is the one it spends least space on: that named
panel presets are the only place a tab is the right metaphor.

---

## 10. Decided — 2026-09-23

The four questions this document opened with, and the answers. Two of them commit to being wrong in a recoverable
direction, which is the reason they are recorded rather than argued.

**A focus follows a cursor, and a projection relocates to a focus — both directions, until one of them hurts.** The
asymmetry in §3 is real: a cursor moving is a field write, and a focus moving means a tree walk and a fold expansion,
possibly over a socket. The decision is to build both and **learn by doing**, with **one-way as the fall-back** rather
than the starting point. What makes this safe is that the fall-back is a subtraction: dropping the expensive direction
leaves a working feature, so being wrong costs a revert and not a redesign.

**Half of this is built — #276, 2026-09-23.** The cheap direction is done: the focus follows the focused window's
cursor, so a panel beside a list is live. Two things kept it cheap. It resolves **after a key rather than per frame**,
and **only when a window is actually showing the panel** — with no panel on screen it is two comparisons, so `j` pays
nothing for a surface nobody is looking at. A cursor move **replaces** the path rather than pushing, because it is a new
question; that is what `K` has always meant, and moving the cursor is the same act without the keystroke.

The expensive direction — a projection relocating to a focus — is **not built**, and is #281. The nearest-ancestor rule
below is what it will need, and #281 is where both of these decisions are either confirmed or spent: they were made
before anyone had felt the cost, which was deliberate, and that is exactly what makes the measurement the point.

**A projection that cannot show the focus goes to the nearest ancestor it can.** A `Concept` has no row in the tree, so
the tree goes to the nearest thing it does hold. This is the same bargain a diff view or a fold already makes — you land
near the thing rather than on it, and you can see that you did. "No movement" was the alternative and it is the one that
loses information invisibly, which is what rules it out.

**`1`–`n` is deferred, not rejected.** #278, filed separately and low priority. §4 records the cost; nobody has yet
argued it is worth paying.

**The projection picker gets built and may get removed.** At five projections it is not needed. The set is expected to
grow, the key is free, and a picker that feels wrong is one deletion — so the cheap experiment is to have it.

Built as `<leader>fv` (#277), and the case against it was stronger than §5 recorded: **the tab strip is already the
picker's own display**, permanently on screen, over items reachable in one keystroke. A picker whose entire content is
always visible is a second `gt`.

It earns its place on one row. `library, flat` — views 1 and 2 are one library drawn two ways, the strip has a single
tab for both, and **no number key reaches the flat one**: it is `:view 2` if you know and unreachable if you do not. The
rows go through `Command::View`, which is where a digit's other meaning lives, so the picker cannot drift from the `:`
line. That row, not the picker, is what would be missed if this were deleted — which is the shape of the argument for
keeping it, and an honest reason it might still go.

---

## Related

- [`music-player-tui.md`](music-player-tui.md) §6 — the command layer, `Context`, and where bindings live.
- [`transport-keymap-spec.md`](transport-keymap-spec.md) §7 — the transport verbs, and why `z`/`v` are next and
  previous.
- [`playback-programme.md`](playback-programme.md) — who owns the queue, and why an entry is a token.
- [`info-panel.md`](info-panel.md) — the detail surface, its stack, and the `K` question §4 reports as settled.
- [`tree-over-the-wire.md`](tree-over-the-wire.md) — why relocating a projection is not free.
