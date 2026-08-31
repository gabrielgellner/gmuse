# Query → Tree View

A single mechanism for rendering any row set as a navigable tree, with per-level control of grouping identity, ordering,
and display, and defaults that make the common cases require no configuration.

---

## 1. The model

One flat entity (`track`), one flat row set, and a **view spec** that names an ordered list of grouping levels. The tree
is not stored. It is derived by sorting rows and partitioning them on runs of equal key.

```
rows ──sort──▶ sorted rows ──partition on key[depth]──▶ tree
```

Everything below exists to make that partition step total and unambiguous.

### The invariant

> Partitioning a row sequence at every index where `ident[depth]` differs from the previous row yields a **tree** if and
> only if the rows are sorted such that every distinct `ident` value at every depth occupies one contiguous run.

Uniqueness of nodes is a _corollary of the sort_, not a separate check. There is no validation pass; the `ORDER BY`
establishes the property.

The sort tuple must interleave sort and identity:

```
(sort₀, ident₀, sort₁, ident₁, …, sortₙ, identₙ)
```

Sorting on `sort` alone is insufficient. Two distinct groups can share a sort value — "Greatest Hits" by two different
artists under a `sort: album_title` level — and their rows would interleave, producing two runs for each ident.
Appending `ident` as a tiebreak at each position removes this.

---

## 2. Types

Two layers. `ViewSpec` is the unresolved, user-authored form with optional fields. `ResolvedView` is what the rest of
the system consumes: every default filled in, every `Option` discharged. Nothing downstream handles absence.

```rust
// ─── Layer 1: authored ────────────────────────────────────────────────

pub struct ViewSpec {
    pub source: Source,
    pub filter: Option<Predicate>,
    pub levels: Vec<Level>,
    pub leaf: Option<SortExpr>,
}

pub enum Source {
    /// Derived from the library. No intrinsic order.
    Query,
    /// Hand-ordered. Carries an intrinsic `position` column.
    Playlist(PlaylistId),
}

pub struct Level {
    /// Grouping identity. Required — this defines the tree's shape.
    pub ident: Expr,
    /// What the user sees. Defaults to `ident`.
    pub display: Option<Expr>,
    /// Ordering among siblings. Defaults per `Source` (§3).
    pub sort: Option<SortExpr>,
    /// Whether `ident` may yield multiple values per row (§6).
    pub multi: bool,
}

pub enum SortExpr {
    /// Ordered by a value on the row itself.
    Row { expr: Expr, dir: Dir, collate: Collation },
    /// Ordered by an aggregate over the group, partitioned by this
    /// level's `ident` and all ancestor idents.
    Agg { func: AggFn, expr: Expr, dir: Dir },
}

pub enum AggFn { Min, Max, Count, Sum }
pub enum Dir { Asc, Desc }

pub enum Collation {
    /// Case-insensitive, Unicode-aware, digit runs compared numerically.
    Natural,
    Binary,
    Numeric,
}

// ─── Layer 2: resolved ────────────────────────────────────────────────

pub struct ResolvedView {
    pub source: Source,
    pub filter: Predicate,          // `TRUE` if absent
    pub levels: Vec<ResolvedLevel>,
    pub leaf: SortExpr,
}

pub struct ResolvedLevel {
    pub ident: Expr,
    pub display: Expr,
    pub sort: SortExpr,
    pub multi: bool,
}

pub fn resolve(spec: ViewSpec) -> Result<ResolvedView, SpecError>;
```

`resolve` is the only place defaults exist. It is also where the errors live: an `Agg` sort at the leaf level, a `multi`
level below another `multi` level if you choose to forbid that, an `Expr` referencing `position` under `Source::Query`.

---

## 3. Sort defaults

`sort` is never absent at runtime, because "no ordering" is not a thing — rows arrive in _some_ order and grouping chops
that order into runs. The defaults below are what make `artist/album/track` a complete spec.

