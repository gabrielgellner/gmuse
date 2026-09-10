# The query language, and the one thing a view is

**Status: proposal, revised once under review.** Nothing here is built. `domain-model.md` says what the facts are;
`tree-view-design.md` says what a `ViewSpec` is. This says what you can ask, how questions compose, and why five nouns
collapse into one.

---

## 1. One noun, two inhabitants

A **view** yields recordings, and comes in two shapes:

- a **set** — membership from a predicate, derived, unordered until a sort is applied
- a **sequence** — membership and order stored as facts, and it **admits duplicates**

Everything the player calls something else is one of those two, differing only in how it is combined with what is
already active:

| was called      | is                                       |
| --------------- | ---------------------------------------- |
| playlist        | a **sequence**                           |
| the queue       | a **sequence** the play head consumes    |
| smart playlist  | a **set**                                |
| catalog         | a set **or**-ed into the active one      |
| `:filter`       | a set **and**-ed into the active one     |
| `exclude` (#28) | a set **and not**-ed into the active one |

**The bag/sequence distinction is not decoration, and an earlier draft of this document lost it.** A predicate cannot
express multiplicity, and playing one recording twice in a set is ordinary — so "the queue is just a view" is false if a
view is only a predicate. The quickfix parallel in §10 makes the same point against that draft rather than for it:
quickfix is a _list_, with duplicates and an order, not a set.

This lines up with `tree-view-design.md`'s existing split rather than replacing it:

```rust
pub struct ViewSpec { source: Source, filter: Option<Predicate>, levels: Vec<Level>, leaf: Option<SortExpr> }
pub enum Source { Query, Playlist(PlaylistId) }   // set | sequence
```

`Source` **is** the set/sequence choice, already named. What this document adds is that the two compose, that `Source`
is the only axis that matters, and that `:freeze` is the coercion between them.

## 2. Composition is an operator, not a type

Catalogs union because they are catalogs; `:filter` intersects because it is a filter. The operator is baked into the
noun, which is why #28 found a "missing cell" — a persistent _narrowing_ had nowhere to live, and a `not christmas`
catalog would have **widened** the library rather than narrowed it.

There is no grid. There is one verb and three operators:

```
:view dance                  set it
:view +jazz                  or       — widen
:view &year>=2015            and      — narrow
:view -genre="*christmas*"   and not  — exclude
```

`active_catalogs = ["general", "dance"]` becomes `general or dance`, and the missing cell is `-`.

**`+` is defined on sets only.** `&` and `-` are safe on a sequence because the result is a subsequence of the left
operand, so its order survives untouched. `+` has no defensible answer: the order of `crateA + crateB` is not
concatenation, or interleaving, or a re-sort — a stored position is hand-assigned and means nothing beside another
list's. So combining a sequence with `+` **demotes it to a set**, and the interface should say so rather than silently
picking one of the three.

**The stack is a left fold**, so `A and (B or C)` is unreachable from it — and it is the first thing anyone will try.
Parentheses live in the written form (§6), which is what `:edit` is for.

**The composed thing is itself a view**, anonymous until named. `:playlist save mornings` on the current one is how a
composition acquires a name — which is exactly Roon's _save this Focus as a bookmark_, and the feature that made Focus
worth having.

---

## 3. Static and dynamic, and the operation between them

A dynamic view is a question; a static one is an answer someone kept. **`:freeze` is the crossing**, and it already
exists:

```rust
Command::Freeze => match ui.open.take() {
    Some(Open::Rule(name)) => "froze {name} — {n} tracks; `:playlist save <name>` keeps it",
    other => "only a rule can be frozen",
}
```

It reports rather than writing, and it already refuses on a static list, which is the right error. Finishing it means
writing the membership as `CollectionEntry` facts.

**A frozen view records what it was frozen from.** Otherwise "this crate was `rating>=4 and genre+=techno`, in March" is
unanswerable, drift is invisible, and re-freezing is a retype rather than a keystroke. The provenance the model already
carries is the place for it.

The reverse direction — a static list that "reduces as played" — is **not a third kind**. It is
`in "crate" and play_count=0`, and it is dynamic. Naming it would be inventing a noun for an expression.

---

## 4. Editing a view is editing the facts that made it true

This is the **updatable view** problem, and SQL's answer half-transfers. A SQL view is not directly updatable through
joins, aggregates, set operators, `GROUP BY` or `DISTINCT`; where the translation to base tables is ambiguous you need
an `INSTEAD OF` trigger, or it is not updatable at all.

**But ambiguity is the wrong test here, and a draft of this document used it.** `year >= 2015` translates unambiguously
— assert a year — and dragging a recording into that view must absolutely not rewrite its release date. The distinction
is not whether the translation is clear. It is **who is entitled to author the fact**:

| category                  | example                              | affordance                                           |
| ------------------------- | ------------------------------------ | ---------------------------------------------------- |
| **observed**              | `play_count`, `verdict`              | none — there is nothing a person asserts             |
| **observed, correctable** | `year`, `mbid`, `title`              | not by default; an explicit correction, never a drag |
| **authored**              | collection membership, genre, rating | yes, and it names the fact                           |

> **An edit to a view is an edit to the facts that made it true. Only _authored_ facts get the affordance, and the
> interface names the fact before it writes it.**

iTunes and Roon both dodge this by making derived lists strictly read-only. Going further is possible here for one
reason: **membership is caused by named facts**, so the translation is inspectable rather than magic.

### Removal is an override, never a retraction

Dropping a recording out of `genre = techno` **cannot be a delete.** The log is append-only and merges by union, so a
deleted `GenreAssigned` returns on the next sync — and if it carried `basis = discogs`, deleting it would destroy the
provenance that says where it came from.

What is needed is an **override event that shadows the assertion**, with precedence by basis: a person's assertion beats
an import. That is a schema question and not a UI one, and it is the difference between an edit that survives a resync
and one that does not. The model already has the shape for it — `EncodingBound` carries what it replaced, and `Merges`
has `bar`/`unbar` for exactly "I have decided otherwise".

The rule that keeps all of it safe: never a control that quietly does nothing. Either the fact is named and written, or
the affordance is not offered.

## 5. What the model demands that the language cannot say

Today's selectors are `Field | Any | History` over a flat `Track`. File-shaped, one hop. The model is Work → Recording →
Release → Encoding → Holding, with credits carrying roles, genre assignments carrying basis and confidence,
measurements, per-device holdings, and derivations.

Currently inexpressible, and all of them wanted:

```
any(encoding).verdict = suspect                 the re-buy list — the point of the FFT pass
holding.device = "asnas" and not holding.here    on the NAS, not here
credit[role=remixer].artist = "Nolah"
genre.basis = discogs and genre.confidence != confirmed     the review queue
not recording.mbid?                              the residue #89 is about
genre+ = electronic                              anything narrower, at any depth
derived+ = <recording>                           every version, through any chain
```

Two capabilities are missing: **traversal** across entities, and **quantification and closure**.

---

## 6. Surface and core

**The core is conjunctive triple patterns with unification.** A variable appearing in two clauses must be the same
thing, so joins are never written:

```
[?rec :recording/encoding ?enc]
[?enc :encoding/verdict :suspect]
```

That is Datalog's syntax and SPARQL's semantics — they are the same construct in different brackets. It suits this model
because **the model is already reified**: `Credit` and `GenreAssignment` are entities rather than properties, decided
that way because a bare reference had nowhere to put basis and confidence. Cypher's advantage is properties on edges;
having reified for provenance, there is none left to take.

**The surface is dotted paths**, because nobody types triple patterns into a status line:

| surface                             | core                                                                     |
| ----------------------------------- | ------------------------------------------------------------------------ |
| `any(encoding).verdict = suspect`   | `[?rec :recording/encoding ?e] [?e :encoding/verdict :suspect]`          |
| `credit[role=remixer].artist = "X"` | `[?c :credit/on ?rec] [?c :credit/role :remixer] [?c :credit/artist ?a]` |
| `genre+ = electronic`               | `[?rec :genre/concept ?c] [?c :concept/broader* :electronic]`            |

### The surface is strictly weaker, and that is a choice

**Dotted paths are a tree-shaped fragment rooted at the recording.** Every clause hangs off the subject, so two paths
cannot meet at a shared variable that is not the root. This is unreachable in the surface and ordinary in the core:

```
recordings whose remixer also produced something else I hold
```

The restriction is worth choosing — it is what keeps a query typable in one line. What must not be pretended is that the
round trip is symmetric: **`:edit` is one-way.** Once a query is edited into a non-tree shape it cannot be rendered back
as a stack of droppable terms.

So a core-edited view becomes **opaque**: the status line shows its name or its raw text, and the per-term affordance is
withdrawn rather than offered and wrong. That is §4's rule — never a control that quietly does nothing — applied to the
query surface instead of the edit surface.

---

## 7. Quantification is explicit, because otherwise the language lies

`not encoding.verdict = good` — no encoding is good, or some encoding is not? A recording with a FLAC and a bad MP3
answers differently, and both readings are useful.

```
any(encoding).verdict = suspect     I have a bad copy
all(encoding).verdict = suspect     every copy I have is bad
```

Implicit `any` is a fine default. What is not fine is `not` binding to the wrong scope silently. beets has this problem
and documents the workaround rather than the fix: multi-valued tags "must use a regular expression search", which is a
regex standing in for a missing quantifier.

---

## 8. Closure, not rules

Two things in this domain are recursive, and both are transitive closure of a single predicate:

- **the genre hierarchy** — `genre = electronic` must reach a techno record
- **derivation chains** — a remix of an edit of an original

**`broader` is not transitive, and an earlier draft of this document said it was.** SKOS declares `skos:broader` a
_sub-property of_ `skos:broaderTransitive` precisely so that it is not: it is "only used to assert a **direct** (i.e.
immediate) hierarchical link between two SKOS concepts", which "allows the information about direct hierarchical links
to be preserved whilst also providing a mechanism for conveniently querying the transitive closure".

