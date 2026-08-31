# Finding things

Three ways to narrow the library, and they differ in how they combine.

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
