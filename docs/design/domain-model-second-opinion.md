# Domain model design notes — music player library core

**Status:** supplementary design input, not a mandate. These ideas came out of a design discussion and are offered to
inform and strengthen the existing design. Where they conflict with decisions already made in this repo, treat the
conflict as a prompt to weigh rationales, not as an override. Each section states _why_ the choice is made so you can
extend the reasoning to cases not covered here, and revisit it if the premise stops holding.

**Context that shapes everything below:** the library is lossy files of varying vintage and quality accumulated over
many years (no lossless archive); it lives on a NAS plus multiple local devices with disconnected/airgapped sync; DJ
features (BPM/key analysis, cue points) are in scope; and the owner wants the core model to behave like a statistical
data cube — a population of observations sliced by many dimensions. The owner works in official-statistics data
infrastructure (SDMX), so that vocabulary is used deliberately and can be used back.

---

## 1. The shape: a MusicBrainz/FRBR-style entity graph

Model the catalog as the following entities. This is essentially the MusicBrainz schema, which is the most battle-tested
music data model in existence, and it aligns with the FRBR (WEMI) conceptual model from library science: Work →
Expression → Manifestation → Item.

| Entity         | FRBR level     | Meaning                                                        |
| -------------- | -------------- | -------------------------------------------------------------- |
| `Work`         | Work           | The composition, independent of any recording                  |
| `Recording`    | Expression     | One specific recorded take of a work                           |
| `ReleaseGroup` | (groups below) | "The album" as people think of it                              |
| `Release`      | Manifestation  | A specific issue: 2012 vinyl vs 2014 digital                   |
| `Medium`       | —              | Disc 2, side B; sits between Release and Track                 |
| `Track`        | —              | A Recording's placement on a Medium (position, title override) |
| `File`         | Item           | Actual bytes on one device                                     |

**Why the Work/Recording/Release levels are separate:** flat tag formats conflate them, and that conflation is where
music libraries rot. The same recording appears on an album, a compilation, and a reissue (one Recording, many
Tracks/Releases). The same work exists as original and cover (one Work, many Recordings). Collapsing any pair makes some
real query unanswerable later. Keep all levels even if the UI mostly shows ReleaseGroup — the model should be richer
than the view.

**Why ReleaseGroup exists at all:** users think in albums, but "the album" issued five times is five Releases.
ReleaseGroup gives the user-facing notion a stable identity without pretending the issues are one thing.

Additional structural entities:

- **`Artist` + `ArtistCredit`.** Never store artist as a string on anything. An `ArtistCredit` is an ordered list of
  `CreditPart`s: (artist_ref, name_as_credited, join_phrase). This represents "Blawan & Pariah credited as Karenn", "X
  feat. Y", and aliases without string-mangling, and it is a parse-don't-validate poster child: parse the credit once at
  ingest into structure; never regex a display string downstream. Recordings and ReleaseGroups both carry an
  ArtistCredit ref (they can differ — a track on a various-artists compilation).
- **`TypedEdge (subject_id, predicate, object_id)`** for everything relationship-shaped: remix-of, samples, member-of,
  genre/scene tags. One edge table beats a column per relationship because the set of interesting predicates grows
  forever and edges are queryable as a graph (recursive CTEs).
- **`RecordingAnalysis`** (BPM, key, beatgrid, loudness) and **`CuePoint`**, both keyed on Recording — see §5 for why
  Recording and not File.

## 2. Recording vs File: the load-bearing split

A `Recording` is the stable, device-independent identity of the audio content. A `File` is one embodiment of it on one
device: path, codec, bitrate, encoder, quality verdict, byte size, device_id. One Recording : many Files.

**Why:** three independent forces demand it.

1. **Multi-device.** The same recording exists on the NAS and two laptops. If identity lives at the file level,
   everything is triple-counted.
2. **Re-encoding.** In a mixed-quality library, files get replaced with better encodes. If ratings/history/cues hang off
   files, replacing an encode destroys them. Hanging them off Recordings makes file replacement a non-event.
3. **Quality is a File fact, not a Recording fact.** Bitrate, codec, spectral quality verdicts (transcode detection)
   describe a particular encode. Keep them there so "show me my worst encodes of my most-played recordings" is a join,
   not a data-cleaning project.

Chromaprint/AcoustID fingerprints are the join key from File to Recording when tags are absent or lying; store the
fingerprint on File.

## 3. Catalog entities are authority records (reference metadata)

