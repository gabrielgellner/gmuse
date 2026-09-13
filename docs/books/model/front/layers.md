# About this part

Four systems share one library: the **librarian**, the **player**, the **scanner**, and the **tools**. This part is the
rule about which of them may talk to which, and in which direction.

The headline: **the model must not know that any of the others exist.** That began as an intention, became a test that
walked the source looking for forbidden imports, and is now a **compile error** — `gmuse-library` does not depend on the
root crate and cannot, because a dependency cycle does not build. The crate split of September 2026 is what turned the
strongest rule in this design from a convention into a property of the build.

Read in this order:

1. **What may talk to what** — the four systems, the direction rule, and the two halves of every write: nothing mutates,
   everything records.
2. **Inside the librarian** — a finer cut through the same material: the log, the projection, the query, the adapters.
   The rule it turns on is that **a read never touches the filesystem and never folds the log**. Partly built; the
   chapter is honest about which part.
3. **Where a file lives** — one rule, and it is short enough to state here: the storage tree may use _structural_ facts
   only. Who made it, and what release it is on. Never genre, era, catalog or tempo, because those are browsing and a
   tree has one dimension.

## Why this part earns its place in a book

Because a layer rule is the kind of claim that goes false quietly. The document in chapter 1 spent some weeks describing
a tree that had moved and a migration that had finished, in a file whose own first line tells you to read it _before
adding a caller in either direction_. Publishing it is partly a way of keeping it honest.