That distinction is required as soon as the hierarchy is **displayed** rather than queried — drawing a genre tree needs
immediate parents, and a schema that lost the difference could not draw one. So the relation stored is the direct link,
and closure is an operation applied to it at query time.

SPARQL answers closure with property paths (`*` zero-or-more, `+` one-or-more, `?` zero-or-one, `|` alternatives, `^`
inverse, `/` sequence). Cypher answers it with variable-length patterns. Datalog answers it with general recursive
rules, which is strictly more powerful.

**Closure is enough.** Alternatives cover the awkward case — `(remix|edit)+` is "through remixes or edits, any depth" —
and nothing in the domain needs a rule with a body. So the core is SPARQL's core minus most of SPARQL: far smaller to
build, and far easier to explain.

### One character, one meaning

An earlier draft used `+` for both `genre+` (glossed as _reflexive_ — the concept or anything narrower) and `derived+`
(one-or-more hops, since a recording is not a derivation of itself). Same glyph, `*` semantics in one place and `+`
semantics in the other — and the justification, _"`+` is SPARQL's own operator"_, then does not hold, because the glyph
was borrowed and the meaning changed. That is the expensive kind of borrowing.

**`+` means one-or-more, as it does in SPARQL, everywhere.** Reflexive gets `*`:

```
genre  = electronic       exactly that concept
genre* = electronic       it, or anything narrower, at any depth
derived+ = <recording>    every version, through one or more hops
```

