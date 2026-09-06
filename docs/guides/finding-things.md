# Finding things

Three ways to narrow the library, and they differ in how they combine.

## Marks fold, so type what you can

`/loffler` finds Christian Löffler, and so does `/loeffler` — a mark can be typed around two ways and searching tries
both. Typographic punctuation folds too, so `don't` finds `don’t`. This holds for `:filter` and for sorting, which is
why he files under Lo rather than after every artist beginning Lz.

Typing a capital still asks for case to matter. It never asks for the mark back — those are different questions.

## The picker — find anything, go there

    <leader>ff   the whole library
    <leader>fg   the groups you have made
    escape       back to where you were
    enter        go to it

One input, two languages. Anything that parses as a filter **narrows** the candidates; whatever is left over is a
**fuzzy needle**, ranked over the row as it is drawn:

    genre="*electron*" burial
    └──── narrows ─────┘ └───┘ ranks

So `burial` alone is a fuzzy search, `year>=2015` alone is a plain filter, and the two together narrow before they rank.
**The title says which half is which** — that is why it is safe to split them without a separator, and if a half-typed
filter cannot parse the title says that instead of quietly ranking it as text.

`enter` **goes** to the row rather than playing it, the same way `gp` and `x` are different keys. `<leader>fg` picks a
collection or a saved view instead, and shows which kind each is.

This is not what `/` is for. `/` moves the cursor inside the list you are looking at; the picker searches everything and
takes you there.

## `/` — search as you type

    /            search forwards
    ?            backwards
    n  N         the next match, and the previous
    escape       stop painting the matches

It works anywhere with rows and a cursor: the library tree, a flat list, a playlist, the queue, and the info panel. The
guides are prose and scroll instead.

**`/` searches the whole view, not only what is open.** A closed fold still holds its rows, exactly as a closed fold in
vim still holds its lines — so `/malibu` with every artist shut finds the album and opens the way to it. Abandoning with
`escape` closes again whatever the search opened: the search never happened, and that includes what it revealed.

**The view is the scope.** `/` looks at whatever `:view` has left in front of you, so narrowing first is how you search
within something — `:view in="Dance"` then `/burial` searches the crate. There is no separate "search only these",
because there is already a better way to say it.

The list follows as you type — vim's `incsearch` — and every match is painted where it matched. **Nothing is committed
until `Enter`**: `escape` puts the cursor back exactly where the search started, so an abandoned search is not an
accidental jump. Each keystroke refines one search from that origin rather than starting a new one from wherever the
last landed, which is why backspacing walks back precisely where typing went.

**`Enter` accepts the match you are looking at**, it does not move on to the next one. The list already followed you
there while you typed; `n` is how you go to the one after it.

`escape` afterwards stops the painting without losing the search — vim's `:nohlsearch` — so `n` still works.

The match under the cursor is not painted, because the cursor line is painted end to end and would cover it. The other
matches on screen show where else the word appears.

**What `/` looks at depends on where the words are.** In a flat list — the sorted view, a playlist, the queue — a row is
the only place its artist and album appear, so all three are searched. In the tree they have rows of their own, so a
track is searched by its title alone: otherwise `/malibu` would match the album _and_ each of its ten tracks, and `n`
would walk nine rows that matched for a reason nothing on them shows.

One consequence worth having: in the tree every match is on a row that shows it, so a match is always painted.

## Filters — a question

    :filter genre="jazz"|genre="blues"
    :filter artist="*ferrell*"&year>2020
    :filter rating>=4&days_since_played>365
    :filter                                  clears it

`&` `|` `!` and parentheses. `=` and `!=` for text; `<` `<=` `>` `>=` for numbers as well. A trailing `?` asks whether a
field is there at all — `genre?` has one, `!genre?` has none.

Strings match by **glob**, and `*` is the only metacharacter:

    artist="Ferrell"      exactly that
    artist="Ferrell*"     starts with
    artist="*Ferrell*"    contains

Matching is smart-case: lowercase is insensitive, and a capital anywhere makes it exact. `?` is a literal question mark,
because titles have those and nobody ever wanted "exactly one character" from a music search.

`any="*live*"` searches every text field at once.

## Albums, singles, EPs — and what a release also is

    :filter release_type="single"        the singles
    :filter release_type="compilation"   the compilations
    :filter !release_type="album"        everything that is not a plain album

