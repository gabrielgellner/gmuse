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

## Opening and closing

    l        open the row under the cursor
    h        close it
    Enter    same as l
    Z        close everything

`h` on something **already closed** closes its parent and moves there. So pressing `h` repeatedly walks you up and out —
from a track, to its album, to its artist — without needing a key for "collapse this level".

## Enter versus x

`Enter` and `l` **confirm**. On an artist or album that means open or close; on a track there is nothing left to open,
so it plays.

`x` plays whatever is under the cursor from anywhere, including from inside a closed album, where it starts at the top.

So `Enter` acts on structure until structure runs out. `x` always means sound.

## What the cursor covers

`a`, `e` and `x` act on everything _under_ the row, not just the row:

    on a track     that track
    on an album    its tracks, in order
    on an artist   everything by them

So gathering an album is one keypress on the album row — you do not have to open it first.
