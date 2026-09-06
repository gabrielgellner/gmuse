# The library tree

View 1 holds all three levels in one pane — artists, their albums, and the tracks of whichever albums you have open.

    ▾ Burial
      ▾ Untrue
          1  Archangel                3:52
          2  Near Dark                3:56
      ▸ Tunes 2011–2019
    ▾ Sierra Ferrell
      ▸ Long Time Coming

Several artists can be open at once, and several of _their_ albums. That is the point: you can put two albums from
opposite ends of the library on screen together and move between them with `j` and `k`, without a pane or a filter.

## What it groups by is yours

Artist then album is a **default**, not the shape of the thing:

    :view levels genre,album     by genre
    :view levels year,album      by year
    :view levels label,album     by imprint
    :view levels                 back to artist,album

The rows are the same either way — this is how the set is _drawn_, not which tracks are in it, so regrouping never
changes what you are looking at.

**A track can carry two genres, and grouping by one puts it under both.** That is what you want — a techno track belongs
under `techno` and under `electronic` — and it means the numbers are careful about it: a node counts distinct tracks,
enqueueing one queues each track once, and going to what is playing highlights every place it sits.

## Opening and closing

    l        open the row under the cursor
    h        close it
    Enter    same as l
    o        open it if it is closed, close it if it is open

`h` on something **already closed** closes its parent and moves there. So pressing `h` repeatedly walks you up and out —
from a track, to its album, to its artist — without needing a key for "collapse this level".

## Records you have part of

An album row says how much of it is here, when that is less than all of it:

    ▾ Beirut
      ▸ Gulag Orkestar
      ▸ Lon Gisland          4 of 5
    ▾ The Bug
        London Zoo  1 of 12 — 03 Poison Dart

**Only when it is short.** `12 of 12` would be on 5,577 of the 6,485 files here, which is a number that stops meaning
anything. A record nothing counted says nothing either — the tags carry the length of a record, and 440 files here have
tags that do not.

It counts discs. A two-disc set whose second disc you have never seen is short by a whole disc, and the row says so
rather than reading complete against the one disc it can see.

An album that spans two editions — some tracks off the original, some off the reissue — says nothing, because the two
have different lengths and there is no single answer to print.

## A group of one is not a level

An album holding one track does not get a row. The track sits where the album row would have been:

    ▾ Burial
        Untrue
          Archangel
          Shell of Light
    ▾ The Bug
        London Zoo — 01 Poison Dart

A row you open to reveal exactly one thing costs a keystroke and tells you nothing. In a library of albums this is rare
— 3 of 495 here. In a library of singles it is nearly everything, and it is the difference between two rows and three
for every file you own.

**The album still says its name when the name says something.** One track of _London Zoo_ is not the same as a single,
so the row carries it. A release named after its own track — `Fake I.D` holding `Fake I.D (Original Mix)` — does not,
because it is already written there.

`/` finds either one. What is drawn is what is searched, so a match always shows you where it matched.

## Folds, by the level

These are vim's fold keys, and they do here what they do there.

    z o      open this row
    z c      close it
    z a      toggle it
    z O      open this row and everything under it
    z C      close this row and everything under it
    z R      open every fold
    z M      close every fold
    z r      open one more level
    z m      close one level
    z x      close everything except what holds this row
    z t z z z b    put this row at the top / middle / bottom of the pane
    g p      go to the playing track

`zr` and `zm` are the two worth learning. From everything closed, one `zr` puts every artist's albums on screen; another
puts every track there. `zm` walks back out the same way.

That works because the levels here all mean the same thing — every row at the first level is an artist, every row at the
second is an album. A file explorer cannot offer this, because "three directories deep" is not a category. Here it is.

`zx` is the one to reach for when the tree has got away from you: it closes everything, then reopens just the folds you
were inside, so you keep your place and lose the mess. On an artist row it is simply `zM`.

`zC` matters more than it looks: closing an artist with `h` leaves the albums you had open still open underneath, so
reopening spills them back. `zC` closes the subtree with it. `zA` is the toggle of the two, the way `za` is the toggle
of `zo` and `zc`.

## Getting about without counting rows

With a few artists expanded the tree gets long, and `j` has to walk every track of every open album. `}` and `{` step to
the next row at the level you are on — artist to artist, or album to album — and `]]` and `[[` always mean artist,
whatever depth you are at.

    }  {         next / previous at this level
    ]]  [[       next / previous artist
    ][  []       the *end* of an artist rather than its start
    -            up to the album, or to the artist
    ]z           the last track of this album
    zj  zk       next fold's start / previous fold's end

`-` walks out one level at a time and pairs with `l` walking in. `]z` is how you get to the end of a long album without
`G` taking you out of it.

## Enter versus play

`Enter` and `l` **confirm**. On an artist or album that means open or close; on a track there is nothing left to open,
so it plays.

`<leader>x` plays whatever is under the cursor from anywhere, including from inside a closed album, where it starts at
the top.

So `Enter` acts on structure until structure runs out, and `<leader>x` always means sound. The leader is there because
in this view your hands are looking for something, not driving the transport — reaching past it is the point.

## What the cursor covers

    a    add to the open playlist
    e    add to the queue

Both act on everything _under_ the row, not just the row:

    on a track     that track
    on an album    its tracks, in order
    on an artist   everything by them

So gathering an album is one keypress on the album row — you do not have to open it first. `<leader>x` reaches the same
way, which is why playing an album is one key on its row too.

    A    add to the library

`A` is cmus's, and here it only tells you what to type instead: the library is the thing you are already looking at, so
there is nothing to add a track _to_. Growing it means scanning somewhere new — `:add <path>`.
