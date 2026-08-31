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

That is also why it cannot be edited by hand — an exception list bolted onto a rule is how these become unexplainable.

    :playlist save keepers    copy what it currently holds, editable
    :freeze                   detach it without writing anything