Treat `Artist`, `Work`, `ReleaseGroup`, `Release` as separately maintained reference artifacts — in SDMX terms, codelist
entries; in library-science terms, authority records. Everything else refers to them by stable ID.

Design rules that follow, each with its reason:

- **IDs are meaningless surrogates; labels are mutable.** All human-readable content on an authority record is editable.
  An artist renames → edit one row → fifteen years of history reinterprets instantly. This is the entire payoff over
  denormalization, so: **the fact/event tables must never cache a name "for convenience."** Any cached label is a bug
  generator.
- **Adopt MusicBrainz IDs (MBIDs) as canonical where they resolve.** MBIDs are a shared global authority file. Using
  them makes the private catalog interoperate with Picard-tagged files (TXXX frames in ID3, `MUSICBRAINZ_*` Vorbis
  comments) and external data for free. Local surrogate IDs still exist for entities MusicBrainz doesn't know; store the
  MBID as an attribute when known.
- **Merge is a first-class operation.** Two Artist rows will turn out to be the same person. Follow MusicBrainz's
  mechanism: the losing ID becomes a permanent redirect/alias of the winner. Old references keep resolving; nothing in
  the event log is rewritten. An `id_redirect(old_id, new_id)` table checked at resolution time is sufficient.
- **The catalog is an exportable artifact.** Because it's small and self-contained, it can be exported (JSON/Parquet),
  kept in git, diffed, and snapshotted before bulk retag sessions. A pinned catalog snapshot + the immutable event log =
  reproducible cube builds.

## 4. Listen is microdata: an append-only event log, never counters

`Listen(ts, recording_id, file_id?, device_id, ms_played, context)` — one row per play event, immutable, append-only.
Play counts, last-played, skip rates are **derived at query time (or in materialized views), never stored as mutable
properties.**

**Why events, not counters:**

- **Retroactive measures.** Any measure invented later — skip rate, listens-in-last-90-days, exponentially-decayed
  playcount — is computable over history already collected. A counter can only answer the questions it was designed for
  on the day it was added.
- **Sync becomes trivial.** An append-only event set with device-scoped IDs is a grow-only set: merging two devices'
  histories is set union, with no conflict resolution at all. Merging _counters_ requires PN-counter CRDT semantics and
  loses last-played entirely. This single choice removes most of the airgapped-sync problem.
- **Time stays available as a dimension.** Listens-per-month-per-artist is the same log with `date_trunc` in the GROUP
  BY. A counter has already thrown the time axis away.

Record `file_id` on the Listen where known: it pins which encode (and via its provenance, which Release) a play came
from, which resolves the "same recording on four releases" ambiguity with data instead of policy. Fall back to a stated
default policy (e.g. earliest Release of the group) when absent — decide once at the model level, not per query.

## 5. The cube: Recording is the observation unit

The primary analytical view is **collection-grained, not listen-grained**: one row per Recording, dimensions resolved
through the entity graph, listen-derived measures attached by aggregation.

**Why the population is the collection:** a listen-grained cube has no rows for never-played items, so "least-played
artists", "albums untouched this year", and "what fraction of X have I actually heard" are unanswerable — absence isn't
an observation. Collection-grained, every Recording is a row and unplayed is a measure equal to zero. Concretely, the
projection is a `LEFT JOIN` from recordings to listens; the LEFT is the whole argument:

```sql
CREATE VIEW collection AS
SELECT
    r.recording_id,
    ac.display_string            AS artist,        -- dimension via graph
    rg.title                     AS album,
    rg.first_release_year,
    ra.bpm, ra.key,
    f.codec, f.quality_verdict,                    -- via canonical file
    count(l.ts)                  AS listens,       -- derived measures
    coalesce(sum(l.ms_played),0) AS ms_played,
    max(l.ts)                    AS last_played
FROM recording r
LEFT JOIN listen l USING (recording_id)
JOIN ... /* graph joins */
GROUP BY ALL;
```

**Why Recording and not File as the unit:** files double-count across devices and die on re-encode (§2). File-grained
questions ("GB per codec per artist") are legitimate but are a _different, secondary_ cube over Files.

Dimension structure, stated SDMX-style so it can be extended systematically — a dimension is a _path through the graph_,
so adding one later requires no fact-table migration:

