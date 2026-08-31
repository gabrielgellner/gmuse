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
    o        open it if it is closed, close it if it is open

`h` on something **already closed** closes its parent and moves there. So pressing `h` repeatedly walks you up and out —
from a track, to its album, to its artist — without needing a key for "collapse this level".

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
reopening spills them back. `zC` closes the subtree with it.

## Enter versus play

`Enter` and `l` **confirm**. On an artist or album that means open or close; on a track there is nothing left to open,
so it plays.

`<leader>x` plays whatever is under the cursor from anywhere, including from inside a closed album, where it starts at
the top.

So `Enter` acts on structure until structure runs out, and `<leader>x` always means sound. The leader is there because
in this view your hands are looking for something, not driving the transport — reaching past it is the point.

## What the cursor covers

`a`, `e` and `<leader>x` act on everything _under_ the row, not just the row:

    on a track     that track
    on an album    its tracks, in order
    on an artist   everything by them

So gathering an album is one keypress on the album row — you do not have to open it first.
