# The layers, and what may talk to what

**Status: partly built.** `src/model/` exists and is enforced; the seam to the player is a ratchet, not yet a wall; the
tools still write files behind the library's back. This says where each piece belongs and why, so the remaining work has
a shape rather than a direction.

---

## 1. Four systems, one library

```
                        ┌──────────────────────────────────────────┐
                        │            THE LIBRARIAN                 │
                        │              src/model                   │
                        │                                          │
                        │   entities · facts · the log · the fold  │
                        └──────────────────────────────────────────┘
                             ▲              │              ▲
              appends facts  │              │ answers      │  appends facts
                             │              ▼              │
   ┌─────────────────────────┴───┐  ┌───────────────┐  ┌───┴────────────────────┐
   │  THE SCANNER                │  │  THE PLAYER   │  │  THE TOOLS             │
   │  src/model/populate, ingest │  │  src/ui       │  │  tools/*.py            │
   │                             │  │  src/audio    │  │                        │
   │  files → Scanned → facts    │  │  asks & plays │  │  measure · enrich ·     │
   └─────────────────────────────┘  └───────────────┘  │  import · normalise     │
                 ▲                                     └────────────┬───────────┘
                 │ reads                    writes tags              │
      ┌──────────┴──────────────────────────────────────────────────▼─────────┐
      │  FILES ON DISK — a serialization of the library, never its truth      │
      └───────────────────────────────────────────────────────────────────────┘
```

The librarian is the only thing that knows what the library _is_. Everything else either tells it something or asks it
something.

---

## 2. The direction rule

> **The model must not know that any of the others exist.**

Asserted, not merely intended: `model/seam.rs` walks `src/model/` and fails if anything imports `ui`, `cmd`, `audio`,
`ratatui` or `crossterm`. It has caught two live mistakes so far — a renderer's sort key and a trait keyed by a type it
should have named from its own side.

The reverse is fine and expected. The player reaches into the model constantly; that is what a seam is for.