| Role      | Component                       | Resolved via                                        |
| --------- | ------------------------------- | --------------------------------------------------- |
| Measure   | listens, ms_played, last_played | aggregation over `Listen`                           |
| Dimension | artist                          | Recording → ArtistCredit → CreditPart → Artist      |
| Dimension | album / label / year / format   | Recording → Track → Medium → Release → ReleaseGroup |
| Dimension | BPM band, musical key           | Recording → RecordingAnalysis                       |
| Dimension | codec, bitrate tier, quality    | canonical File per Recording                        |
| Dimension | genre / scene / remix lineage   | closure over TypedEdge                              |
| Dimension | time (when wanted)              | `Listen.ts`, pulled into GROUP BY                   |

Coded dimensions (key, codec, context, quality verdict) are closed enums — Rust `enum`s parsed at ingest, so cube
queries are total functions over validated data.

**Beware many-valued dimension paths:** a Recording with three credited artists contributes to three artist marginals;
that's correct, but marginals then don't sum to the grand total. Document this per dimension rather than "fixing" it by
picking one artist.

## 6. Ingest is reconciliation, not truth

File tags are claims, not facts; in an old mixed library they will disagree with each other and with reality. The ingest
boundary therefore parses tag soup into a **resolution attempt against the catalog**:

1. MBID present in tags → resolve directly (redirects applied).
2. Else acoustic fingerprint match.
3. Else fuzzy name/title match above a confidence threshold.
4. Else create a **provisional** catalog entity, flagged for later review.

Past this boundary, everything is a typed reference; nothing downstream ever touches a raw tag string. This is
parse-don't-validate applied at the system boundary: illegal states (a Listen against a nonexistent artist, an unparsed
credit string) are unrepresentable after ingest. Provisional entities keep ingest total — bad tags degrade to "needs
review", never to rejection or to silently invented facts.

## 7. Sync boundary follows the mutability boundary

- **`Listen` events:** immutable, device-scoped IDs → merge is set union. No conflicts possible.
- **`File` rows:** per-device facts; each device is authoritative for its own.
- **Catalog + user metadata (ratings, edits, cue points):** the only genuinely mutable state. It is small (thousands of
  rows), so per-field last-writer-wins with timestamps is sufficient at personal scale; don't build a general CRDT layer
  for it.

The design intent: confine the hard sync problem to the smallest possible surface. That is achieved by the event-log
choice (§4), not by sync-layer cleverness.

## 8. DJ analysis and cue points

BPM, key, beatgrid, loudness are content facts → `RecordingAnalysis`, keyed on Recording. Cue points are conceptually
Recording-level too, **but absolute sample positions differ between encodes of the same audio**: MP3 encoder
delay/padding shifts sample offsets, so a cue set against one file lands early on another. Store cues on Recording
against a canonical timeline, store each File's encoder delay (LAME header gapless info provides it), and correct at
load time. Do not "solve" this by making cues File-level — that reintroduces the §2 failure where replacing an encode
destroys user work.

## 9. Storage: typed relational core, projections outward

- **Operational store: SQLite.** The player's workload is point lookups and small transactional updates (OLTP-shaped).
  Model entities as Rust types with newtype IDs; edge tables for the graph; recursive CTEs cover the graph queries a
  player actually runs.
- **Analytical layer: DuckDB over it** — attach the SQLite file directly, or periodically export the joined-flat
  collection/listen views to Parquet. The flat export is the "data message"; the normalized graph is the "structure" —
  the same structure/data separation as SDMX-CSV against a DSD.
- **RDF/ontologies (Music Ontology etc.): projection only, if ever.** The ontological clarity has been mined for the
  model shape; running the player on a triple store buys daily friction for no operational benefit. An MO-vocabulary
  export from the relational core is cheap to add later if wanted.

## 10. Anti-decisions (deliberate, with reasons)

- **No mutable play-count columns** — destroys retroactivity and sync triviality (§4).
- **No artist/album name strings on facts or files' library rows** — breaks the one-edit-reinterprets-everything
  property (§3).
- **No File-level identity for history, ratings, or cues** — dies on re-encode and double-counts across devices (§2,
  §8).
- **No triple store as the operational core** — wrong workload; keep RDF as an export (§9).
- **No genre column** — genres are TypedEdges to authority-controlled tag entities; a column freezes a taxonomy that in
  this domain never stops moving (§1).
- **No rejection-based ingest** — a strict validator that refuses bad tags makes a decades-old library unimportable;
  provisional entities keep ingest total (§6).