A release has **one** kind — album, single, EP, broadcast — and then any number of other things it also is: a
compilation, live, a soundtrack, a remix album, a DJ mix. Both answer to `release_type`, because somebody asking for
their compilations means an `album/compilation` too, and there are 1,084 of those here.

Twenty-eight records in this library are three things at once — `album/compilation/live`, `album/compilation/dj-mix` —
and each answers to all three.

**A file whose tags say nothing is an album.** Almost everything is, so that is the cheapest wrong answer available; it
also means `release_type="album"` is "an album, or nothing said", which is not the same as a claim that somebody made.

## How much of a record you have

    :filter release_held="partial"     records you have some of
    :filter release_held="complete"    records you have all of
    :filter release_held="unknown"     records nothing counted

The tags say how long a record is — `3/12` is track three of twelve — so the library can tell short from whole. Of the
6,485 files here: 5,577 sit on records held complete, 468 on records held in part, and 440 on records nothing counted.

**Unknown is a real answer and not a hedge.** A file whose tags carry no total, or a record nothing has identified,
cannot be measured — and calling that "complete" would be the one wrong answer worth avoiding, because it is the one
that stops you noticing what is missing.

It counts discs too. A two-disc set whose second disc you have never seen has no disc to be short of; the pressing's own
disc count is what catches it, which is why _All Eyez on Me_ reads partial at 19 tracks rather than complete at 19 of
the 19 it can see.

## Catalogs — which bodies of music

Catalogs are named and persistent, and they **union**: turn on jazz and blues and you get both. A filter then
**intersects** whatever they add up to.

    :view saved="dance"        only this one
    :view +saved="general"     add it
    :view                      everything

`6` — or `<leader>fg` — opens a picker over what you have saved, and `enter` makes it the view.

## Grouping by a level of the genre hierarchy

    :view levels genre,album      by genre, then album
    :view levels genre@1,genre@2  by the broad term, then the specific one

`genre@1` is the broadest ancestor, `genre@2` the one under it. That is what the `Broad; Style` tag convention was
imitating in a string: with the hierarchy in the library, `Techno` and `House` collapse into one `Electronic` node at
level 1 and stay two at level 2.

A genre with no parent stands for itself rather than dropping out of the tree, so a half-curated hierarchy still draws
every row.

**If the axis has no hierarchy yet, it says so** rather than drawing the same tree twice:

    genre has no hierarchy yet — nothing to group a level of

The hierarchy is curated outside the player — `gmuse-tools genre hierarchy`, then `gmuse shelf apply`. On this library
239 genre terms collapse to 27 at level 1, and the ones that stay are the ones nobody has decided: `Gospel` is Folk and
Blues and Funk depending on the record.

## Counts under a multi-valued level do not sum

A track with three genres is under three nodes. So the count in the status line reads as _groups a track is under_, and
never as a partition of the library:

    levels: genre,album (14 groups, a track may be in several — they do not sum)

This is not a defect to be fixed by picking one genre. Picking one is how `label` ended up inside `albumartist`.

## Versions — what a recording was made out of

    derived[]                       it came out of something
    derived[kind=remix]             it is a remix of something
    derived[depth<=2]               a remix of a remix, and no further
    derived[kind=edit & depth=1]    an edit, directly
    work_derived[kind=arrangement]  the other traversal, one level up

The bracket is **correlated**, the same as `genre[…]`: one edge has to satisfy every clause. So a track that is a remix
one hop back and an edit two hops back does not match `derived[kind=remix & depth=2]` — there is no single hop that is
both.

The walk is bounded at 16 hops whatever you ask. Edges union across machines, so two of them can produce a loop neither
one authored, and a render has to answer rather than hang.

## Three tiers, one language

Simple things inline; composed things in an editor.

    :view genre="techno"    one line
    :view                   take the composition apart
    :edit                   open the whole query in $EDITOR

`:edit` writes the composed view out one term to a line, opens it in `$VISUAL` or `$EDITOR`, and reads it back when you
save. It is **the same text** the `:` line takes — not a save format, not a form. Two things the one-line version has
nowhere to put:

    # the ones I actually reach for
    in="Dance"
    # but not the seasonal ones
    - genre="*christmas*"
    & year>=2015

A newline is whitespace, so a long query gets a line each; `#` starts a comment, and comments survive the round trip.
Every term still carries its sign — a line without one is an error, because guessing whether you meant `+` or `&` is
exactly the kind of guess that makes a view mean something you did not say.

