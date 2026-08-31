# Moving around

Seven views, switched like nvim tabs. `gt` and `gT` step; `3gt` jumps straight to view 3; `g<Tab>` returns to the one
you were on before.

    1 library    2 sorted     3 playlist   4 queue
    5 browser    6 scope      7 settings

Within any of them the motions are vim's. `j` and `k` move, and take a count — `3j` goes down three. `gg` and `G` go to
the top and bottom, and `12G` goes to row 12. `^f` and `^b` page.

`/` searches forwards and `?` backwards; `n` and `N` repeat.

## The leader

`space` is the leader. It is not bound to anything on its own, which is what lets it start a chord.

    <leader>?    the keys that work in this view
    <leader>u    rescan the library

`<leader>?` is worth pressing in each view — it lists that view's own bindings as well as the common ones, which is a
different question from view 7's list of every binding everywhere.

Bind your own in the config, where `<leader>` stands for whatever `leader` says:

    [keys.common]
    "<leader> p" = "playlist list"

## Numbers down the side

    :set relative_numbers=true

Turns on a vim-style gutter: the row you are on shows its absolute number, and everything else shows its distance. The
number beside a row is what to type before `j` or `k` to get there. Track lists count _tracks_, so an album divider
never shifts the number you need.