| Source     | Level | Default `sort`                   | Rationale                |
| ---------- | ----- | -------------------------------- | ------------------------ |
| `Query`    | group | `Row(ident, Asc, Natural)`       | Alphabetical browse      |
| `Query`    | leaf  | `Row((disc, track, title), Asc)` | Album running order      |
| `Playlist` | group | `Agg(Min, position, Asc)`        | First-appearance order   |
| `Playlist` | leaf  | `Row(position, Asc)`             | Preserves the hand order |

```rust
fn default_sort(source: &Source, level: &Level, is_leaf: bool) -> SortExpr {
    match (source, is_leaf) {
        (Source::Query, false) =>
            SortExpr::Row { expr: level.ident.clone(), dir: Asc, collate: Natural },
        (Source::Query, true) =>
            SortExpr::Row { expr: track_order(), dir: Asc, collate: Numeric },
        (Source::Playlist(_), false) =>
            SortExpr::Agg { func: Min, expr: position(), dir: Asc },
        (Source::Playlist(_), true) =>
            SortExpr::Row { expr: position(), dir: Asc, collate: Numeric },
    }
}
```

The playlist default is the load-bearing one. Grouping a hand-ordered playlist alphabetically discards the ordering,
which is the entire content of a playlist. `Agg(Min, position)` keeps albums in the order the user first introduced
them, and pulls stragglers up to join their group.

### `display` default

`display` defaults to `ident`. Override when the two must differ:

```rust
Level {
    ident:   Expr::field("musicbrainz_albumid"),   // stable identity
    display: Some(Expr::field("album")),           // human label
    sort:    Some(SortExpr::Row {
        expr: Expr::field("album_sort_order"),     // "Wall, The"
        dir: Asc, collate: Natural
    }),
    multi: false,
}
```

Three distinct expressions, three distinct jobs. Collapsing them into one is the mistake that is painful to undo later.

---

## 4. SQL lowering

`ResolvedView` lowers to one statement. The database does grouping-key computation, window aggregation, sorting, and
collation.

```sql
WITH base AS (
    SELECT * FROM tracks
    WHERE <filter>
),
keyed AS (
    SELECT
        track_id,
        <ident₀>   AS id0,  <display₀> AS d0,
        <ident₁>   AS id1,  <display₁> AS d1,
        ...
    FROM base
),
sorted AS (
    SELECT *,
        -- Row sort: the expression verbatim
        <sort₀>                                        AS s0,
        -- Agg sort: window partitioned by this ident and all ancestors
        MIN(position) OVER (PARTITION BY id0, id1)     AS s1
    FROM keyed
)
SELECT * FROM sorted
ORDER BY s0, id0, s1, id1, <leaf_sort>;
```

Two rules:

1. **`Agg` at depth _d_ partitions by `id0 … id_d`** — every ancestor ident plus its own. Partitioning by its own ident
   alone breaks under a repeated album title across two artists.
2. **`ident` follows every `sort` in the `ORDER BY`.** This is the tiebreak from §1 and is not optional.

Aggregate sorts are window functions, not `GROUP BY` — the row set stays at track granularity throughout, which is what
the navigator needs.

---

## 4a. Lowering without SQL

§4 lowers to one statement and lets the database do grouping-key computation, window aggregation, sorting and collation.
gmuse has no database, and §13 of the project brief closed that deliberately on measurement — a whole-library filter
pass is 0.92 ms at 20k tracks, so there is no index to be won. Everything else in this document survives that; only this
section changes.

The model is unaffected because §1 never needed SQL. "Sort rows, partition on runs of equal key" is a `Vec<TrackId>`, a
comparator, and a linear scan.

### What each construct becomes

| §4                             | Here                                                                 |
| ------------------------------ | -------------------------------------------------------------------- |
| `WHERE <filter>`               | `filter::Filter::compile(env)` used as a `retain`                    |
| `keyed` CTE                    | evaluate each `ident` and `display` per row into columnar `Vec<Key>` |
| `MIN(x) OVER (PARTITION BY …)` | a pre-pass over already-grouped runs (below)                         |
| `ORDER BY s₀, id₀, s₁, id₁, …` | one comparator over a precomputed sort tuple                         |
| `COLLATE natural`              | `library::sort_key`, plus a numeric-run rule it does not yet have    |

