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

The list follows as you type — vim's `incsearch` — and every match is painted where it matched. **Nothing is committed
until `Enter`**: `escape` puts the cursor back exactly where the search started, so an abandoned search is not an
accidental jump. Each keystroke refines one search from that origin rather than starting a new one from wherever the
last landed, which is why backspacing walks back precisely where typing went.

`escape` afterwards stops the painting without losing the search — vim's `:nohlsearch` — so `n` still works.

`/` searches the artist, the album and the title together, and paints whatever part of the match the row actually shows.
A row that matched on its artist is still where the cursor goes, even in a list that only displays titles.

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

## Catalogs — which bodies of music

Catalogs are named and persistent, and they **union**: turn on jazz and blues and you get both. A filter then
**intersects** whatever they add up to.

    :view saved="dance"        only this one
    :view +saved="general"     add it
    :view                      everything

`6` — or `<leader>fg` — opens a picker over what you have saved, and `enter` makes it the view.

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
