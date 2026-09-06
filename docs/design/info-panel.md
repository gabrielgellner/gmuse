# The info panel: a focus and its links

**Status: proposal.** Nothing here is built. It is the design for a surface that answers _what does the library actually
say about this, and who said it_ — the milestone "One thing in focus".

`query-language.md` §4 already claims that membership is caused by named facts, "so the translation is inspectable
rather than magic". Nothing in the player inspects it. `Provenance` has carried the note _"the review queue has no UI"_
since it was written, `Catalogue::conflicts` and `Catalogue::merges` are folded and unread, and #96's whole point — that
a claim carries a basis and a confidence — is invisible from the screen. This is the surface where those get cashed.

---

## 1. The frame: it is not a graph view

The model is a graph. Twenty-one entity kinds, a spine of containment, assertions hanging off subjects, and edges
between concepts. The obvious move is to draw it, and the obvious move is wrong.

**Node-link diagrams stop working at about a dozen nodes**, and a terminal makes it worse: no zoom, no pan, character
cells. Every general-purpose RDF browser that tried this — LodLive, the graph tabs of a dozen triple stores — is
remembered as a demo rather than a tool.

What _does_ work, and what independent lineages converged on:

| tool                        | shape                                                                                    |
| --------------------------- | ---------------------------------------------------------------------------------------- |
| Smalltalk / Pharo inspector | one object, its fields, click a field to inspect it, a breadcrumb of where you came from |
| MusicBrainz entity pages    | one entity, its own data, then relationships grouped by type, each a link                |
| `k9s`                       | one resource, describe it, drill into owners and children, `esc` pops                    |
| `tig`, lazygit              | a stack of "what am I looking at"; `enter` pushes, `q` pops                              |
| vim's tag stack             | jump to a definition, `^o` comes back                                                    |

The shape is the same every time: **one thing in focus, its own attributes, its links grouped by relation, and a stack
of where you have been.** The graph is _traversed_, not depicted.

That is also already this project's idiom. `go parent`, the fold motions, `[[`/`]]`, and the `:` line are all "navigate
a structure by moving through it". A node-link diagram would be the one screen in gmuse that is not vim.

### What this buys, and what it gives up

It gives up seeing shape at a glance — you cannot look at this panel and see that an artist has forty recordings across
six release groups without walking it. That is a real loss and it is the right trade: the questions people actually ask
of a library are local ones — _why is this filed here, where did this genre come from, which copy is playing, what else
is on this release_ — and every one of them is answered from a focus and one hop.

---

## 2. What makes _this_ graph unusual: three kinds of edge

A generic browser would list every link the same way. This model has three kinds, and they want different treatment.

### Structural — the spine

Containment, mostly 1:N, and mostly a path:

```
Work ─performs─ Recording ─slot─ Track ─on─ Medium ─of─ Release ─in─ ReleaseGroup
                    │
                    └─ Encoding ─held on─ Device
```

These are navigational. They have no basis and no confidence — a track is on a medium, and that is a fact about a
pressing rather than a claim somebody made. They render as **links**: a label, a name, and an arrow.

### Descriptive — the claims

Concepts per facet, credits, names, aliases. These are **not values**. The catalogue keys them by
`(subject, concept, source)` and `(subject, artist, role, source)` precisely so that two sources asserting the same
thing stay two rows, and #96 added a retraction that shadows rather than deletes.

So the panel must not print `genre: Techno, Minimal`. It must print who said what:

```
genre    Techno     tag · confirmed
         Techno     discogs · matched
         Minimal    discogs · matched
       ⊘ Ambient    ruled out · manual
```

**This is the whole differentiator.** Every other player shows the value. This is the one that can show why the value is
what it is, because the model refused to collapse it in the first place. `library-is-the-truth` says tags are a
serialization and never authoritative; this is where you can see the difference.

### Historical — the facts

The log itself: stamped, per device, append-only. "When did this become true, and on which machine." A claim's basis
says _who_; the fact says _when_ and _where_, and a retraction says somebody changed their mind.

The log is deliberately greppable on disk, and this does not replace that. What it adds is the same records **already
filtered to one subject**, which grep can only do if you know the id.

---

## 3. The shape of the thing

### `Focus` — one addressable thing

The panel is always focused on exactly one entity. `Focus` is the enum of what that can be, and it is what the
navigation stack holds and what `:info` parses.

Not all twenty-one kinds at once. The ones reachable from a row, first:

```
Recording · Encoding · ReleaseGroup · Release · Artist · Concept · Collection · Work
```

`Facet`, `Hierarchy`, `Role`, `Label`, `Device`, `View` follow when something links to them.

