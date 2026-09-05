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

    :catalog dance          only this one
    :catalog +general       add it
    :catalog all            everything

View 6 lists them, and `space` turns one on or off. That view is called _scope_ because it shows the whole answer — the
catalogs and the filter together.

## History and ratings

Four fields come from listening rather than from tags:

    play_count           0 for never
    days_since_played    missing if never
    days_since_added
    rating               1–5, or missing

    :filter play_count=0                 never heard
    :filter days_since_added<30          new this month
    :filter !rating?                     not judged yet

Dates read as _days since_ so a question sounds like English and the language needs no date literals. Note that "never
played" has **no** `days_since_played` at all — `!days_since_played?` finds exactly those — while `play_count` is `0`,
because never is an answer.

    :rate 4      rate what is playing, or the row under the cursor
    :rate none   clear it
    :rate        what is it now

Ratings are 1 to 5. Nothing carries zero stars, so _unrated_ and _rated badly_ stay different questions.
