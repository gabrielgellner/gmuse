# gmuse

A terminal music player and library manager, in Rust, with a pure-Rust audio path.

This book publishes the design documents that describe **what the library is** and **which part of the program may talk
to which**. It adds no new prose beyond the framing you are reading: every chapter below is a document from
[`docs/design/`](https://github.com/gabrielgellner/gmuse/tree/main/docs/design), rendered rather than retold.

That is deliberate. A book that paraphrased those documents would be a second copy of every claim in them, and this
project has already paid for what happens next: an audit in September 2026 found ten load-bearing sentences that had
quietly stopped being true, including a guidance document still describing a migration that had finished. So the
chapters are derived from the documents at build time — split on their own headings, so a renamed section renames a
chapter rather than breaking one.

## The one thing to know before reading

**cmus is the origin and no longer the authority.** The interaction model started there, and the design has since
outgrown it in three directions that matter:

- the library is an **append-only fact log** rather than a tag reader,
- the player is one view onto an **engine** that other clients drive,
- the transport vocabulary is derived from **DJ software**, not from a player.

"cmus does it this way" is evidence, not a decision.

## How to read a status marker

Each chapter opens with one. They are not decoration — several of these documents are proposals, and reading one as a
description of the program is the single most likely way to be misled by this book.

| Marker                        | What it means                                                            |
| ----------------------------- | ------------------------------------------------------------------------ |
| **Built**                     | Describes the program as it is. Enforced where it says it is enforced.   |
| **The contract**              | A built interface. What it forbids, it forbids today.                    |
| **Settled**                   | A decision taken and not expected to reopen.                             |
| **Settled design, not built** | The decision is made; no code implements it yet.                         |
| **Proposal, not built**       | Under argument. Do not read as description.                              |
| **Proposal, partly built**    | Some of it holds today. The chapter says which.                          |
| **A critique, kept**          | An argument _against_ the design, preserved because it was worth making. |

A decision can be reopened — with an entry in the Decision Log, not on a whim.

## The companion book

[The interaction model](../interaction/) is how the player is _driven_ — the tree, the motions, the query language, and
the transport vocabulary. It also publishes the seven guides the program ships, which are the same files `gmuse`
compiles into its binary and draws as view 7.

The two books meet at the query language: what you type after `:` filters the same projection the librarian folds.

## What is in neither

`NOT_IN_ANY_BOOK` in [`tools/build_book.py`](https://github.com/gabrielgellner/gmuse/blob/main/tools/build_book.py)
lists every document that is in no book and why, and the build **fails** if one is neither published nor listed — so
nothing goes missing by being forgotten.
