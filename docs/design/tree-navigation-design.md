# Tree Navigation & Keymaps

Companion to `tree-view-design.md`. That document defines how a query becomes a sorted row set with a range-based
navigator. This one defines how the user moves through it.

---

## 1. Position

Snacks' explorer is a picker first: the primary interaction is typing to filter, and tree navigation is the fallback.
Its bindings are the common denominator across nvim-tree, neo-tree and yazi:

| key          | action                         |
| ------------ | ------------------------------ |
| `<CR>` / `l` | open file, or toggle directory |
| `h`          | close directory                |
| `<BS>`       | up one directory               |
| `.`          | set current directory as cwd   |
| `Z`          | close all directories          |
| `]g` / `[g`  | next / previous git change     |
| `]d` / `[d`  | next / previous diagnostic     |

Fine as a baseline, but it under-uses Vim. A tree view _is_ a fold hierarchy, and Vim already has a complete vocabulary
for that which users have memorised.

### The structural advantage over a file explorer

In a filesystem, "depth 3" means nothing in particular — depth is incidental. In this design levels are **homogeneous**:
under `artist/album/track`, every node at depth 0 is an artist and every node at depth 1 is an album.

So fold _level_ is semantically meaningful, and `zr`/`zm` — which almost no file explorer implements, defensibly —
become first-class operations:

- `zm` → "collapse to artists"
- `zr` → "expand to albums"

Fold level **is** `depth`. This falls directly out of the view spec and is worth designing around rather than treating
as a nicety.

---

## 2. Vim's fold vocabulary

The mapping is total. Nothing needs inventing.

| key                | fold meaning               | tree meaning                    |
| ------------------ | -------------------------- | ------------------------------- |
| `zo` / `zc` / `za` | open / close / toggle      | node under cursor               |
| `zO` / `zC` / `zA` | recursive variants         | node and all descendants        |
| `zR` / `zM`        | open all / close all       | whole tree                      |
| `zr` / `zm`        | fold level ±1              | **expand / collapse one level** |
| `zv`               | view cursor line           | reveal now-playing, minimally   |
| `zj` / `zk`        | next / previous fold start | next / previous group           |
| `[z` / `]z`        | start / end of fold        | first / last row of group       |

`zv` is the second one worth stealing. "Reveal now-playing" should open exactly the levels needed to show it and no more
— not expand everything and scroll.

---

## 3. Proposed keymap

```
Movement
  j / k          down / up one visible row
  { / }          previous / next sibling at current depth
  [[ / ]]        previous / next node at depth 0
  -              go to parent
  gg / G         top / bottom
  zz / zt / zb   scroll cursor row to centre / top / bottom

Expansion
  l / <CR>       open node; at a leaf, play the track
  h              close node; if already closed, go to parent
  L              open recursively         (zO)
  H              close recursively        (zC)
  <Tab>          toggle open              (za)
  zR / zM        open all / close all
  zr / zm        fold level ±1
  z1 .. z5       jump to fold level N
  zv             reveal now-playing

Selection & queue
  <Space>        toggle selection on node or track
  a              append node's subtree to queue
  A              append and start playing
  y              yank paths / ids
  D              remove selection from queue or playlist

View
  <leader>v      switch view spec (artist/album, year/album, …)
  /              filter (fuzzy, incremental)
  P              toggle now-playing panel
```

### Two details that matter more than they look

**`h` is overloaded.** On an open node, close it. On a closed node or a leaf, jump to the parent. nvim-tree
distinguishes these as `close_node` and `parent_node`; merging them makes `hhh` a natural "back out" gesture instead of
a no-op followed by manual navigation.

**`{` / `}` for sibling jumps.** Moving artist-to-artist without scrolling through 200 expanded tracks is the most
frequent navigation you will do, and `j`/`k` cannot express it. Vim's paragraph motions are unused in this context and
the semantics line up.

---

## 4. Expansion state

Replaces the placeholder `expanded: HashSet<Vec<Key>>` in `tree-view-design.md` §5. Three fields give the whole fold
family, including the interaction where `zr` after a manual `zo` behaves sanely.

```rust
pub struct Expansion {
    /// Everything shallower than this is open. Driven by zr/zm/zR/zM/zN.
    level: u16,
    /// Explicit opens at or beyond `level`.
    opened: HashSet<Vec<Key>>,
    /// Explicit closes shallower than `level`.
    closed: HashSet<Vec<Key>>,
}

impl Expansion {
    pub fn is_open(&self, path: &[Key]) -> bool {
        if self.closed.contains(path) { return false; }
        if (path.len() as u16) < self.level { return true; }
        self.opened.contains(path)
    }

    /// za / zo / zc
    pub fn toggle(&mut self, path: &[Key]) {
        if self.is_open(path) { self.close(path) } else { self.open(path) }
    }

    pub fn open(&mut self, path: &[Key]) {
        self.closed.remove(path);
        if (path.len() as u16) >= self.level {
            self.opened.insert(path.to_vec());
        }
    }

    pub fn close(&mut self, path: &[Key]) {
        self.opened.remove(path);
        if (path.len() as u16) < self.level {
            self.closed.insert(path.to_vec());
        }
    }

    /// zr / zm / zR / zM / z1..z5 — level ops discard manual overrides,
    /// exactly as Vim's `foldlevel` does.
    pub fn set_level(&mut self, n: u16) {
        self.level = n;
        self.opened.clear();
        self.closed.clear();
    }

    /// zv — open ancestors of `path` and nothing else.
    pub fn reveal(&mut self, path: &[Key]) {
        for d in 1..path.len() {
            self.open(&path[..d]);
        }
    }
}
```

Keying on `Vec<Key>` (the ident path) rather than `NodeRef` is what makes state survive re-sorts, filter changes and
library rescans — the file-explorer reason, and the same reason `NodeRef` must not be stored anywhere persistent.

### Recursive variants

`zO` / `zC` need the row range, not just the path, so they live on the navigator rather than on `Expansion`:

```rust
pub fn open_recursive(exp: &mut Expansion, rows: &Rows, node: NodeRef);
pub fn close_recursive(exp: &mut Expansion, rows: &Rows, node: NodeRef);
```

Both are a scan over `node.range` collecting distinct ident paths at each depth below `node.depth`. Linear, no recursion
into owned structures.

---

## 5. Filtering

Incremental filter should auto-reveal, matching the picker behaviour the user already expects:

1. Compute the row mask (predicate over rows, plus retain any group with a surviving descendant).
2. Snapshot `Expansion`.
3. While the filter is non-empty, treat every group with a surviving descendant as open — i.e. filter mode overrides
   `is_open`.
4. On clear, restore the snapshot.

Do **not** mutate `Expansion` during filtering. Restoring the user's manual expansion state after they clear a search is
the difference between the view feeling like a tool and feeling like it lost their place.

---

## 6. Where to defer to Vim

When an edge case is unclear — `zr` past max depth, `zc` on a leaf, `za` on a node whose parent is closed, what `zm`
does at level 0 — do what Vim's fold implementation does. The semantics are already specified, already documented, and
already in the user's fingers. Deviating creates a surprise for no gain.

Two worth noting up front, since they are easy to get wrong:

- `zr` beyond the deepest level is a no-op, not an error, and does not clamp `level` to something that then makes `zm`
  behave asymmetrically. Store `level` unclamped and clamp only at render.
- `zc` on a leaf closes the _parent_, then leaves the cursor on the parent. This is what makes repeated `zc` walk up the
  tree, and it is the behaviour that makes `h`-as-parent feel consistent rather than redundant.
