# Access layers: the log, the projection, the query, and who may touch which

**Status: a proposal, not built.** It supersedes nothing; it makes explicit a separation [`layers.md`](layers.md)
already implies between systems, and applies it _inside_ the librarian, where three kinds of thing are currently one.

Read [`layers.md`](layers.md) first. That says which **system** may talk to which — the librarian, the player, the
scanner, the `tools/`. This says which **layer inside the librarian** may talk to which, which is a different cut and
the one that is not being kept today.

---

## 1. What is mixed, measured

A warm `gmuse shelf query --filter 'artist="GAK"'` returns four rows. Where the time goes:

| phase                                                         |        time |
| ------------------------------------------------------------- | ----------: |
| `Instance::open` — read and parse 122 MB / 279,340 records    |       217ms |
| `merge` — records into an instance                            |        41ms |
| **`populate` — walk 7,267 files and parse every file's tags** | **2,503ms** |
| `reconcile` — walk and `stat`                                 |       130ms |
| fold to `Catalogue`                                           |        43ms |
| build `Librarian`                                             |        42ms |
| emit and filter rows                                          |       158ms |

`populate` is 80% of it. The gap between it and `reconcile` — 2,503ms against 130ms, over the same files with the same
measurement cache — is entirely tag parsing: ~0.33ms × 7,267 files, re-deriving what the log recorded at import.

**Speed is the symptom.** The defect is that answering a question consults three different kinds of truth at once:

1. the **log**, which is authoritative,
2. the **filesystem**, which is a device's current circumstance,
3. the **tags in the files**, which the design says are a serialisation consumed at import and never consulted again.

So the same query can answer differently depending on what is on disk and what a tagger last wrote, and no signature
says so. That is the bug magnet: not any single wrong line, but that there is no boundary a wrong line would have to
cross.

### It drifted for a reason, and the reason is fixable