`genre*` is very slightly worse to read than `genre+` and worth it, because the two operators now mean in gmuse what
they mean everywhere else.

---

## 9. Presentation is a property of the view, not a place to go

```rust
pub struct ViewSpec { source, filter, levels: Vec<Level>, leaf }
```

Tree is `levels: [artist, album]`. Flat is `levels: []`. **The numbered views 1 and 2 are two hardcoded presets of one
field that already exists**, and treating it as a field generalises:

```
levels: []                  flat
levels: [artist, album]     the tree as it is now
levels: [genre]             by genre — not expressible today
levels: [year]              by year
```

So switching representation happens **in place**. It is not a different screen; it is a setting on the thing being
looked at, the way `foldmethod` is a setting on a buffer.

**And a level has a reading outside the browser** — `gmuse shelf tree --levels 'genre@1,album'` (#182). Until it did,
the one thing a level decides — which rows fall under which heading — could only be asked _of_ the TUI and only be
answered _by_ looking at it, so the setting was untestable from anywhere a script or a person could stand. The command
is read-only and groups through the same `Tree::grouped` with the same member set and the same smart artist sort the UI
starts with; a disagreement between it and the pane is a bug in one of them rather than a difference in how they were
asked. `--levels flat` is refused rather than printed: a tree of no levels is one row per track, which is `shelf query`,
and two ways to answer one question is the shape this project removes (#113).

It reads back what the taxonomy actually holds, which is its first use. `genre@1` names 40 headings and `genre@2` names
several hundred — but `Acid Techno`, `Alternative Hip Hop` and `Ambient Techno` appear under **both**, because a concept
shallower than the level asked for stands for itself rather than vanishing. That is the ragged case working as designed,
and it is also a list of terms nobody has placed yet.

**It does not generalise for free**, and an earlier draft said it did. A recording with two genres appears under two
branches, so `levels: [genre]` is not a partition — and the navigator's nodes are half-open ranges over one sorted
array, where a row cannot be in two places at once.

`tree-view-design.md` §6 already settles this, which is why the cost is bounded rather than open. Rows are duplicated by
unnesting before keying, so _every row still has exactly one position_ and the tree stays well-formed; what breaks is
the row ↔ track bijection, and the `multi` flag drives the three call sites that care:

| behaviour       | single-valued | multi-valued               |
| --------------- | ------------- | -------------------------- |
| subtree count   | `range.len()` | `COUNT(DISTINCT track_id)` |
| enqueue subtree | slice         | dedup on id, order kept    |
| reveal current  | one `NodeRef` | highlight all N            |

That doc also names the failure exactly: without the flag "a subtree reports 42 tracks and enqueues 37". So the
precondition is that a level is either single-valued or declared `multi` — three call sites and a flag, not free, and
not unsolved.

---

## 10. Buffers, not tabs

The numbered views conflate two axes:

| view       | which set       | how drawn           |
| ---------- | --------------- | ------------------- |
| 1 Tree     | the active one  | **grouped**         |
| 2 Sorted   | the active one  | **flat**            |
| 3 Playlist | the open list   | flat                |
| 4 Queue    | the queue       | flat                |
| 6 Filters  | the saved rules | a list of _queries_ |
| 7 Guide    | not a set       | prose               |

1 and 2 are one set drawn two ways. 3 and 4 are different sets drawn the same way. The tabs are cmus's shape, inherited;
the navigation vocabulary is vim's, and where they disagree the navigation should win — §1 makes vim's vocabulary the
point of the exercise.

The mapping is already half-built:

| gmuse                          | vim                                                       |
| ------------------------------ | --------------------------------------------------------- |
| a view                         | a buffer                                                  |
| `g<Tab>` (`previous_view`)     | `Ctrl-^`, the alternate buffer — **already exists**       |
| `:view <Tab>` over saved views | `:b <Tab>`                                                |
| `levels`                       | a buffer-local option                                     |
| **the queue**                  | **quickfix** — a list with a special role, still a buffer |
| view 6                         | not a tab, a **picker**                                   |
| view 7                         | `:help`                                                   |

The quickfix parallel is the useful one: quickfix is consumed by `:cn`, has dedicated navigation, and is still an
ordinary list you can open. That is exactly the queue's odd status, solved without a tab.

**The number keys stay.** They stop being built-in views and become bindings to views, which is where §6's one command
language wants them anyway:

```toml
[keys]
"1" = "view library"          # levels: [artist, album]
"2" = "view library --flat"
"3" = "view open"
"4" = "view queue"
"5" = "view rebuy"            # now possible, and yours
```

Nothing gets slower. What is lost is the hardcoding, not the muscle memory.

**What genuinely needs two places at once** — digging in one list while collecting into another — is `g<Tab>` for the
flip and splits for the rest. Splits are the larger feature and the right one, wanted for moving recordings between
lists regardless.

---

## 11. Writing a query: three tiers, one language

```
:view genre+=techno            one line, narrows the active view
:view                          shows the stack, each term droppable
:edit                          opens the composed query in $EDITOR
```

Simple things inline; composed things in an editor. The middle tier is Roon's Focus — stack criteria, invert any of
them, save the result — which is the interaction this design is copying on purpose, because it is the one that has been
shown to work.

That the escape hatch is the **same text** is what makes the tiers one language rather than three. It argues against a
structured save format like Navidrome's JSON: text all the way down, or the editor becomes a translation.

---

## 12. Prior art

| source                  | what is taken                                                                     | what is not                                        |
| ----------------------- | --------------------------------------------------------------------------------- | -------------------------------------------------- |
| **SPARQL**              | triple patterns, unification, property paths (`+ * ? \| ^ /`)                     | SELECT/CONSTRUCT/OPTIONAL, IRIs, the whole stack   |
| **Datalog / Datomic**   | that joins are unification, and the EAV fit                                       | general recursive rules — closure is enough        |
| **Cypher / GQL**        | the check that a property graph is the other option                               | its model: this one is already reified             |
| **SQL updatable views** | the editability rule, restated for facts                                          | `INSTEAD OF` triggers — an unwritable view says so |
| **SKOS**                | direct `broader` plus a closure operator; Concept and Collection are **disjoint** | the RDF serialization                              |
| **DCAT**                | catalogue as enumerated membership; theme as a property                           | its `Catalog` as a UI concept                      |
| **Roon Focus**          | stack, invert, save as a name                                                     | —                                                  |
| **beets**               | `field:value`, ranges, `-` negation                                               | regex as a stand-in for a quantifier               |
| **iTunes / Navidrome**  | any/all nesting, sort, limit                                                      | derived lists being read-only; a JSON format       |
| **vim**                 | buffers, the alternate buffer, quickfix, buffer-local options                     | tabs                                               |

---

## 13. Not decided

- **Named views referencing named views.** Once `in "crate"` is a legal term, a saved view can name a saved view — SQL
  views over views. Two things follow and neither is optional: **cycle detection**, or `:view` loops forever; and a
  choice between **by-name** (late binding, so editing the referenced view propagates) and **by-value** (a snapshot at
  save time, which is what Roon bookmarks effectively are). This decides what the saved-view store has to be, so it
  comes before building one.
- **`view` versus `focus`** as the word. `view` is free once the numbered views are gone, and is the preferred term; the
  argument against is that it then names both the buffer and the composition being built inside it.
- **Whether `:freeze` copies the order or only the membership.** Copying order lets a sorted result be hand-reordered,
  which is probably wanted — and makes the frozen order a fact that immediately diverges from the query's.
- **How a sequence's positions are stored**, so that inserting between two entries does not rewrite every row after it.
  A dense integer per entry is the obvious answer and the wrong one under a log that merges by union.

### Settled in review, recorded so they are not re-argued

- **`limit` and `sample` are not predicates.** They are post-order operations belonging at the end of the pipeline,
  rendered as a terminal clause rather than a droppable term. That is why they compose badly with `and`: they are not
  the same kind of thing, so they do not have to.
- **A view scoped on play history is evaluated once at open** and refreshed on an explicit gesture, or the row under the
  cursor vanishes because you played it. Quickfix behaves exactly this way, and §10 has already committed to that model.
- **`all(...)` lowers to an anti-join**, and negation combined with closure is where a naive lowering gets slow. Worth
  writing into `library-api.md` before it is discovered by measurement.

## 14. On reading this document

Three claims in the first draft were wrong, and all three were **asserted rather than derived**: §1's table of five
nouns collapsing cleanly, §9's "generalises for free", and §12's "SKOS `broader` is transitive". Each was well-shaped
enough to read as settled, and each was where a problem turned out to be.

So the useful lens for a revision is not _is this well argued_ but **is this claim earned, or is it just well formed**.
A sentence that names its evidence — a spec quotation, a line of code, a measured number — has been checked. A sentence
that only reads well has not.

---

## 14. Related

`domain-model.md` (what the facts are) · `tree-view-design.md` (`ViewSpec`, `levels`) · `library-api.md` (query and
apply) · `layers.md` (who may ask) · #90 (catalogs are queries) · #28 (the missing cell) · #52 (genres, which `genre+`
depends on) · #76 (collections) · #54 (the picker)