**`Holding` was on this list and is not a focus** (#124). A holding is `(device, encoding) -> locator` and no id is ever
minted for one, so there is nothing to address it by — and what a holding page would show, where a copy is and on which
machine, is what the _encoding_ page shows a row of. Eight kinds, and every one has an id of its own in the fold.

**Named `Focus` rather than `Subject`**, which `model::proposal` already uses for what a proposal is about, and rather
than `Described`, which `model::facet` uses for the two things a concept can be asserted of. Three near-synonyms in one
codebase is how a reader stops trusting any of them.

### `About` — what the library says

One seam question, `Listing::about(focus) -> About`, returning sections:

```rust
struct About {
    kind: &'static str,        // "recording"
    title: String,             // "Callisto"
    subtitle: Option<String>,  // "Stephan Bodzin & Marc Romboy"
    sections: Vec<Section>,
}

struct Section { title: &'static str, rows: Vec<Row> }

enum Row {
    /// A structural edge. Enter follows it.
    Link { label: String, name: String, to: Focus },
    /// A claim, and who made it. `said` is `None` when the assertion has been
    /// swept and only the retraction is left — see below.
    Claim { label: String, value: String, said: Option<(Source, Confidence)>,
            retracted: Option<Source>, to: Option<Focus> },
    /// Something the fold simply is. No asserter.
    Fact { label: String, value: String },
    /// Too many to list: how many, and the query that finds them.
    Many { label: String, count: usize, query: String },
}
```

`Many` is a fourth row and **not a fourth edge kind**: it is a fan-out summarised. An artist with four hundred
recordings cannot list them, so it says how many and hands over the query — which makes the panel a way _into_ the query
language rather than a competitor to it.

**A retracted claim has no basis behind it, and the type says so** (#125). `Catalogue::retract` sweeps every asserter of
a pair out of `assertions`, so a concept somebody ruled out leaves a retraction and no assertion. Printing `manual` as
the asserter would be a lie and dropping the row would lose that anybody decided; `said: None` is the third answer. The
assertion that was shadowed is still in the log, and §5's facts section is where it shows — which is a clean division:
the claims section is the state, the facts section is the history.

**The three row kinds are the three edge kinds**, which is the point: the distinction is in the type rather than in how
the drawing code happens to format things.

### The opening focus is the recording

A row under the cursor is a `CopyId`, which is an `Id<Encoding>` — the bytes. But almost everything worth reading is
about the _recording_: the title, the credits, the genres, the plays, the rating. Opening on the encoding would put the
useful screen one hop away, every time.

So `<key>` over a row opens on its recording, with the encoding and the file listed under **copies** and one keystroke
away. The panel is honest about the distinction rather than hiding it — the section is called copies because that is
what they are.

### Navigation is a stack

- `enter` follows the link under the cursor — pushes.
- `^o` goes back — pops. (`^i` forward, as in vim, if it earns itself.)
- `escape` closes the panel and returns to where you were.
- A breadcrumb along the bottom shows the path: `recording ▸ release group ▸ artist ▸ (here)`.

`^o`/`^i` rather than `h`/`l` because this is a jump list, not a tree, and vim already spells that.

### It is a view

`View::Info`, beside `Guide`. It needs the room and it needs to hold a stack, and the Guide is the precedent for a
content view with its own navigation. Anyone who wants the list beside it has `^w v` already.

---

## 4. What it is not

- **Not an editor.** #96 decided how a view is edited and by what authority; nothing here writes. A later issue may put
  `:add-view`'s affordance on a claim, and it will go through the same authorship rules or not at all.
- **Not a tag viewer.** It shows what the _library_ says. What the file's tags say is a different question with a
  different answer, and conflating them is the mistake `library-is-the-truth` is about. If the tags become worth
  showing, they are their own section, labelled as the serialization they are.
- **Not a graph explorer.** No node-link drawing, no "show me everything within two hops". §1.
- **Not the review queue.** `Provenance`, `Catalogue::merges` and `Catalogue::conflicts` become _reachable_ through this
  — a merged recording can say what it was merged from — but a queue is a list of work, which is a different surface
  with a different shape. It gets its own issue after this milestone.

---

## 5. The milestone

Linear; each one is usable before the next exists.

1. **`Focus`** — the addressable thing, its kinds, and how one is written and parsed.
2. **`About`** — the seam question, and the three row kinds. Where the model work is.
3. **The panel** — `View::Info`, drawing an `About`, opening on the row under the cursor. Usable here.
4. **Following a link** — the stack, `^o`, and the breadcrumb. A browser here.
5. **The facts behind a claim** — the log section, stamped and per device, retractions included.
6. **`:info <thing>`** — reaching a focus by name rather than by cursor.

---

## 6. Open questions

- **Which key opens it.** vim's `K` is "look up the thing under the cursor", which is exactly this, and it is unbound.
  `<leader>i` is more discoverable and less surprising. Decide when the panel exists and can be lived with.
- **How much of a long list to show.** An artist with four hundred recordings cannot list them; a count and a link into
  a view (`artist="…"`) is probably right, which makes the panel a way _into_ the query language rather than a
  competitor to it.
- **Whether a claim's row should be foldable** to hide agreeing sources. Three rows saying `Techno` is honest and noisy.
  Collapsing by default with a count — `Techno (3 sources)` — is quieter and one keystroke from the truth.