If a line does not parse, **nothing is applied** and the status line names the line:

    line 4: "jazz" on its own names nothing — a collection is `in="jazz"`, a saved view is `saved="jazz"`, …

Set `$EDITOR` first. Nothing is guessed if neither `$VISUAL` nor `$EDITOR` is set — being dropped into an editor you did
not choose is worse than being told to choose one.

## Parentheses — the thing the line cannot say

`:view` builds a **left fold**, so `A + B & C` always means `(A + B) & C`. To say the other one, group it:

    in="Dance" + (saved="mornings" & year>=2015)

A group is **one term**: it drops as a whole, and the terms inside it are not separately droppable — a stack of controls
over something that is no longer a stack would be a control that lies. That is the only thing `:edit` can make that the
line cannot, and it round-trips exactly.

## Saying how many

A row carries **many** genres, so a question about them needs to say how many have to match:

    any(genre)="rock"     some genre is rock
    all(genre)="rock"     every one of them is

Both readings were already here and neither could be written. The bare form is the one you almost always want, and it
differs by operator:

    genre="rock"     is any(genre)="rock"     — some
    genre!="rock"    is all(genre)!="rock"    — none

That asymmetry is deliberate. A file tagged `Rock; Blues Rock` **is** rock, and every one of its genres is **not** jazz
— making both default to "some" would have made `genre!="rock"` true of that file, which is not what anyone means.

`all` is not true of nothing. A track with no genre fails `all(genre)="rock"` rather than passing it on a technicality;
`!genre?` is how you ask for the untagged, and it is a different question.

## Asking about who said so

A genre on a row is a **claim**: somebody said it, and was more or less sure. A bracket asks about one claim rather than
about the row:

    genre[basis=discogs]                          Discogs said something
    genre[basis=discogs & confidence!=confirmed]  …and was not sure — the review queue

    basis         tag  inferred  musicbrainz  discogs  manual
    confidence    provisional  matched  confirmed
    label         the concept itself

**The brackets are what correlate.** Written as two separate questions — "Discogs said something" and "something is
unsure" — a row where Discogs was _confirmed_ and the tag was unsure would match, and that is a different set from the
one you meant. Inside one bracket, both have to be true of the **same claim**.

`|` inside a bracket is refused, and nothing is lost by it: `genre[basis=discogs] | genre[basis=tag]` says everything
`genre[a | b]` would. Only `&` needs the bracket, because only `&` fails to distribute.

## Asking about a whole branch

A genre hierarchy exists so you can ask for a _kind_ of music without naming every term under it:

    genre="electronic"     tagged exactly that
    genre*="electronic"    that, or anything narrower, at any depth
    genre+="electronic"    strictly narrower — techno yes, electronic no

`*` is zero-or-more hops and `+` is one-or-more, as in SPARQL and every regex you have met. One meaning per character.

Put the marker on the field, inside a quantifier if there is one — `any(genre*)="electronic"`. The closure is about the
field; the quantifier is about how many of its values have to match.

A hierarchy is curated by hand, so a loop in it is a question of when rather than whether. Closure walks a bounded
distance and visits each concept once: a cycle stops it rather than hanging.

## History, ratings, and what you have filed

Five fields come from what you have done rather than from tags:

    play_count           0 for never
    days_since_played    missing if never
    days_since_added
    rating               1–5, or missing
    collected            how many collections hold it, or missing

    :filter play_count=0                 never heard
    :filter days_since_added<30          new this month
    :filter !rating?                     not judged yet
    :filter !collected?                  in no crate
    :filter collected>1                  in two, which is allowed

Dates read as _days since_ so a question sounds like English and the language needs no date literals. Note that "never
played" has **no** `days_since_played` at all — `!days_since_played?` finds exactly those — while `play_count` is `0`,
because never is an answer.

`collected` follows the same rule as `rating`: in nothing is **missing**, not nought, because a track nobody has filed
is different from one decided to belong to nothing. And it is a count rather than a flag, because collections overlap on
purpose — a pop song is in Dance when you are DJing with it and in General the rest of the time, and both are true.

A scan says what has landed and belongs to nothing, by the folder it landed in, and `gmuse shelf unfiled` turns that
into a proposal you read before applying. **The folder never files anything itself** — it is evidence somebody left, and
reading it once is all it is for.

    :rate 4      rate what is playing, or the row under the cursor
    :rate none   clear it
    :rate        what is it now

Ratings are 1 to 5. Nothing carries zero stars, so _unrated_ and _rated badly_ stay different questions.
