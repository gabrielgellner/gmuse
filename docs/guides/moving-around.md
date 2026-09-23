# Moving around

Views are switched like nvim tabs. `gt` and `gT` step; `3gt` jumps straight to view 3; `g<Tab>` returns to the one you
were on before.

`]b` and `[b` step as well, if brackets are what your hands reach for — the same shape as `]]` and `[z`, on the prefix
those already live on. **The count is the one difference between the two spellings**: `3gt` is view 3, while `3]b` is
three views along from here. Both of those are vim, which is why neither is wrong and why it is worth knowing which you
are typing.

    1 library    3 playlist   4 queue      7 guide

Three of them hold rows, and the guide holds prose. `2` and `6` still work and are **not views**:

    2   the library, drawn flat — the same rows, ungrouped
    6   the picker over your saved views and collections

That is the difference the numbers used to hide. `2` was never a different set from `1`, only a different drawing, so it
is `:view levels flat` — see [the library tree](the-library-tree.md). And `6` was a list you scrolled; it is a fuzzy
picker now, the same one `<leader>fg` opens.

## The queue, without leaving where you are

`gq` shows the programme in a panel along the bottom, and hides it again. **It is not a view**, which is the point: the
list you were on stays where it is, so you can dig through the library and watch what you are collecting at the same
time.

    gq       show or hide the panel
    ]q       down a row in the programme
    [q       up a row

`]q` and `[q` reach into the panel from whatever you are looking at, and they **move a cursor — they do not play
anything**. Next and previous track are `z` and `v`, in the transport row, and they stay that whether the panel is open
or shut.

The panel is as tall as the programme needs, up to a point, and never more than half the screen. It is the same rows
view `4` shows, drawn small — one cursor between them, so moving in one moves in the other.

## Two panes

`g<Tab>` moves _between_ two lists. A split shows both — for digging in one while collecting into the other.

    ^w v     side by side
    ^w s     one above the other
    ^w w     move the focus
    ^w o     back to one pane

The second window starts on wherever you were before, which is what `g<Tab>` would have taken you to — so `g<Tab>` then
`^wv` puts the two lists you have been going between on screen together.

Keys go to the **focused** window, and only that one is highlighted. `^w w` moves the keys; the windows stay where they
are. Splitting the same view twice is not offered: two windows on one list is what `g<Tab>` already gives you, and the
cursors are shared per list rather than per window, so both halves would move together.

**The panel follows the cursor.** `<leader>i` puts the info panel beside the list and takes it away again — the keys
stay in the list, and moving the cursor changes what the panel is about. You do not press anything to update it. `K`
while it is already open asks about the row you are on rather than taking the window the list is in.

It is not a separate thing from view `8`: same panel, same rows, just arranged beside the list instead of filling the
screen. So `^w w` into it and `^o` still walks back the way you came.

Moving the cursor is a _new_ question, so it replaces where you had navigated to: if you followed links into the panel
and then moved in the list, `^o` has nothing to go back to. Nothing follows anything while the panel is off screen.

**Each window groups the tree its own way.** `:view 1` on one side and `:view 2` on the other gives you the albums to
open on one half and the flat track list on the other, of the same library. Two windows grouped _differently_ — by
artist on one side and genre on the other — is not there yet: they will both show the first one's grouping.

`5` is unbound. It was a file browser, and gmuse navigates the library rather than a filesystem.

Within any of them the motions are vim's. `j` and `k` move, and take a count — `3j` goes down three. `gg` and `G` go to
the top and bottom, and `12G` goes to row 12. The arrow keys — `up`, `down`, `left`, `right` — and `home`, `end`,
`pageup` and `pagedown` do what you would expect, for the times your hands are not on the home row.

    ^d      half a screen down, recentred
    ^u      half a screen up, recentred
    ^f      a screen down
    ^b      a screen up

`^d` and `^u` are the pair to learn. Half a screen is the distance at which you can still see where you came from, and
they recentre afterwards, so the cursor stays pinned mid-screen and the text slides past it rather than the cursor
drifting to an edge and staying there. `^f` and `^b` keep two rows of the old screen as an anchor, exactly as vim does.

All four measure the pane you are actually in, so they move further on a tall terminal and less in a split view. They
work in the guides too, where the whole right-hand pane scrolls.

`/` searches forwards and `?` backwards; `n` and `N` repeat.

`gp` goes to whatever is playing, opening any folds that were hiding it.

## Moving by structure

`j` and `k` count rows. These move by what the list is made of, which is usually what you meant.

    }            next row at this level
    {            previous one
    ]]           next artist
    [[           previous artist
    ][           end of this artist, then the next one's
    []           end of the previous artist
    -            up to what this is filed under
    ]z           last row of this group
    zj           next row that can be opened
    zk           end of the previous fold

`}` is the one to learn. With an artist expanded, `j` walks every track; `}` steps to the next artist and ignores the
two hundred rows in between. On an album row it steps album to album, because "this level" is wherever the cursor
already is.

They mean the same thing in a flat list, where the album dividers are the structure: `}` is the next album, `]]` the
next artist, `-` the top of the album you are in and `]z` its last track. One idea rather than a second set of keys for
view 2.

`-` is netrw's "up a directory". `[z` does the same thing, which is what vim calls it.

`]]` and `][` are two halves of one idea, and vim spells them the same way: `]]` lands on where a section _starts_ and
`][` on where it _ends_. Leaving an artist from the bottom is what you want when the next thing you do is `j` into
whatever follows; leaving from the top is what you want when you are scanning. `[[` and `[]` are the same pair going
backwards.

`zj` and `zk` are **not** each other's undo, and that is vim's doing rather than an oversight — `zj` goes to the next
fold's _start_ and `zk` to the previous fold's _end_. Read as a pair they walk you through the gaps between groups
rather than through the groups themselves. A closed fold counts as one fold, so with everything collapsed `zk` simply
steps to the previous row.

## Where you are on screen

    zt      put this row at the top of the pane
    zz      at the middle
    zb      at the bottom

These move the _view_, not the cursor — the row you are on stays the row you are on. Lists draw `~` past their last row
exactly as vim does, which is what lets `zt` work on the final track instead of being ignored.

## The leader

`space` is the leader. It is not bound to anything on its own, which is what lets it start a chord.

    <leader>?    the keys that work in this view
    <leader>u    rescan the library

`<leader>?` is worth pressing in each view — it lists that view's own bindings as well as the common ones, which is a
different question from the `Index` guide's list of every binding everywhere.

**Start any chord and it tells you the rest.** Press `z`, or `g`, or the leader on its own, and a panel lists every key
that could come next and what each one does. There is no delay to wait through: a key bound on its own fires immediately
here, so there is never anything to wait and see about.

Bind your own in the config, where `<leader>` stands for whatever `leader` says:

    [keys.common]
    "<leader> p" = "playlist list"

A **list** binds several commands to one key, run in order — the one thing the flat rc file could not express:

    [keys.library]
    o = ["add-queue", "down"]

A count goes to the first of them, so `3o` there queues three tracks and then moves down once. `:bind` still takes
exactly one command: there is no separator in the command language, because one would collide with the free-form
arguments that `add`, `view` and the filters carry.

## Transport lives behind the leader

    <leader>p    play what the cursor is on
    <leader>z    previous
    <leader>x    play/pause
    <leader>c    cue — back to the reference, and wait there
    <leader>v    next
    <leader>P    preview from the reference while held
    <leader>+    louder, 3 dB a step
    <leader>-    quieter
    <leader>>    seek a minute on
    <leader><    seek a minute back
    right        seek ten seconds on
    left         ten seconds back

The prefix is deliberate. Navigating is what your hands are doing in these views, and reaching for the transport is
stepping outside that — so it costs a key, and the bare letters go to the folds and motions that browsing wants.

The letters read as four directions rather than four verbs: backward, run, reference, forward. That matters twice over —
there is one alphabet to learn rather than two, because in transport mode the same letters are bare, so `x` and
`<leader>x` are one play/pause reached from two postures; and each letter means the same _kind_ of thing whatever the
mode is, which is what lets the row grow later without moving. `b` is deliberately unbound.

`c` is cue, and it is the one letter that is not Winamp's. Pressed while something is playing it returns to the
reference point and waits there; pressed while paused it _sets_ the reference where the playhead is. On a track you have
never marked, the reference is the top — so cue is "back to the start", which is what stop used to be for. There is no
stop: `v` is next.

`P` previews: it plays from the reference for as long as you hold it, and snaps back when you let go. Cue marks the
spot, `P` listens to it, and nothing moves.

**Whether holding works depends on your terminal.** Reporting a key _release_ needs the kitty keyboard protocol — kitty,
WezTerm, Ghostty and foot have it; macOS Terminal.app does not. Where it is missing the same key latches instead: press
to start the preview, press again to snap back. The status line says which you got, `[preview]` or `[preview latched]`,
so a hold that does not behave like a hold explains itself rather than looking broken.

## Transport mode

`i` enters it and `escape` leaves, as in vim. While you are in it the status line says `-- TRANSPORT --`, and the
keyboard means playback rather than navigation.

    x            play what the cursor is on
    c            pause
    v            stop
    b            next
    z            previous
    space        pause, the key every other player uses
    h            seek ten seconds back
    l            ten seconds on
    H            a minute back
    L            a minute on
    j            quieter
    k            louder
    +            louder
    =            louder, for the key without the shift
    -            quieter
    50%          jump half way through the track
    ?            what works in here
    escape       back to normal

The five toggles below — `s`, `r`, `R`, `C` and `m` — keep their letters in here too, because what plays next is
transport state rather than navigation.

`50%` is vim's: a count and `%` goes that far into the track, the way `50%` in vim goes that far into a file. Bare `%`
means halfway.

A panel floats over the middle of the view while the mode is on: what is playing in more detail than the status line has
room for, a position bar, and what is next — which is worth a line precisely because shuffle makes it unguessable. It
covers the list on purpose. In transport mode the list is not what your hands are doing, and a panel tucked along the
bottom edge reads as one more helper rather than as a mode you are in.

**Nothing falls through.** A mode is a keymap of its own, not a layer over the normal one — `j` is volume here and not
"down", because a mode that leaked navigation would not have bought anything. That is why the way out is bound inside
it, and why `?` lists only what actually works.

The trade is deliberate: pausing costs `<leader>x` while you are browsing, and one key once you are listening. Normal
mode is home because the common _interaction_ is looking for something to play, even though playing is the common
_state_.

## What plays next

Five toggles, and they show in the status line so you can see what is on without pressing anything.

    s            shuffle
    r            repeat the list when it ends
    R            repeat the current track
    C            continue — carry on into the next album rather than stopping
    m            cycle all / artist / album

`m` is what cmus calls `aaa_mode`, and it decides how far "the list" reaches: everything, everything by this artist, or
just this album. It is the setting that makes `r` mean three different things.

The queue always wins. Anything in view 4 plays before whatever these would have chosen, which is what makes it a queue
rather than a second playlist.

## Leaving

    :q

There is no confirmation and nothing is lost: the view you were on, the queue, the open playlist, the current track and
its position all come back next time.

**Quitting is not a keystroke.** cmus binds `q`, and a single unmodified letter sitting beside `n` and `p` is one slip
from ending a session — the one action here with nothing behind it to undo. `:q` is the same cost vim charges.

## Numbers down the side

    :set relative_numbers=true

Turns on a vim-style gutter: the row you are on shows its absolute number, and everything else shows its distance. The
number beside a row is what to type before `j` or `k` to get there. Track lists count _tracks_, so an album divider
never shifts the number you need.
