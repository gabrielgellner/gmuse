# Rules that make their own lists

A smart playlist is a saved filter with an order. It is a **source** — it fills view 3 and leaves the library alone —
where a catalog _narrows_, hiding everything outside it. Same expression, two different jobs.

    [playlists.fresh]
    where = "days_since_added<30"
    sort = "-added"

    [playlists.neglected]
    where = "play_count=0"
    sort = "random"

    [playlists.best]
    where = 'rating>=4&genre="*jazz*"'
    sort = "-rating, artist, album, track"

`:playlist open fresh` fills view 3 from it. `open` takes a rule or a saved file without caring which — both are places
to play from.

## Saving one without leaving the player

A table in `config.toml` is one way to write a rule. The other is to compose the question with `:view`, then keep it:

    :view rating>=4                          ask it
    :playlist rule best                      keep the question, under a name
    :playlist rule best sort -rating, artist the same, with an order
    :playlist drop best                      delete it

`best` then opens with `:playlist open best` and lists in view 7 like any other rule, and it is just as live. It is kept
in the library rather than in the config, so it travels with your ratings and history.

Whatever `:view` can compose, a saved rule can say — `in="Jazz" &rating>=4` included, which a `where` line cannot. With
no `sort`, it plays in library order. Saving under the same name again rewrites the rule; that is how you change its
order.

**A name in `config.toml` is the config's.** Saving under one is refused, and so is dropping one: edit the file instead.

**`:playlist rule` keeps the question; `:freeze` keeps the answer.** Rate another track and `best` gains it. A frozen
copy only changes when you freeze it again.

## Sorting

Comma-separated, applied left to right, `-` for descending. The keys are the same field names the filter and format
languages use, plus `random`.

Numbers compare as numbers, so track 2 comes before track 10. A missing value sorts **last** in either direction — a
list whose first screen is blank looks broken rather than sorted.

One thing to know: **dates are spelled differently here.** A filter says `days_since_added`; a sort says `added`.

    where = "days_since_added<30"     added this month
    sort  = "-added"                  newest first

`days_since` reads well in a question and inverts in an order — ascending would mean newest first — so
`-days_since_added` written for "newest" would quietly give you the oldest. Each language takes the spelling that reads
in it and rejects the other by name.

## It is a live query

The rule is re-read whenever something it depends on moves. Rate a track and a `rating>=4` list gains it immediately;
play one and a `play_count=0` list drops it. Nothing to refresh.

That is also why there is no exception list — "everything the rule says, minus these four" is how these become
unexplainable. What you can do instead is **make the rule true**, which is the next section.

## Adding to a rule writes the fact, not an exception

    :add-view       add the selection to the open view

It does not staple a row onto a list. It writes the fact that makes the row a member, and the status line says which
fact before it does:

    assign genre techno to 3

The row you want to add is, by definition, not in the view — so the way in is the register, the same way a track crosses
between playlists:

    yy                      copy it, anywhere in the library — an artist row copies the artist
    :view in="Dance"        compose the view
    p                       put it in: the fact is written, and the row appears
    dd                      take it out, by retracting that fact; `p` puts it back

`dd` is the mirror of `p`, kind for kind. Out of a crate it retracts the entry. Out of `genre="techno"` it **rules the
genre out** — the imported claim stays in the log, shadowed by yours, which is what stops it coming back on the next
sync and what the info panel draws as _ruled out_. Out of `rating=4` it unrates, because leaving a rating cannot say
which other number was meant. With nothing composed there is nothing to put into or take out of: the library is a
projection, and `p` and `dd` say so.

Whether it is offered at all depends on **who authors the field**:

| the rule is about                  | what `:add-view` does                                           |
| ---------------------------------- | --------------------------------------------------------------- |
| `genre`, `rating`, a crate         | writes it, and names it                                         |
| `year`, `title`, `artist`, an mbid | refuses — these are read off the world, correct them on purpose |
| `duration`, `bitrate`, a path      | refuses — there is nothing anybody asserts about these          |

`year>=2015` translates perfectly well into "assert a year", which is exactly why it is refused: dropping a track into a
list must never rewrite a release date.

Three more refusals, each said out loud rather than doing nothing:

- **`genre="techno" | genre="house"`** — ambiguous. Add to one of them.
- **`rating>=4`** — names no single value. Four, five, or nothing in particular.
- **`genre*="electronic"`** — satisfied by anything narrower, so writing the broad one would be a guess.

  :freeze keepers keep what it holds now, under a name :playlist save keepers the same crossing, said the other way

## Freezing: a question becomes an answer you kept

A rule is a **question** and stays current. Freezing it keeps **what the answer was**, and that list stays put — you can
edit it, reorder it, delete rows.

    :freeze keepers      the first time: keeps what the rule holds now
    :freeze keepers      after that: re-runs the rule and says what changed

**It remembers what it was frozen from.** So `keepers` can tell you it came from `rating>=4`, and re-freezing is one
keystroke rather than retyping the question. The report is the drift — `keepers: +3 −1` — because the count is the
boring half.

Re-freezing re-runs **the recorded query**, not whatever view happens to be open. Refreshing a crate from something else
would quietly make it a different crate, which is the one thing recording the source exists to stop.

`:freeze <name>` works on the composed view too, so any question you can ask is one you can keep. What is **not** a
third kind of thing: a list that "reduces as you play it" is `in="keepers" & play_count=0`, and it is a question.
