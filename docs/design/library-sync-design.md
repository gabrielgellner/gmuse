# Library Sync: Catalog, Local State, Events

Third document in the set, after `tree-view-design.md` and `tree-navigation-design.md`. Those define how a row set
becomes a navigable tree. This one defines where the rows come from when the library lives in more than one place.

**Governing constraint:** the NAS is the source of truth but is _not_ assumed reachable. A device must be fully
functional — browsing, playback, event recording — having never contacted it. Bundles move by removable media. Airgapped
operation is the design target, not a degraded mode.

---

## 1. Three states, three disciplines

The single most consequential decision here. Keep these separate and each is trivial; merge them and you need CRDTs.

| State                                               | Writer           | Discipline         | Travels?   |
| --------------------------------------------------- | ---------------- | ------------------ | ---------- |
| **Catalog** — tags, duration, MBIDs, audio hash     | one (see §6)     | immutable snapshot | yes, pull  |
| **Local state** — path, present, mtime, verified_at | this device only | mutable, private   | **never**  |
| **Events** — plays, skips, ratings, tag edits       | every device     | append-only log    | yes, union |

Two rules follow, and both are easy to violate by accident:

- **Path lives in local state, never in the catalog.** It feels like a property of the track. It is a property of _a
  device's copy_ of the track. Putting it in the catalog is the mistake that makes bundles non-portable.
- **`play_count` is never a stored number.** It is an aggregate over the event log, recomputed. Incrementing a counter
  across disconnected devices loses writes silently and you will not notice until you have two devices.

---

## 2. Identity

`track_id` must be identical on every device, generated without coordination, and stable under retagging.

- Path-derived: **no** — differs per device.
- File hash: **no** — editing a Vorbis comment changes it.
- **Audio-stream hash** (decoded frames, tag blocks excluded): yes. Stable across retagging, identical everywhere,
  computable offline.
- MusicBrainz recording ID where available, audio hash as the fallback, AcoustID/chromaprint if you later want matching
  across transcodes.

`device_id` is a random UUID generated at first run. No registration, no server handshake — a device that has never seen
the NAS still has a valid identity.

`event_id` is a ULID. Lexicographically sortable, monotonic per device, collision-free without coordination, and carries
its own timestamp.

---

## 3. Bundle format

A bundle is a directory that is simultaneously the sync unit, the backup format, and a working library. Copy it to a
drive, point the player at it, it works — no NAS, no network, no import step.

```
music-bundle/
  BUNDLE.toml            # version, created_at, origin_device, catalog_vintage
  catalog.parquet        # snapshot: track_id, tags, duration, audio_hash
  manifest.parquet       # track_id -> relpath, size, audio_hash
  events/
    <device_id>/
      <ulid>.parquet     # append-only, immutable once written
  audio/
    a3/a3f2....flac      # sharded by first 2 hex of track_id
    b1/b19c....opus
```

**Audio is laid out by `track_id`, not by artist/album path.** Paths are local state and the tree is derived; an
artist/album directory layout would bake a particular view into storage and break the moment a tag is corrected.
Sharding by hash prefix keeps directory sizes sane.

`BUNDLE.toml` records provenance — which device produced it, from what catalog vintage, and which device event ranges it
contains. This is what makes a multi-hop chain debuggable six months later.

**A bundle is always an export, never a live lakehouse.** Whatever the NAS or a satellite uses internally (§9), what
crosses the airgap is plain Parquet plus a TOML file, readable by anything. No catalog database, no path rewriting, no
version coupling between the drive and the machine that reads it.

---

## 4. Merge

The merge is a set union over `event_id`. That is the whole algorithm.

```sql
CREATE VIEW events AS
SELECT * FROM read_parquet('events/**/*.parquet')
QUALIFY row_number() OVER (PARTITION BY event_id) = 1;
```

Properties that fall out for free:

- **Idempotent.** Importing the same bundle twice is a no-op.
- **Commutative and associative.** Merge order is irrelevant, so `A → drive → B → drive → NAS` gives the same result as
  any other route.
- **Partition-tolerant.** No coordination, no locks, no online handshake.
- **No merge code.** DuckDB reading a glob is the implementation.

git-annex is the precedent: its branch is union-merged precisely so that disconnected repositories synced over
sneakernet converge without conflict resolution. Same mechanism, smaller problem.

### Fast-path dedup

Full `event_id` deduplication over years of history gets slow. Because ULIDs are monotonic _per device_, keep a
watermark:

```
high_water[device_id] = max(event_id) already merged
```