- **No parent-side collection fields** (`ReleaseGroup.releases: Vec<Release>` and the like) — associations are stored on
  the child; see §11.
- **No meaning carried by container order** — if order matters it is an explicit field; see §12.

## 11. Association representation: foreign keys live on the child

**Rule:** every to-one association is a scalar FK on the child (`Release.release_group`, `Track.medium`,
`File.recording`). No entity ever stores a collection of its children. "The releases of a group" is a query (or a
derived, locally-rebuilt index), never a stored field.

This is _not_ a claim about the semantics of the relation. Release-in-group is genuine exclusive, mandatory containment
— and the child-side FK encodes those semantics _more_ strongly than a `Vec` would: a single scalar FK makes
multi-parenting unrepresentable, `NOT NULL` makes membership mandatory, and `ON DELETE RESTRICT`/`CASCADE` gives
whole-part lifecycle. A parent-side `Vec` happily represents the illegal states (a release in two groups' vectors, or in
none). Containment is asserted by constraints, not by which struct holds the pointer. The API may still _present_
containment (`catalog.releases_of(rg)`) over a derived index.

**Why this matters for sync — the load-bearing rationale:** the unit of conflict in the sync model is the record. A
parent-side collection packs N independent membership facts into one write-unit, so two devices adding _different_
releases to the same group both mutate the parent record and collide; per-record LWW then resolves destructively (one
addition silently lost). Fixing that properly means per-collection set-CRDT machinery (tombstones, add/remove-wins
policy), and list CRDTs if order mattered — the hard end of the literature. Child-side, the same two operations are
inserts of distinct rows: the parent stays byte-identical, and merge is row union keyed by ID, with no policy consulted.
Membership expressed as _row existence_ merges by union for free — the same property the Listen log exploits.

Corollaries:

- **Reassignment is one field write.** Moving a release between groups is `release.group = RG2` — a single LWW register,
  which converges to some one consistent answer under concurrent edits. Parent-side it is a paired remove+add across two
  records, which can interleave into "in both groups" or "in neither."
- **Every field is a replicated data type once devices sync; representation chooses which one.** Scalar fields are LWW
  registers (the easy case). This rule builds the whole catalog out of registers and row-existence, i.e. exclusively out
  of the easy cases.
- Secondary, genuinely many-to-many groupings (box sets, series, trilogies) go through `TypedEdge`, not through a second
  parent pointer.

## 12. Order is domain data or it doesn't exist

If an ordering is meaningful, it is an explicit field on the row (`Track.position`, `CreditPart.position`), never
encoded in a container's sequence. If no such field is justified, the collection has set semantics and callers sort at
query time (releases within a group: order by date, by preference policy — all derived). This keeps ordered associations
syncable as scalar fields (§11) and applies parse-don't-validate to ordering: order you can see is order you can
validate; order smuggled in through structure is neither.

## 13. Playlists: the one genuinely mutable ordered list

Playlists are user-editable, ordered, and edited on whichever device is at hand — the worst-case sync object, which is
why they are their own entity and must not overload any catalog structure. Pattern, applying §11 + §12:

```
Playlist      { playlist_id, name, ... }
PlaylistEntry { entry_id, playlist, recording, ord, deleted }
```

- Membership = row existence → concurrent adds from two devices union cleanly.
- `ord` is a fractional/lexicographic sort key: insert-between generates a key between the neighbours' keys, so no other
  row is touched and concurrent inserts land in a deterministic total order after merge.
- Removal is a `deleted` flag (an LWW register), not row deletion, so remove-vs-edit races resolve predictably.

This stays out of list-CRDT territory entirely while giving users what they observe as a synced ordered list. Periodic
key compaction (rewriting `ord` values when keys grow long) is safe as a local operation followed by normal field-level
sync.

## 14. Open questions worth taking a position on

- Canonical-file selection policy per Recording (highest quality verdict? device preference order?) — needed for the
  file-attribute dimensions in §5.
- Confidence thresholds and UX for provisional-entity review (§6).
- Whether ReleaseGroup-level ArtistCredit differences from Recording-level credits need surfacing in the UI or only in
  the model.
- Codelist for musical key: standard notation vs Camelot, or both with a mapping table (DJ features suggest Camelot as
  the display form).
- Materialization strategy: live view vs refreshed materialized collection table; measure staleness tolerance before
  optimizing.
- Playlist `ord` key scheme (decimal fractions vs lexicographic strings à la fractional indexing) and when/whether
  compaction runs (§13).