There is no working explicit step that notices the filesystem. **`:rescan` is a stub and a launch no longer scans**
(#64), so `populate(roots)` became the only mechanism there is — and `populate.rs` says so in as many words: _"seven of
its eight callers pass the library roots to bring their view up to date."_

The genuine need underneath is narrow and honest: an identification work list has to show files that arrived and rest on
their bytes, or the pass that exists to identify them cannot see them. That is **one caller's requirement, generalised
into every read**.

---

## 2. The four layers, and the one rule each

```
                   ┌──────────────────────────────────────────────┐
   named           │  L0  FACTS          the append-only log      │   the truth
   operations ────▶│      write: append a fact                    │
   only            │      read:  the projector, and nothing else  │
                   └───────────────────┬──────────────────────────┘
                                       │  fold, forward-only, resumable
                   ┌───────────────────▼──────────────────────────┐
                   │  L1  PROJECTION     a materialised view      │   derived,
                   │      write: the projector, from facts        │   disposable
                   │      read:  L2                               │
                   └───────────────────┬──────────────────────────┘
                                       │
                   ┌───────────────────▼──────────────────────────┐
                   │  L2  QUERY          the evaluator            │   pure
                   │      filter, levels, sort, format            │
                   └───────────────────┬──────────────────────────┘
                                       │
                   ┌───────────────────▼──────────────────────────┐
                   │  L3  ADAPTERS       CLI · TUI · Python       │   no logic
                   └──────────────────────────────────────────────┘

   THE FILESYSTEM is not a layer. It is reached only by named operations —
   scan, import, verify, render, retire — which read bytes and append facts.
   Nothing in L1, L2 or L3 may open a music file.
```

**L0 — facts.** Append-only. The only write is appending a fact; there is no update and no delete. The only reader is
the projector. Nothing else folds the log, and nothing else needs to.

**L1 — the projection.** The `Catalogue`: recordings, encodings, holdings, credits, concepts, aliases, measurements,
plays and ratings — **keyed by id and assembled into nothing**. It already has this shape, because it mirrors a log
whose facts name ids: `artist_names` holds a name once per artist, `credits` is keyed by
`(subject, artist, role, source)`, `assertions` by ids.

**Derived and disposable** — it holds nothing that cannot be rebuilt by replaying the log, which is the rule every
event-sourced system converges on and the one this project already applies to `cache.rs`.

**The drawn rows are not here.** `Librarian` holds `BTreeMap<CopyId, Drawn>` with credit lines assembled into strings,
and that is **L2's materialisation**, not L1's storage: built per fold, never persisted, thrown away and rebuilt rather
than patched. It cannot go stale for the same reason `Lookup::Built` cannot — it does not outlive the thing it was
derived from. Rebuilding all 7,273 of them costs **42ms**.

That distinction is the whole of §5a. Assembled strings are fine where they cannot outlive their source; what is not
fine is _storing_ them somewhere a fact would have to reach in and correct.

**L2 — the query.** `filter.rs`, `format.rs`, `sort.rs` and the levels. Pure: given a projection it is a function, with
no I/O and no clock it was not handed.

**L3 — adapters.** The `:` line, `shelf …`, and the Python extension. They parse input into typed values, call L2, and
render. They hold no rules.

### The rule that makes it airtight

> **A layer may call the one directly beneath it. Nothing may reach past one, and nothing may reach up.**

Two consequences worth stating because they are what is broken today:

- **A read never touches the filesystem.** Not to stat, not to hash, not to parse a tag. This is the absolute one.
- **A read may bring the projection up to date, and may not go looking for news.** Folding is a derivation over the log
  and costs an integer comparison when there is nothing to do; scanning is a discovery over the disk and costs seconds.
  §3a separates them, because today they are one function.

---

## 3. The contracts

### L0 — facts

```rust
pub trait Facts {
    /// Append. The only mutation there is.
    fn append(&mut self, now: Stamp, fact: Fact);
    /// Everything after a position, in order. For the projector.
    fn since(&self, at: Position) -> impl Iterator<Item = (Position, &Record)>;
    fn head(&self) -> Position;
}
```

**What it may not do:** know what a row is, what a query is, or that a projection exists. It holds records and hands
them back in order.

**Who may hold one:** the projector, and the named operations that append.

### L1 — the projection

```rust
pub trait Projection {
    fn rows(&self) -> impl Iterator<Item = RowRef<'_>>;
    fn row(&self, id: CopyId) -> Option<RowRef<'_>>;
    fn locator(&self, encoding: Id<Encoding>, device: Id<Device>) -> Option<&str>;
    /// How far the log has been folded in. The whole of staleness.
    fn position(&self) -> Position;
}

pub trait Projector {
    /// Fold forward. Idempotent, so a crash mid-fold costs nothing but time.
    fn advance(&mut self, facts: &dyn Facts) -> Result<Position, ProjectError>;
    /// Throw it away and replay from nothing. Always available, by construction.
    fn rebuild(&mut self, facts: &dyn Facts) -> Result<Position, ProjectError>;
}
```

**Three properties, and they are the standard ones.**

- **Idempotent.** Applying a fact twice is applying it once. A fold that stops halfway can be resumed or restarted and
  neither is a special case. The model already has this: facts are row-existence or last-writer-wins by design, and
  `AliasAsserted` says so explicitly — _"nothing here has a last-writer rule because nothing here can disagree."_
- **Rebuildable from nothing.** Never store in the projection anything that cannot be reconstructed from the log. This
  is the invariant that makes schema change safe and routine: bump a version, rebuild, done. It is also exactly the
  trade `cache.rs` already documents — _"a format change bumps `FORMAT_VERSION` and the next run rescans."_
- **It knows its own position.** A projection that cannot say how far behind it is cannot be trusted or repaired, and
  staleness becomes a rumour instead of a number.

**What it may not do:** open a file, read a tag, ask the network, or decide anything. It is a fold and a store.

### L2 — the query

```rust
pub fn ask(view: &dyn Projection, spec: &ViewSpec) -> Vec<Row>;
```

**What it may not do:** touch L0, touch the filesystem, or mutate anything. Given the same projection and the same spec
it returns the same rows, and that is testable without a temp directory — which is the practical payoff and the reason
the current tests need one.

### L3 — adapters

**What they may not do:** hold a rule. If the CLI and the TUI could disagree about what `artist=` means, the rule is in
the wrong layer. This is already the project's instinct — `:filter` and `--filter` share one parser on purpose — and
this generalises it.

### The named operations, which cross deliberately

These are the only things that read bytes, and each is a sentence a person says:

| operation | reads                 | appends                              |
| --------- | --------------------- | ------------------------------------ |
| `scan`    | the filesystem        | `HoldingsAsserted`                   |
| `import`  | the filesystem, tags  | everything an ingest declares        |
| `verify`  | the filesystem, bytes | nothing — it reports                 |
| `render`  | the filesystem        | `HoldingsAsserted` for what it moved |
| `retire`  | the filesystem        | tombstones                           |

**`render` is the one that writes the tree**, and it is why the tree is an output rather than an input. A fact never
touches the filesystem: it changes what the tree _should_ be, and `render` makes it so and records where things went.

That leaves three states and two gaps, which are not the same job:

|                              |                                                |
| ---------------------------- | ---------------------------------------------- |
| where a file **should** be   | derived from the facts and `storage-layout.md` |
| where a file **is**          | `HoldingsAsserted`, in the log                 |
| where a file **actually** is | the disk                                       |

**Only the second is a gap.** `is ≠ actually` is **drift**: a holding pointing at bytes that are not there, which costs
the ability to open the file, and which `scan` closes. `should ≠ is` is **rendering lag**, and it is _cosmetic_ — the
folder names are out of date, and nothing in the library is wrong, nothing is unreachable, and no query answers
differently.

**The layout carries no meaning.** `reconcile` hashes the audio, derives the encoding, and stores the path as a value;
the path is never consulted for identity, and `populate.rs` says so in as many words — _"known by its bytes, not by
where it sits. A file that moved is the same encoding at a new locator, which is why a move needs no operation and
cannot lose anything."_ A library of `<uuid>.flac` would bind every holding identically.

So `render` is for **people, and for other software that reads folders** — a phone, a DJ tool, a share somebody browses
— rather than something the library needs. `storage-layout.md`'s one rule is a presentation decision, which is exactly
why it says a tree has one dimension and genre is for browsing.

The one thing a readable tree buys that a content-addressed one does not is **recovery**: lose the log, and
`{file_under}/{album}/{track} {title}` is self-describing where a hash is opaque. That is a hedge worth keeping, and it
is not a dependency.

They sit **beside** the layers rather than inside them: they take an L0 handle and the roots, and they are commands, not
functions something calls on the way to answering a question. `library-api.md` already says this — _"a scan reconciles
holdings and reads no tags"_, _"import, relink and forget are named operations"_. This makes the code match.

---

## 3a. Folding is not scanning, and only one of them is a read

These are one word today — `populate` reads the filesystem **and** mints facts — and separating them is most of the fix.

|                  | **fold** (log → projection)     | **scan** (filesystem → log)       |
| ---------------- | ------------------------------- | --------------------------------- |
| what it is       | a derivation                    | a discovery                       |
| reads            | the log only                    | the disk, and tags                |
| deterministic    | yes — same log, same projection | no — depends on what is out there |
| produces         | nothing new                     | **facts**                         |
| costs            | 43ms whole; ~0 incrementally    | seconds                           |
| may a read do it | **yes**                         | **never**                         |

A scan is **upstream** of the log, so nothing downstream can trigger it. "Append a fact, therefore rescan" is circular:
the scan is the thing that writes facts, not a consumer of them.

### So when does the fold happen?

Not on every append, for three reasons, and the rule that covers all three is better than the hook would have been.

- **Facts do not only arrive by local append.** This is a multi-device design — another machine's log merges in, and
  `AliasAsserted` is written so that two devices recording one name union to both. A hook on local append misses
  precisely the case staleness comes from.
- **Appends come in batches.** `shelf import --write` appends thousands. Folding per fact would be pathological; the
  unit is the operation, not the record.
- **A failed projection write must not fail the append.** The log is the truth and has to accept the fact regardless, or
  the derived thing has quietly become load-bearing.

**So it is position-driven, not event-driven:**

```
advance():  fold from projection.position() to facts.head()
            equal     -> a no-op, one integer comparison
            behind    -> fold exactly the gap
```

- Any operation that **appended** calls `advance()` once when its batch is done.
- A **read may also call it**, because it is cheap and touches only the log. That is the answer to "may a query
  auto-advance": yes. What a read may never do is **scan**.
- A **sync** that merged another device's records leaves the projection behind, and the next `advance()` catches up
  without anything special happening.

This is why L1 must know its own position (§3). Without it, "am I behind" is unanswerable and the only safe move is to
rebuild — which is how a cheap derivation turns back into an expensive one.

### The interactive case, which is the one that has to feel instant

Editing in the TUI — filing a recording under `Dance`, changing a rating, correcting a credit — appends a fact, and the
projection **is** stale the moment it does. By exactly one fact, for microseconds.

**Staleness is counted in facts, not in time.** The 43ms above is a fold of 279,340 records from nothing; the gap after
a keystroke is one record.

```
append  ->  flush  ->  advance()  ->  redraw
```

The ordering carries weight: the fact is durable before the UI claims it happened, so a failed append has nothing to
un-draw.

What matters more than the speed is that **this is the path `shelf apply` already takes**. An edit made in the TUI and
an edit proposed by a tool go through the same machinery rather than through two that happen to agree.

**The temptation to refuse is the optimistic update** — patching the drawn row to say `Dance` before the fold, so the
redraw has nothing to wait for. It makes the UI a _second interpreter of facts_, and then what the UI thinks `Filed`
means and what the projector thinks it means can drift. Deciding what a fact meant is the projector's only job. This is
#113 in a performance costume, buying microseconds.

### What that requires of the projector

> `advance()` must be able to apply **one** fact to an existing projection.

If the only move is `rebuild`, a keystroke costs a full replay and the design collapses back to where it started. So
per-fact application is a requirement and not an optimisation, and it has three tiers worth naming up front:

|             | example                                               | cost                                                                  |
| ----------- | ----------------------------------------------------- | --------------------------------------------------------------------- |
| **narrow**  | `Filed`, `Rated`, `AliasAsserted`, `EncodingMeasured` | touch the rows named by the fact                                      |
| **wide**    | `ArtistMerged`, `RecordingMerged`                     | touch every row crediting the loser — still incremental, just broader |
| **rebuild** | a fact whose effect cannot be localised at all        | a full replay                                                         |

**Settled, and only three facts are wide.** All 53 variants are classified by `Fact::blast()`, an exhaustive `match` —
so a new fact does not compile until it has a tier.

The first pass classified eight as wide and **seven of those were wrong**, for one reason: they were being read against
the _drawn rows_ rather than against the projection. `ArtistNamed` looked wide because every row showing that artist
carries the name; the fold writes `cat.artist_names.insert(who(artist), name)` — one entry. Rows are L2's
materialisation, rebuilt per fold, and no fact has to reach into one.

| fact                                 | at the projection                                                   |
| ------------------------------------ | ------------------------------------------------------------------- |
| `ArtistNamed`, `LabelNamed`          | one map entry                                                       |
| `CreditAsserted`, `CreditRetracted`  | one entry — `credits` is keyed by `(subject, artist, role, source)` |
| `ReleaseGroupRetitled`               | one group                                                           |
| `HierarchyNarrowed`, `ArtistRelated` | one edge                                                            |
| `HoldingsAsserted`                   | every encoding it names — **big, not wide**                         |
| **the three merges**                 | **wide**                                                            |

**The merges are wide and the fold shows why.** It applies them in a _first pass_ and then resolves every id it meets
through the finished set, because a merge recorded on Tuesday has to move a rating given on Monday. Applying one to a
projection that already exists means re-resolving what is already in it — a set the fact does not name, which is what
wide means.

**Wide means the affected set is not derivable from the fact**, and size is not the question: `HoldingsAsserted` is the
largest fact this log holds — 62 of them were 54 MB before #201 compacted them — and it is narrow, because the affected
set is exactly the keys of the map it carries.

**The caveat that would reverse two of these.** `HierarchyNarrowed` and `ArtistRelated` are narrow only while nothing
_stores_ a closure over them. Give either a closure table and they become wide, and the arm has to say so — which is
§5a's rule arriving with teeth: never patch a derivation incrementally.

**When it is not obvious, the answer is wide.** `Narrow` is the optimisation; a fact classified narrower than it is
leaves the projection _wrong_, which is a different kind of bad from leaving it slow.

Three tests hold it, and each was checked by breaking it: the rebuild tier is empty; no arm is a wildcard, so the
compiler's exhaustiveness check stays meaningful and a future fact cannot be silently classified as the cheapest tier;
and every variant is named rather than swallowed by a grouping, because the compiler proves the match is _exhaustive_
and not that each variant was _considered_.

**If the third tier ever gains a member, it is named here as well as in the match.** A fact that quietly needs a rebuild
is a stutter nobody can predict; a fact that is _documented_ to need one is a known cost a person can be told about.
Deciding which tier each `Fact` variant sits in is work this plan owes before step 3, and it is exactly the kind of
thing the `every_fact_names_its_subject` test already asserts in a neighbouring register — a fact that names its subject
is a fact whose blast radius can be computed.

---

## 4. How the boundaries are held

**Built, by #193's split.** `src/facts/` and `src/projection/` now exist and carry these tests; `src/query/` has had
them since #191. What blocked it until now was that `model/log.rs` held `Record` and `Fact` at the top and `Catalogue`
and `fold` below them, in one 3,917-line file — the boundary ran through the middle of a file, so there was nothing for
a wrong line to cross.

Documented boundaries rot. This project already has the answer and it is `the_model_does_not_know_the_player_exists` — a
test that walks `src/model/`, reads every `use` line, and fails on a forbidden import. It has caught two live mistakes.

**Extend that mechanism rather than inventing one.** One test per boundary:

| directory         | may not import                                                               |
| ----------------- | ---------------------------------------------------------------------------- |
| `src/facts/`      | `projection`, `query`, `ui`, `cmd`, `audio`, `walkdir`, `lofty`, `symphonia` |
| `src/projection/` | `query`, `ui`, `cmd`, `audio`, `walkdir`, `lofty`, `symphonia`               |
| `src/query/`      | `facts`, `ui`, `cmd`, `audio`, `std::fs`, `walkdir`, `lofty`                 |

The filesystem crates are the load-bearing entries. **`walkdir`, `lofty` and `symphonia` in a read path are the defect
this document exists to prevent**, and an import test catches it at the moment somebody writes it rather than the
afternoon somebody measures it.

A second test, cheap and worth more than it looks: **no `std::fs` under `src/query/`**. That single line is what makes
"a read never touches the filesystem" a fact rather than an intention.

---

## 5. The projection store

### Measured first, because the numbers settle it

The folded log, against the real library — 7,271 recordings, 7,351 encodings, 1,175 artists, 29,880 assertions:

|                                                   |                     |
| ------------------------------------------------- | ------------------- |
| `Catalogue` serialised (postcard)                 | **5.9 MB**          |
| loading that snapshot                             | **9ms**             |
| a process holding **only** the projection         | **31 MB** resident  |
| — against today —                                 |                     |
| reading and parsing the log (279,349 records)     | 228ms               |
| folding it                                        | 44ms                |
| a process holding the parsed log **and** the fold | **221 MB** resident |

So the whole library, fully assembled, is **5.9 MB on disk and ~25 MB in memory**, and a process can have it ready in
**9ms** rather than 272ms — while using a seventh of the memory, because it never holds the 279,349 records at all.

**That settles it for now: a postcard snapshot.** No new dependency, the pattern `cache.rs` already uses, and it is not
close — 9ms is below the threshold where a store's cleverness could repay its complexity. SQLite would buy nothing
measurable at this size.

### What the other options are still for

They are for a library an order of magnitude larger, and the `Projection` trait in §3 exists so that swapping is a
change to one module rather than to everything above it:

| option              | for                                                                                  | against                                                     |
| ------------------- | ------------------------------------------------------------------------------------ | ----------------------------------------------------------- |
| postcard snapshot   | 9ms; 5.9 MB; no dependency; **chosen**                                               | loads everything; no partial read; no concurrent writer     |
| SQLite (`rusqlite`) | partial and indexed reads; concurrent readers; **openable with the `sqlite3` shell** | a C dependency, against the "pure Rust except libopus" line |
| `redb` (pure Rust)  | keeps that line; embedded; typed                                                     | no shell to poke it with; smaller ecosystem                 |

The crossover is memory rather than time. At ten times this library the snapshot is ~59 MB on disk and ~250 MB resident,
which is the point at which a background player holding all of it stops being obviously fine. **Revisit when resident
size becomes the complaint**, not before — and the trait is what makes revisiting cheap.

The three options, for the record:

| option              | for                                                                                                  | against                                                     |
| ------------------- | ---------------------------------------------------------------------------------------------------- | ----------------------------------------------------------- |
| postcard snapshot   | no new dependency; the pattern `cache.rs` already uses; ~50ms to load 7k rows                        | loads everything; no partial read; no concurrency           |
| SQLite (`rusqlite`) | indexed and partial reads; atomic swap; concurrent readers; **inspectable from the `sqlite3` shell** | a C dependency, against the "pure Rust except libopus" line |
| `redb` (pure Rust)  | keeps that line; embedded; typed                                                                     | no shell to poke it with; smaller ecosystem                 |

At 7,272 rows a snapshot gets under a second. SQLite earns its place on where §1 of the brief says this is going — this
is a library manager as much as a player, and its scope is meant to grow — and on being openable by something that is
not gmuse, which for a tool about _your own_ data is worth more than it sounds.

Whichever is chosen, a new dependency needs a Decision Log entry.

### The line to hold, whatever the store

> **One query language. The store is a store.**

`filter.rs`, `format.rs` and `sort.rs` are a tested evaluator with a documented language, and `:filter` and `--filter`
already share one parser precisely so that there is one. Pushing predicates into SQL would be #113's _"two ways to
answer one question"_ at the largest scale available. So L2 stays in Rust over the projection's rows. Pushing a
particular predicate down to an index is an optimisation to measure later, per field, and never a second dialect.

---

## 5a. The shape of the projection

**Normalised, and close to the shape the log already has.** `Catalogue` holds `credits`, `assertions`, `aliases`,
`artist_relations` and `narrows` as association tables; it is the fold into `Drawn` that flattens them into rows
carrying assembled strings. Normalising the projection is mostly a matter of **not doing that**.

A dimensional reading fits, and the vocabulary is worth borrowing because it names the two places a naive star goes
wrong here.

### There is not one fact table — there are four grains

This is the refinement that matters most, because conflating two of them is a bug this project has already paid for.
`EncodingMeasured` is keyed by an encoding, and a work list handing out one row per _recording_ could never reach the
second copy — #183, and 53 files were retired on the strength of a related confusion.

| grain                  | one row is                        | what hangs off it                              |
| ---------------------- | --------------------------------- | ---------------------------------------------- |
| **recording**          | a performance                     | identification, rating, plays, acquisition     |
| **appearance** (track) | a recording in a slot on a medium | position, sleeve title, which release          |
| **encoding**           | one set of bytes                  | format, bitrate, length, cutoff, cliff, levels |
| **holding**            | `(device, encoding)`              | the locator, and nothing else                  |

They share conformed dimensions and they are **not** collapsible. A recording held as a FLAC and an MP3 is one
recording, two encodings, and possibly two holdings; a song on an album and on a compilation is one recording and two
appearances. `CopyId::On(track)` versus `CopyId::Loose(recording)` is this distinction already visible in the row
identity (#155).

**A query names its grain.** `--every-copy` (#183) is that choice made explicit at the CLI, and it exists because the
default — one row per recording — is the right grain for a question about the _performance_ and the wrong one for a
question about the _bytes_.

### Dimensions, and the two that are not simple

**Simple:** release, medium, release group, label, work. Foreign keys, nothing to argue about.

**Bridges, because the relationship is many-to-many and carries its own attributes:**

- `credit(subject, artist, role, position, as_credited, join_phrase)` — already exactly this. It is what makes _"every
  album this artist is on, including ones where they are one of several"_ a join rather than a string search, and it is
  why the printed line must not be the stored thing: `line_of` is a **rendering** of the bridge, not a column.
- `assertion(subject, concept, source, confidence)` — many concepts per subject per axis, with lineage. Already this.

**Ragged hierarchies, and there are two of them with the same shape:**

- **genre**, via `narrows(hierarchy, broader, narrower)` — what `genre@1` climbs (#79)
- **monikers**, via `artist_relations(from, to, kind)` — what `file_under@1` climbs (#174)

Both are queried _by depth_, and both are walked today — `artist_at_level` climbs a chain per call. The standard answer
is a **closure table**: `(ancestor, descendant, depth)`, derived like everything else in the projection, which turns a
walk into a lookup and serves both hierarchies with one mechanism. A concept shallower than the level asked for stands
for itself, which the closure expresses as a depth-0 row rather than as a special case in a walk.

**Alias as a dimension attribute set**, not a column: `artist_name(artist, value, kind)`. That is what #179 needed and
what `also_known_as` on a row is a flattening of — search should reach `aphextwinsucks` and nothing should print it,
which is a property of the _name_, not of the row.

### The target, stated plainly

> **A row carries ids. Every string a person reads is assembled by the query layer, and none of it is stored.**

No credit lines, no artist names, no genre labels on a row. The projection mirrors the log; the query layer renders.

### Why this is faster, and not slower, for the queries actually run

I argued the other way earlier on the strength of #123, and that was wrong: #123 measured the **absence of an index** —
"four passes over every credit in the library, per row" — and the fix was `Lookup::Built`, a map built once. It measured
nothing about assembly.

Take `artist="*campfire*"`, which is the shape most filters have:

|                                             | strings matched                     |
| ------------------------------------------- | ----------------------------------- |
| flattened — build every row, then glob each | 7,272 rows × their names            |
| normalised — glob the dimension, then join  | **~1,430** names over 1,399 artists |

Filtering a dimension and joining to the facts is strictly less work than materialising every row and scanning it, and
it gets better as the library grows, because dimensions grow far more slowly than rows. The same holds for `genre=`,
`label=` and anything else whose values are shared.

### Measured, on the first field normalised

`also_known_as` — the list of names a row could be found by — became `credited: Vec<Id<Artist>>`, with the names asked
of the library through a `Named` capability at the moment a filter wants them.

|                                                   |                                                          |
| ------------------------------------------------- | -------------------------------------------------------- |
| strings stored on rows                            | **0.82 MB**                                              |
| the same held once each                           | **0.04 MB** — 2,603 distinct values, **21× duplication** |
| assembling a credit line, through the built index | **0.78µs a row** — ~6ms for a whole-library draw         |
| the same through the _unindexed_ path             | 8.3µs a row — 61ms, and the trap #123 named              |

**The estimate this replaced was ~200ms, and it was mine.** Measuring the unindexed path first and nearly believing it
is the same mistake in miniature: the cost of assembly is a question about the index, not about assembly.

It also **deleted an index**. `Lookup::Built` carried an alias map so that building a row's names was a lookup rather
than a walk, and `Reference::aliases_by_artist` existed to fill it. Normalised, neither has a caller — there is no
per-row name to build. Derived state that no longer needs maintaining is the shape of the whole argument.

And two facts changed tier: `AliasAsserted` and `HeadingRetired` were wide _because a row stored the names_, and are now
narrow, touching one artist record and no row at all.

**Where normalisation does cost** is rendering, and sorting by something derived: a tree sorted by `file_under` has to
assemble every line. Two things bound it — the TUI draws a screenful rather than 7,272 rows, and a per-query memo (the
descendant of `Lookup::Built`) **cannot go stale**, because it lives for one query rather than being maintained across
facts.

### And it is a representation argument before it is a performance one

**A flattened projection can represent a state that is wrong by definition.** A row holding `file_under = "Aphex Twin"`
while the artist table holds `AFX` is expressible, and nothing in the type forbids it. Invalidation is then a
_discipline_ keeping the model out of states its own shape permits — and a discipline is what this project keeps
replacing with structure.

Normalised, the name lives in one place. The disagreeing state is **not expressible**: there is nothing to invalidate,
nothing to test, and nothing to get out of step.

This is not a new principle here, it is the same one applied a layer down:

| the second source that could disagree   | what was done                                                                                                        |
| --------------------------------------- | -------------------------------------------------------------------------------------------------------------------- |
| a path, as a second identity for a file | identity became the **audio**, and `history.toml` keyed by path cost a four-star rating on a file that still existed |
| tags, as a second account of a library  | tags became a **serialisation**, consumed at import and never consulted again                                        |
| a gain compared approximately           | stored as an **integer** in hundredths, because a value that compares approximately cannot be a key                  |

**What is honest about the alternative:** a flattened projection with correct invalidation is not wrong at rest, and
`advance(p, facts) ≡ rebuild(facts)` is directly assertable — `Catalogue` derives `PartialEq` and a rebuild is 44ms. It
is a real test and it belongs in #193 either way. But it is true _by test_ over sequences of facts, where the normalised
version is true _by construction_, and the test space is combinatorial rather than 53 cases.

The coupling is the sharper problem. `ArtistNamed` is wide **because a row stores the name** — a dependency between two
files with nothing linking them. Change what the projection stores and a classification elsewhere becomes wrong,
silently, with every test still green.

**The rule is not "never derive".** A closure table is derived and stored, and that is fine. The rule is **never patch a
derivation incrementally**: a closure rebuilt whole whenever `narrows` changes cannot drift, and one patched edge by
edge can.

It also dissolves most of §3a's tier problem: `ArtistNamed`, `LabelNamed`, `ReleaseGroupRetitled` and the credit facts
are wide **only because** something derived is stored on the row. Normalised, they are narrow, and `Blast` stops being
something anyone has to maintain with care.
---

## 6. The Python boundary

### What is actually wrong today, and what is not

**`bronze.py` is fine and should stay.** It is a sqlite cache of what MusicBrainz and Discogs _answered_, keyed by
request, living in `$XDG_CACHE_HOME`. Its own header states the test that makes it legitimate: _"Deleting this may cost
time. It must never cost a fact."_ It is not library state and it is not a second store of library state.

**No tool reads the log.** They take a work list produced by `gmuse shelf query` and emit proposal files consumed by
`gmuse shelf apply`. The outer boundary is already file handoff and it is roughly right.

What _is_ wrong is that the handoff has **no contract anyone can fail against**. A tool writes JSONL by hand; whether
the fields are the ones `apply` reads is discovered at apply time, on a file somebody spent an afternoon producing. Two
spellings of one album id (`musicbrainz album id` against `musicbrainz_albumid`) is exactly this class of bug and it
cost eight albums their genre.

### The shape

A PyO3 extension module — `gmuse_core` — built with maturin, that _is_ L3 for Python:

```python
from gmuse_core import Library

lib = Library.open()                     # L1 read handle, no walk, no tags
for row in lib.query('artist="AFX"'):    # L2, the same evaluator as :filter
    ...
lib.propose([Proposal.Identified(...)])  # typed, validated at the boundary
```

- **One evaluator.** `lib.query` is `filter.rs`, so a spec the `:` line accepts, Python accepts, by construction rather
  than by two parsers agreeing today.
- **Typed proposals.** A malformed proposal fails where it is constructed, with the field named, not three hours later
  in `apply`.
- **No second reader of anything.** The tools stop knowing what the work-list JSON looks like, because there is no JSON.

**Practicalities**, from current PyO3/maturin guidance: build with maturin; enable the `abi3` feature with a minimum
version (`abi3-py311`) so one wheel covers every later CPython and the wheel does not need rebuilding per interpreter;
generate `.pyi` stubs with `pyo3-stub-gen` so the tools keep their type checking, which this project runs.

**What stays in Python:** everything it is better at and that is not library state — HTTP and rate limits, MusicBrainz
and Discogs parsing, `bronze`, tag writing via mutagen, the review TSVs, and the judgement calls that produce them.

---

## 7. What this does not change

Worth stating, so the plan is not read as bigger than it is.

- **The log stays the source of truth.** Convergent ids, append-only, no last-writer ambiguity. It is not replaced by a
  database; it is _projected into_ one. At 217ms to parse it is not the cost anyway.
- **`Fact` does not change.** No new variants are required by any of this.
- **The query language does not change.** Same filter, same levels, same fields.
- **"Reported, never decided" does not change.** None of this makes a tool decide anything it does not decide now.
- **`verify` does not change.** It still reads every byte, because there the guarantee is truth and not speed.

---

## 8. Sequencing

Nine steps. Each leaves the tree working and each is independently reviewable. **(1) is worth doing whatever is decided
about the rest**, and buys the time to do the rest properly.

1. **Cache the whole `Scanned`, not just `Measured`.** Same stat guard and invalidation as #188. `populate` drops to
   roughly `reconcile`'s 130ms; a warm query to ~600ms. Buys time; changes no structure.
2. **Name the boundaries where they already are.** Move today's code into `src/facts/`, `src/projection/`, `src/query/`
   with no behaviour change, and add the import tests from §4 asserting _no worse than today_ — the ratchet `src/ui/`'s
   `Store` count already uses. This is where the mixing becomes _visible_ rather than merely true.
3. **Decide each fact's blast radius** — narrow, wide, or rebuild (§3a). Cheap to do, and step 4 is built wrong without
   it: a projector that cannot apply one fact makes every TUI edit a full replay.
4. **Give the projection a position and a rebuild.** `Projector::advance` and `::rebuild` over the existing in-memory
   `Catalogue`, with the position recorded. No store change yet; the contract lands first.
5. **Persist the projection** as a postcard snapshot (§5). Now a read stops folding 279,340 records.
6. **Cut reads over.** `shelf query`, the TUI and the work lists read L1 only. The import tests from (2) stop being a
   ratchet and become assertions: `std::fs` leaves `src/query/`.
7. **Give the filesystem its door** — the scan #64 needs regardless, plus a projection that reports staleness rather
   than hiding it. **(6) and (7) land together**; (6) alone is a regression.
8. **`gmuse_core`**, the PyO3 module: open, query, propose, with stubs.
9. **Move the tools onto it**, one at a time, deleting each hand-rolled work-list reader as it goes.

---

## 9. What needs deciding

- ~~**The projection store**~~ — **settled by measurement in §5**: a postcard snapshot. 5.9 MB, 9ms to load, no new
  dependency. The `Projection` trait keeps the swap cheap if resident size ever becomes the complaint.
- **May a query be stale?** **Settled in §3a**: a read may `advance()` freely, because folding touches only the log and
  a caught-up projection costs one integer comparison. It may never scan. So a query is never stale with respect to the
  _log_, and may be stale with respect to the _disk_ — which is the right way round, and exactly what a named `scan` is
  for. What is left is cosmetic: whether a read that had to fold a large gap says so.
- **Does `--unidentified` keep a walking mode?** It is the one caller with a real claim on the filesystem, and the
  honest answer may be that it is a named operation rather than a query.
- **How far does `gmuse_core` go?** Query and propose is the minimum. Whether it also exposes the projection's rows to
  pandas-shaped analysis is a bigger question and not one this needs answered.

---

`layers.md` — which system may talk to which · `library-api.md` — the operations that cross to the filesystem ·
`domain-model.md` — what a row is · #188 (the measurement cache) · #189 (this decision) · #64 (the scan that must exist)
