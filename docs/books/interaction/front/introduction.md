# gmuse — the interaction model

How the player is driven: the tree you browse, the motions that move through it, the language you type at `:`, and the
transport verbs that start and stop sound.

Two halves, and they are for different moods. **Part I is the guides the program itself ships** — the same files `gmuse`
compiles into its binary and draws as view 7, published here so they are searchable and linkable without opening the
player. **Part II is the design** behind them: why one pane holds three levels, why a motion counts groups rather than
rows, and what the transport vocabulary is borrowed from.

## The origin, and why it is no longer the authority

The interaction model started at **cmus**, and the reference is still useful. It has since outgrown it in the direction
that matters here: the transport vocabulary is derived from **DJ software** rather than from a media player, because a
player's four verbs — play, pause, next, previous — cannot express cueing, looping, or a jump whose unit is a bar.

"cmus does it this way" is evidence, not a decision.

The other borrowing is **vim**, and it is deeper than the keys. Views are tabs, `gt` and `3gt` work, the `z` family
folds, and `d` then `p` swaps two tracks in a playlist. What makes that more than a key list is that the library tree is
drawn as **one buffer**: `j`, `3j`, `gg`, `G` and `/` move over artists, albums and tracks uniformly, because they are
rows rather than levels of a hierarchy you have to think about.

## How to read a status marker

Part II's chapters open with one, and they are not decoration — two of the five are proposals under argument.

| Marker                  | What it means                                           |
| ----------------------- | ------------------------------------------------------- |
| **A guide**             | Written for someone using the program. Ships inside it. |
| **Design**              | Describes the program as it is, and why.                |
| **Proposal, not built** | Under argument. Do not read as description.             |

## The companion book

[The information model and the layers](../model/) covers what a library entry _is_ — facts, identity, the log — and
which part of the program may talk to which. This book assumes none of it, but the two meet at the query language: what
you type after `:` is filtering the same projection the librarian folds.