Skip any file whose maximum ULID is below the watermark. Keep the `event_id` union as the correctness fallback — the
watermark is only valid when a device's events are merged as contiguous ranges, and a partially-copied bundle breaks
that assumption.

### Compaction

Only the NAS compacts, because only the NAS sees everything. **Compacted output must retain `event_id`**, or a device
that reappears after two years re-introduces duplicates that dedup can no longer catch. Roll into monthly files; do not
aggregate away the ids.

**`event_id` must be a real data column.** Never identify an event by a storage engine's internal row id or lineage
column. Those are meaningful only within one storage lineage and mean nothing in a bundle that has passed through two
other machines. Under DuckLake specifically (§9), compaction expires snapshots and limits what the change feed can
return, so any dedup that leaned on snapshot history rather than on the id in the data would silently start admitting
duplicates.

### Dangling references

An airgapped device on catalog vintage V generates events for `track_id`s that a later catalog may no longer contain.
**Keep them.** Store as orphan events, resolvable if the track returns. Dropping events on an unresolvable foreign key
is data loss caused by a housekeeping rule.

---

## 5. Event schema

One table, one `kind` column, so new event types need no new plumbing.

```
event_id     ULID          -- globally unique, sortable
device_id    UUID
track_id     BLOB          -- may dangle (§4)
ts_wall      TIMESTAMP     -- device wall clock, display only
ts_seq       BIGINT        -- per-device monotonic counter
kind         ENUM          -- played | skipped | rated | tagged | added
payload      STRUCT        -- kind-specific
```

**Never order across devices by `ts_wall`.** A device offline for six months with a drifting RTC will produce timestamps
that interleave wrongly with everything else. `ts_wall` is for display; `ts_seq` gives a total order within a device;
across devices there is no true order and the design must not need one.

This is why `played` events are the right primitive — a set union needs no cross-device ordering at all. `rated` needs
last-write-wins per track, which does. Use a hybrid logical clock if you care; for a personal library, accept that a
rating set on two devices during the same disconnected window resolves arbitrarily.

---

## 6. Catalog writers: the real fork

Two options. Pick deliberately.

**A — Satellites are read-only.** Catalog is a snapshot pulled from the NAS; all tagging happens there. Simple, and
correct if the NAS is where you rip and tag.

**B — Tag edits are events.** `kind = tagged`, payload `(field, value)`, last-write-wins per `(track_id, field)`. The
catalog becomes a _projection_ of the tag-event log rather than a primary artifact. Uniform — one merge mechanism for
everything — and it is what you need the moment the airgapped machine is where the work actually happens.

**Recommendation:** start with A, but reserve `kind = tagged` in the schema now. The upgrade path is then a new
projection query, not a redesign. Choosing A with a schema that cannot express B is the version of this decision that
costs real work later.

---

## 7. Availability is a dimension

```rust
enum Availability { Local, Remote, Absent }
```

Three states, not two. `Local | Remote` can play; only `Local` works offline. The binary version breaks on the first
train journey.

`available` is an ordinary `Expr`, so it composes with everything in `tree-view-design.md`:

- **Prune** — `filter: available != Absent`
- **Browse by it** — `Level { ident: available }`, "what's downloaded"
- **Display** — dim absent rows rather than hiding them, so the catalog stays complete even on a device holding 2% of it

### Sync policy is a view spec

The payoff. "Download my most-played unsynced tracks" is not a feature:

```
filter: available == Absent AND play_count > 3
sort:   play_count DESC
limit:  500
```

git-annex calls these preferred content expressions; it is their best idea. "What is on this device but not the NAS" is
a set difference on `track_id` — a query, not a tree walk.

---

## 8. Sizing

Per track: ~120 bytes of strings, ~40 numeric, 80 for MBIDs as binary UUIDs, 32 for the audio hash. Artist, album and
genre dictionary-encode to nearly nothing.

| Tracks | Catalog (Parquet + zstd) | FLAC audio |
| ------ | ------------------------ | ---------- |
| 100k   | 10–20 MB                 | ~2.6 TB    |
| 1M     | ~150 MB                  | ~26 TB     |

The catalog is roughly 1/170,000 of the audio. **Size is never the constraint** — rsync the whole catalog every time and
skip incremental sync entirely until it demonstrably hurts. It will not.

---

## 9. Storage and transport

Nothing here is a sync protocol.

- **Catalog:** rsync `catalog.parquet` down. Whole file.
- **Events:** rsync `events/<device_id>/` in both directions. Files are immutable once written, so rsync never rewrites
  one.
