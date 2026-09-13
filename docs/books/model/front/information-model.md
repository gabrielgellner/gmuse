# About this part

The information model answers one question in three ways: **what is a library entry, and how is it different from a
file?**

The short answer is that a file is a _serialization_ of the library and never its truth. Tags are an export format. The
library is an append-only log of facts — somebody said this, at this time, on this device — and what you see on screen
is a fold of that log. A rename is invisible to it, because a fact names a recording rather than a path.

Three documents, in the order they are worth reading:

1. **The domain model** — the entities, the two layers (observation and reference), how identity is minted so two
   machines converge without coordinating, and what is taken from RDF and what is not. It is a _proposal_: the grain it
   argues for is not what the code is keyed by everywhere yet.
2. **Where a name comes from** — the rule that a term is borrowed verbatim from MusicBrainz where MusicBrainz has one,
   and that a distinction taken from SKOS or MADS/RDF is spelled MusicBrainz's way. Short, and it settles arguments.
3. **The named operations** — the built contract. Import, relink and forget are operations with names; a scan reconciles
   holdings and reads no tags; the UI edits library metadata and writing tags is a separate export. There is no tag
   editor, and that is a decision rather than a gap.

## The rule underneath all three

**Nothing is implicit.** There is no "update the library" call anywhere in this design — only appending a fact. That is
why history survives a rename, why two devices can be edited apart and merged, and why a measurement that says "this
file looks like a 128k source" is a queryable record rather than a folder somebody moved things into.
