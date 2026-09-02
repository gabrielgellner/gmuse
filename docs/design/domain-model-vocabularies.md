# Vocabularies and identity: which ones get to be entities

**Status:** supplementary design input for `domain-model.md`, following the review in `domain-model-review.md`. This
resolves how controlled vocabularies (genre, key, credit role, derivation kind, …) are represented — which get their own
identity as maintained reference artifacts, which stay compiled-in, and why. It answers the draft's open questions 3 and
8 directly and builds on mechanisms the draft already has (§3 identity/bindings/redirects, §2 child-side rows, §5
hierarchies). Rationale is stated so the rule can be extended to vocabularies not listed here.

---

## 1. The promotion test

The draft's own entityhood test is "things point at it." Genre already passes three ways: `GenreAssignment` rows point
at genres, hierarchy edges point at genres, and the 37 `RENAME` entries in `tools/genre/propose.py` are alias bindings
by another name. Anything carrying assignments, structure, _and_ aliases is an authority record and should be modeled as
one.

The general rule:

> **A vocabulary is an entity codelist when it is _data_** — it grows at runtime, users extend it, it needs aliases,
> hierarchy, merge, or external bindings. **It is a Rust enum when it is _code_** — program behavior branches on the
> variants and a new variant should be a compile error until that behavior is written.
>
> Operational form: **does adding a value require writing behavior?** Enum. **Only grouping and filtering?** Codelist.

## 2. Genre, fully promoted

```rust
struct Genre { id: Id<Genre>, name: String }        // id meaningless, label mutable

struct HierarchyEdge {
    hierarchy: Id<Hierarchy>,   // which hierarchy — one list can carry several
    parent: Ref<Genre>,
    child: Ref<Genre>,
}

struct ExternalBinding {
    subject: Id<Genre>,
    scheme: BindingScheme,      // Discogs | MusicBrainz | ...
    code: String,
}
```

Every existing mechanism applies unchanged — this is the evidence the shape is right, not a coincidence:

- **Merge by permanent redirect (§3).** "IDM" and "Braindance" turn out to be one concept → `id_redirect`, nothing
  rewritten. This is what the `RENAME` entries have been simulating in Python dictionaries.
- **Child-side rows (§2).** Hierarchy membership is edge-row existence — unions across devices; there is no
  `Genre.children` collection.
- **Foreign identifiers are bindings, not keys (§3).** Discogs and MusicBrainz genre ids bind onto locally minted
  concepts exactly as MBIDs bind onto artists. This _answers Q3: the vocabulary is ours_, with external schemes as
  exact-match bindings — the `skos:exactMatch` idea without the RDF.
- **Hierarchy separate from the list (§5, already decided).** The SDMX move, contra SKOS's `broader`-on-the-concept: one
  genre list carries a browse hierarchy and a filter hierarchy simultaneously without the concepts changing.

The 169 assertions in `propose.py` (`IMPLIES` → hierarchy edges, `RENAME` → redirects/aliases, `BROAD` → top concepts of
the browse hierarchy) migrate into model data and the Python dict stops being load-bearing — the same promotion §5
already performs on the compound genre string.

## 3. The verdict table

| vocabulary                                       | representation              | rationale                                                                                                                                                        |
| ------------------------------------------------ | --------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| genre                                            | **entity codelist**         | open-ended, hierarchical, aliased, externally bound                                                                                                              |
| musical key                                      | **codelist, compiled-in**   | closed by music theory (24/30 codes) but _structured_: circle-of-fifths distance and Camelot are traversals over it; ships with the binary, no runtime extension |
| credit role                                      | **entity codelist, seeded** | grows in practice (remixer, conductor, soloist, "performs"); behavior rarely branches on it; seed known roles, let ingest mint provisionals                      |
| `DerivationKind`                                 | **plain enum**              | closed, small; player logic branches on it (a DjMix expands to a tracklist, a Sample does not) — a new kind must be a compile error until its behavior exists    |
| `Format`, `Confidence`, `MediaFormat`, SCD class | **plain enum**              | closed, no structure, exhaustively matched                                                                                                                       |
| bpm, cutoff, length, bitrate                     | **attribute — nothing**     | no vocabulary exists; the draft's three-kind table (§2) already guards this                                                                                      |

**Defend the enum row hardest.** Making `DerivationKind` a codelist would make the set of derivation _semantics_
user-editable data, while the code that implements those semantics stays closed — the two would drift. The promotion
test exists precisely to stop "everything gets identity," which is the triple store arriving through a different back
door.

**Key + Camelot (answers Q8, confirming the draft's own instinct):** one key codelist; Camelot is a _second
hierarchy/ordering over it_, standard notation the display default elsewhere. Not a second vocabulary, so no mapping
table to maintain — the mapping _is_ the hierarchy artifact.

## 4. The codelist as a maintained artifact

The genre codelist plus its hierarchies plus bindings is small, slow-moving, and precious. Treat it as the draft treats
the catalogue, but more so:

- exportable as a single file (TOML/JSON — #40's call on format);
- kept in git, diffed after tagging passes;
- snapshot before a bulk retag = a poor man's codelist version, which is all the versioning a one-owner library needs.

This is the personal-scale version of an agency publishing a codelist release, and it makes review workflows
(`genre-uncertain.tsv`) diffs against a versioned artifact instead of a shadow database.

## 5. What is deliberately NOT imported from SDMX

Take: the _Identifiable_ rung (things point at it → it has an id), hierarchy as a separate artifact from the list,
external bindings.

Leave: the **concept-scheme / codelist split** (concepts as meanings, codes as representations). At agency scale that
separation earns its keep; at personal scale it is a fifth wheel — a genre concept and its code are the same row, and
`ExternalBinding` covers the "same concept, other people's codes" case the split exists for. Consistent with §3's ruling
that Versionable and Maintainable collapse.

## 6. Why the growing link-web stays coherent

With artist relations, recording derivations, genre assignments, hierarchy edges, and external bindings, the graph is
getting dense. It does not dissolve into soup because every link is a **typed struct with constrained endpoints** — the
decision made when `TypedEdge` was declined. Each edge kind has a schema, a merge class in §4's table, and a home. The
resulting architecture is a structural-metadata layer (codelists, hierarchies, bindings — maintained artifacts) under an
observation layer (recordings, events) that references it — the SDMX shape, in Rust structs, with exactly the rungs of
the ladder this library needs and no more.