Three of the five already exist. `Expr` is `format::Field` — the vocabulary the format, filter and sort languages
already share — and `SortExpr::Row` is `sort::Term` with a collation attached.

### The aggregate pre-pass

`Agg` is the one construct with no local equivalent, and it has a chicken-and-egg: the aggregate needs groups, groups
need sorting, and the sort needs the aggregate. SQL hides this; here it is two passes, which is what the database does
internally anyway.

```rust
// 1. Sort on idents alone. Groups are now contiguous runs.
rows.sort_by(|a, b| idents(a).cmp(idents(b)));

// 2. Walk the runs, filling each row's aggregate slot.
for run in runs_of_equal_ident(&rows, depth) {
    let value = fold(agg.func, run.iter().map(|r| agg.expr.eval(r)));
    for r in run { sort_slot[depth][r] = value; }
}

// 3. Sort again on the full interleaved tuple from §1.
rows.sort_by(|a, b| full_tuple(a).cmp(&full_tuple(b)));
```

Pass 1 exists only to make pass 2 possible, and its result is discarded by pass 3. That is not waste worth removing: it
is the same work `PARTITION BY` does, and skipping it means computing the aggregate with a `HashMap` keyed on the ident
path — one allocation per group rather than none, and a hash per row rather than a comparison.

Levels with no `Agg` skip passes 1 and 2 entirely. The `Query` defaults in §3 are all `Row`, so **the common case is one
sort**.

### Cost

At 20k tracks and two levels: 40k key evaluations, one sort of 20k, and one linear partition scan. The sort dominates
and is a few milliseconds — the same order as the 0.92 ms filter pass, and far inside a frame. §9's cache decision
resolves the same way it does there: re-derive, and revisit when something is measured to be slow rather than assumed to
be.

Rebuilding on every keystroke of a live filter is affordable at this size. What would not be is re-evaluating `Expr` per
_frame_; the derived `Rows` is built on change, not on render.

### What is missing locally

**Natural collation.** `library::sort_key` case-folds and moves a leading article aside, which is two thirds of
`Natural`. It does not compare digit runs numerically, so `Disc 10` sorts before `Disc 2`. The sort language has the
same gap and hides it by comparing numeric _fields_ as numbers — which works for `{track}` and not for a digit inside a
title. Worth fixing once, in `sort_key`, where both would get it.

**Multi-valued idents (§6).** `LATERAL UNNEST` becomes a flat-map before the keying pass: one input row yields several
keyed rows. The `track` column then repeats, which is precisely what §6's table is about — and it is live for this
library today, whose genres are `;`-separated compound values (`Electronic; Dubstep`), so a genre level needs the split
from the start rather than later.

---

## 5. Navigator

The result set is columnar and sorted. A node is a half-open range.

```rust
pub struct Rows {
    pub ident:   Vec<Vec<Key>>,   // [depth][row]
    pub display: Vec<Vec<Key>>,
    pub track:   Vec<TrackId>,
    pub depth_of: Vec<u16>,       // terminal depth per row (§7)
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeRef {
    pub depth: u16,
    pub range: Range<u32>,
}

/// Children of `node`: split its range wherever `ident[depth]` changes.
pub fn children<'a>(rows: &'a Rows, node: NodeRef)
    -> impl Iterator<Item = NodeRef> + 'a;
```

Linear scan over a sorted slice. No allocation per node, no tree object, no `Rc`, no recursion into owned children.
Handles worked over the row set are indices, not references — the same discipline as an arena graph.

The visible viewport is a flat `Vec<NodeRef>` rebuilt from the expansion set on each change. At 100k tracks this is a
scan of a contiguous `Vec` and stays well inside a frame budget.

### Expansion state

```rust
expanded: HashSet<Vec<Key>>   // keyed on ident path, not on NodeRef
```

