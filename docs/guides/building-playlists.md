# Building playlists

View 3 has two panes: the playlists on the left, the open one's tracks on the right. `tab` swaps between them.

    ┌ playlists ────────┐┌ some-beats ──────────────┐
    │  electronic  rule ││   1  The Bug — Poison…   │
    │▸ some-beats     7 ││   2  Sei A — Always      │
    │  roadtrip      12 ││   3  Mr. Oizo — Ham      │
    └───────────────────┘└──────────────────────────┘

Rules are marked as such. Opening one gives a list you cannot edit — it is computed from its rule every time — which is
worth knowing before you try.

## Starting one

    :playlist new roadtrip     start an empty one
    :playlist open roadtrip    open it
    :playlist list             what there is

Then browse the library and press `a`. It adds to **the playlist you have open**, so there is always somewhere for it to
go — and if nothing is open, `a` says so and points at `:playlist new` rather than inventing a file.

Every edit is written to the file immediately. There is no save step, and nothing to lose to a crash.

## Reordering, and moving between playlists

The register keys are vim's, and they work here for the same reason they work between buffers: **the register is
global.**

    yy      copy the track under the cursor
    dd      cut it
    p       put after the cursor
    P       put before
    3dd     cut three
    2p      put twice
    D       cut from here to the end
    Y       copy from here to the end

Because `p` puts _after_, `dd` then `p` swaps a track with the one below it. That is the whole reordering story — there
is no separate "move up" key.

To move a track to another playlist:

    dd                      cut it here
    tab, j, Enter           open the other playlist
    p                       put it there

`yy` instead of `dd` copies rather than moves, which is usually what you want — a track can live in several playlists.

## Undo

    u       undo
    ^r      redo

Per playlist, the way vim's undo is per buffer. Undoing in one playlist never reaches into another, and a playlist keeps
its history across being closed and reopened.

## Keeping one

    :playlist save mix

Copies the open playlist under a new name. On a rule that is how you get an editable copy of what it currently holds —
gather first, name it only once it has earned a name.

Playlists are `.m3u8` files in `<data_dir>/playlists`, with paths relative to the file. Anything else can read them, and
one dropped into that directory from another program simply appears.