The player's side of it is a **ratchet** rather than a rule, because it is not true yet: `src/ui/` still names `Store`
36 times, and the test asserts _no worse than today_ so that the number can only fall. Five of those are outside test
fixtures. It becomes an assertion when it reaches zero (#86).

---

## 3. Writing: nothing mutates, everything records

There is no "update the library" call. There is only **appending a fact**.

```rust
Fact::RecordingCatalogued { recording, title, artist }
Fact::CreditAsserted { subject, artist, role, position, as_credited, join_phrase }
Fact::TrackPlaced { track, medium, recording, position, title_on_release }
Fact::GenreAssigned { subject, genre, basis, confidence }
Fact::EncodingMeasured { encoding, cutoff_hz, analyser }
Fact::RecordingRated { recording, stars }
Fact::RecordingPlayed { recording, encoding }
```

Three things follow, and they are the reason for the shape:

**A fact carries who said so.** `basis` distinguishes a Discogs genre from a hand-typed one; `confidence` distinguishes
a MusicBrainz match from a guess. A tag write throws both away — which is why the current genre pipeline's careful
`genre-uncertain.tsv` is discarded at the end of every pass.

On an assignment those two fields are not the same kind of field, and the fold treats them differently. **`basis` is
part of the claim's identity; `confidence` is an attribute riding on it.** So `(subject, concept, basis)` is what makes
a row exist — Discogs saying Techno and a person saying Techno are two rows, because they are two independent pieces of
evidence and their agreeing is the most useful thing the pair says. `confidence` is last-writer-wins within one row, so
re-scoring an import updates in place while a human's disagreement lands beside it. Nothing ever edits another
asserter's claim.

A retraction is keyed one field shorter — `(subject, concept)`, no basis — because it answers the question rather than
arguing with one asserter. Otherwise retracting would mean retracting once per source that ever said it, and the next
import would put it back.

### Who appends, and who proposes

Two routes write to the library and they are not equals.

| author                                  | route                                             | basis                                                             | gate                              |
| --------------------------------------- | ------------------------------------------------- | ----------------------------------------------------------------- | --------------------------------- |
| a person in the UI, acting on one thing | direct append to this device's log                | `Manual`, confidence `Confirmed`                                  | none — the person _is_ the review |
| any tool, matcher or import             | `proposal.rs` JSONL → a person reads it → `apply` | the tool's own source (`Discogs`, `MusicBrainz`, `TagVariant`, …) | mandatory                         |

No tool writes `Source::Manual`, and no tool appends to a log directly. **Volume is the whole reason.** A person
mis-tags one recording; a matching pass mis-tags six hundred in a minute, and the three of 607 that the identification
pass got wrong were found only by reading the file. A tool that could write `Manual` would be laundering its guesses
into the one basis nothing downstream second-guesses. `src/model/seam.rs` asserts it.

(`Manual` is the existing name for what the addendum calls `Human`. It is on the wire in every proposal file, and the
two say the same thing.)

### What a gesture assigns to

Listen-time tagging — acting on what is playing — targets the **`Recording`**. The claim is about this version, and must
not leak onto its siblings or the rest of the album. Album-level actions, acting on a group in the tree, target the
**`ReleaseGroup`**, matching the import default and the census.

The UI never targets a `Track` (a slot on a medium) or an `Encoding` (bytes). A gesture that seems to want either is
mis-designed.

### Concepts, renames, and orphans

A concept id is minted from its **normalised label**, through `ingest::normalise` and nothing else — which is what lets
two devices define "Dub Techno" independently and union into one row with no registry. The same bet recording ids make.

A rename therefore does **not** re-mint: the id came from the old label and every assignment points at it. A device that
meets the new label fresh mints a different id, and one concept becomes two. **That split is accepted rather than
prevented**, because it is the repairable error — a split is one merge from repaired, a false merge is permanent. A
rename emits the label change and a `Retired` alias, so the old name stays findable, and the ordinary redirect
(`merge.rs::Merges`) resolves both ids to one for good. Do not re-mint on rename, and do not switch concepts to random
ids to dodge this: the first rewrites every assignment, the second gives up convergent definition entirely.

A concept with no place in the browse hierarchy is an ordinary row. Assignment does not wait for placement — the moment
you know the answer would otherwise be the moment you cannot record it. What it costs is that closure queries (`genre+`)
do not reach it, so `Catalogue::unplaced_genres` is a standing review query, alongside `not qualifier?`.

The assignment itself has no id, and must not grow one. A third identity beside the subject's and the concept's could
only be random, which breaks convergent minting outright — two devices importing the same Discogs page have to emit the
same row — or content-derived, which buys nothing because the content is already the key. `src/model/seam.rs` asserts
this.

**Merging is union.** Two devices, two tools, two passes — all append, nothing is overwritten, and the fold decides what
shows. That is what makes a second machine free and a second _tool_ free with it.

**A mutation declares itself.** #62's inversion: today a rename happens behind gmuse's back and `scan.rs` pairs a
departure with an arrival by size and duration and _hopes_. Every library operation of 2026-09-01 was guessed at, and
all four were guessed wrong — one of them orphaning a four-star rating. An inference can be wrong silently; a
declaration cannot.

---

## 4. Reading: projections, never the store

The player asks the librarian and gets back something already assembled:

| what it wants             | what it asks        | what it must not do            |
| ------------------------- | ------------------- | ------------------------------ |
| a row to draw             | `Librarian::row`    | reach for a `Track`            |
| what a list groups under  | `Listing::listed`   | reach for a `Store`            |
| where to open a file, now | `Listing::playable` | **keep** the locator           |
| the artist tree           | `Tree::of`          | un-normalise a sort key itself |

The last column is the load-bearing one. A locator is produced at the moment of playing and never stored, because a
player that cannot hold a path cannot key anything by one — and keying by path is what stranded three history entries
and a four-star rating on a file that still existed.

---

## 5. Where the Python tools fit

They are not a special case. They are a fourth writer, and they divide by what they actually produce:

| tool         | is           | produces                            | today                        |
| ------------ | ------------ | ----------------------------------- | ---------------------------- |
| `quality.py` | an analyser  | `Fact::Measured`                    | a TSV that gets deleted      |
| `genre/*.py` | an enricher  | `Fact::GenreAssigned` + basis       | writes `TCON` into files     |
| `credits.py` | an enricher  | credits with roles and confidence   | a TSV, then hand-editing     |
| `ingest.py`  | an importer  | `Scanned` → the whole ladder        | copies files, then tags them |
| `tags.py`    | a normaliser | **nothing** — it is the export side | writes files, which is right |

The first four should append facts. The last one should not: normalising a tag is writing the _serialization_, which is
downstream of the library and always will be.

The verbs they use are in [`library-api.md`](library-api.md): `query` for asking, `apply` for a file of proposed facts,
and `import` / `relink` / `forget` for the operations that change what the library _is_. All of them work with no player
running, because a tool must be able to say something whether or not you are listening to music.

### What the tools cannot do for themselves

**Compute an identity.** `Id<Recording>` derives from a MusicBrainz id or from `audio_id`, and `audio_id` is XXH3 over
Symphonia's packet boundaries — not reproducible in Python, and not worth trying.

So a tool speaks in **paths** and the librarian translates. That is exactly the shape `migrate::to_records` already has:
events carrying paths, plus an `identify` closure supplied by something that has scanned. A tool emits `(path, fact)`;
gmuse resolves the path and appends.

### Why the remote socket is not this

`--remote` exists and is deliberately narrow: `is_remotable` lists what may cross it, and it is the _player's_ controls
— play, pause, next, status. It drives the thing in front of you. It is not, and should not become, a way to edit the
library, because those are different lifetimes: the player is running or it is not, and the library is there either way.

A tool must be able to say something with no gmuse running at all.

---

## 6. What is true today, and what is not

**True.** The model exists, is enforced not to know about the player, and holds the whole vocabulary — entities, facts,
the fold, the projections. Files become rows end to end. Two instances converge without coordinating.

**Not true yet.** The player still reads a `Store`. The tools still write files. There are two logs. The measurement
that decides which copies need replacing lives in a TSV that has already been lost once.

The gap between those two lists is the migration (#80–#86), and the ordering is not arbitrary: the tools cannot append
facts until the log they append to is the only one (#82), and the player cannot stop reading a store until the librarian
answers everything it asks (#77–#79).

---

## 7. Related

`domain-model.md` — what the entities are · `library-sync-design.md` — how the log merges · `tree-view-design.md` — what
a view asks for · #62 (mutation API) · #77 (the seam) · #82 (one log) · #86 (delete the store) · #87 (a bad copy is a
row)