Keyed on the ident path so it survives re-sorts, filter changes, and library rescans — the same reason a file explorer
keys on path rather than row number. Storing `NodeRef` here would invalidate on every mutation.

### Filtering

A live fuzzy filter is a predicate over rows plus "retain any group with a surviving descendant." On sorted rows that is
one more linear pass producing a `Vec<bool>` mask; ranges are then computed against the compacted index. No change to
`children`.

---

## 6. Multi-valued levels

A track with genres `[Rock, Electronic]` under a genre level appears in two branches. The tree stays well-formed — every
_row_ still has exactly one position. What breaks is the row ↔ track bijection.

Lowering: `LATERAL UNNEST` of the tag before `keyed`.

Three call sites must then change, and only these three:

| Behaviour       | Single-valued    | Multi-valued                    |
| --------------- | ---------------- | ------------------------------- |
| Subtree count   | `range.len()`    | `COUNT(DISTINCT track_id)`      |
| Enqueue subtree | slice of `track` | dedup on `track_id`, order kept |
| Reveal current  | one `NodeRef`    | N matches — highlight all       |

The `multi` flag on `ResolvedLevel` drives all three. The navigator never sees it. Without the flag, a subtree reports
42 tracks and enqueues 37, which is the kind of bug that is reported as "the counter is wrong" and takes a day to trace.

---

## 7. Ragged depth

Optional segments (`[%date% - ]%album%`) let a level vanish for some rows, so a node's range may contain both leaf rows
and subgroups — a directory holding files and subdirectories. Two consequences:

- Paths are variable-length `Vec<Key>`, not fixed-arity tuples.
- A short path must tie-break against a longer one sharing its prefix.

Convention: **groups before loose tracks**, matching filesystem explorers. Sort on `(is_terminal_here, sort, ident)` at
each position, `is_terminal_here` ascending so non-terminal (group) rows sort first.

`depth_of` on `Rows` records the terminal depth per row so the navigator can tell a leaf from a group without
re-evaluating expressions.

---

## 8. Example specs

```rust
// Library browse — every default applies
ViewSpec {
    source: Query,
    filter: None,
    levels: vec![level("albumartist"), level("album")],
    leaf: None,
}

// By decade, then artist, artists ordered by debut
ViewSpec {
    source: Query,
    filter: None,
    levels: vec![
        Level { ident: expr("($left(%date%,3) || '0s')"), ..d() },
        Level { ident: expr("%albumartist%"),
                sort: Some(Agg { func: Min, expr: expr("%date%"), dir: Asc }),
                ..d() },
    ],
    leaf: None,
}

// Playlist grouped by album — hand order preserved by default
ViewSpec {
    source: Playlist(id),
    filter: None,
    levels: vec![level("album")],
    leaf: None,
}

// A–Z index over artists
ViewSpec {
    source: Query,
    filter: None,
    levels: vec![
        Level { ident: expr("$upper($left(%albumartist_sort%,1))"), ..d() },
        level("albumartist"),
        level("album"),
    ],
    leaf: None,
}

// Flat: most played. Zero levels — the degenerate case, same code path.
ViewSpec {
    source: Query,
    filter: Some(pred("play_count > 0")),
    levels: vec![],
    leaf: Some(Row { expr: expr("%play_count%"), dir: Desc, collate: Numeric }),
}
```

Specs are data. Adding a view is a config change, not code.

---

## 9. Open decisions

- **Nested `multi` levels.** Genre under mood fans out multiplicatively. Legal under the invariant, confusing in the UI.
  Consider rejecting in `resolve`.
- **Empty-key policy.** `$if2(%album%,Unknown Album)` at the key function keeps the key total. Decide where the sentinel
  sorts — first, last, or naturally.
- **`Count` as an aggregate sort** with the `multi` flag set: distinct or raw? Should probably follow the count rule in
  §6.
- **Cache granularity.** The `sorted` CTE result per view, invalidated on library mutation, vs. re-querying per
  navigation. Start with re-querying; the sort dominates and it is fast enough until it isn't.