- **Audio:** rsync the set produced by the sync-policy query.
- **Local state:** never leaves the device.

The minimal version works: DuckDB reads `catalog.parquet` in place, joins against a local `local_state` table, and merge
is the view in §4. Total sync logic written by you is the policy query and an rsync invocation.

### DuckLake variant

DuckLake fits the authoritative side well and the wire not at all. The distinction is worth holding firmly, because the
tempting move — making the NAS's lakehouse _be_ the sync mechanism — works right up until the first disconnected device
and then not at all.

|           | storage                                                     |
| --------- | ----------------------------------------------------------- |
| NAS       | DuckLake — catalog, events, manifest as tables              |
| Bundle    | plain Parquet + `BUNDLE.toml` (§3)                          |
| Satellite | DuckDB file + Parquet; DuckLake locally if it rips and tags |

**What it buys:**

- **Data inlining** is the right answer for event append. A play is a one-row insert, and without inlining that means
  one Parquet file per play — the small files problem in its purest form. DuckLake stores changes below a row threshold
  (default 10, configurable) directly in the metadata database, immediately queryable and transactional, flushing to
  Parquet on a maintenance call. This replaces the roll-files-by-session machinery in §4.
- **Snapshots are `catalog_vintage`.** The downgrade check in §10 becomes a snapshot id comparison. Snapshots are
  database-level, so a change touching catalog and manifest together is one atomic snapshot.
- **Time travel is undo for the tag editor.** Commit a bulk retag from the buffer editor as one transaction — one
  snapshot — and revert to the prior version if the regex was wrong. Better than writing inverse operations for
  partial-application recovery: the storage layer supplies both atomicity and retroactive undo.
- **`table_changes`** gives incremental catalog pull between two snapshots. At 15 MB (§8), don't. Rsync the file.

**What it does not buy:** there is no union merge. DuckLake is MVCC with optimistic concurrency against a single catalog
— one lineage. Two devices each running their own instance produce divergent snapshot histories with no reconciliation
operation, because that is not what the format is for. The convergence property in §4 belongs to the event log design,
not to the storage engine, and it is the only reason disconnected operation works.

**Ingest of a returning bundle** is one transaction:

```sql
INSERT INTO events
SELECT * FROM read_parquet('bundle/events/**/*.parquet') b
WHERE NOT EXISTS (SELECT 1 FROM events e WHERE e.event_id = b.event_id);
```

One snapshot per bundle import, which makes `SELECT * FROM snapshots()` the sync audit log — when each device's bundle
landed, and what it contained. That subsumes most of the provenance chain `BUNDLE.toml` was carrying.

The airgapped machine can run DuckLake symmetrically: local undo for its own tagging, and its export is still plain
Parquet. The lakehouse format never crosses the airgap; only Parquet does.

---

## 10. Failure modes to handle explicitly

- **Tombstones.** Deleting from the catalog needs a tombstone row or the next pull resurrects it. Same for redacted
  events.
- **Verification cost.** "Is this file still present and unmodified" over a NAS is slow. Cheap check on `(size, mtime)`
  at startup; full `audio_hash` only on an explicit `fsck`. `manifest.parquet` makes fsck work with no NAS present.
- **Partially copied bundle.** Interrupted drive copy yields a truncated event file. Write `events/*.parquet` to a temp
  name and rename; treat a bundle without a readable `BUNDLE.toml` as unusable rather than partially importing.
- **New audio from a satellite.** An airgapped machine that rips CDs is a _source_. `kind = added` plus audio files it
  holds and the NAS does not. Push, don't just pull.
- **Two devices, same drive, different vintages.** Merging a newer bundle's catalog over an older one is fine (catalog
  is a snapshot); merging an _older_ catalog over a newer one silently reverts tags. Compare `catalog_vintage` in
  `BUNDLE.toml` and refuse the downgrade.

---

## 11. What to build first

1. `track_id` = audio-stream hash. Everything depends on it and changing it later invalidates every event ever recorded.
2. Event log as append-only Parquet, with `event_id`, `kind`, and `ts_seq` in the schema from day one even if only
   `played` is emitted.
3. `local_state` as a separate table, joined — never a column on the catalog.
4. `Availability` as a three-state enum in the view expression language.

Bundles, watermarks, compaction, and the sync policy UI can all come later without redesign. The four above cannot.

Note that none of the four mentions a storage engine. That is deliberate: the choice in §9 is reversible, and the four
decisions above are not.
