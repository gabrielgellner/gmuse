# The library API

**Status: proposal.** Nothing here is built. `layers.md` says which systems exist and what may talk to what; this says
what they actually say to each other.

---

## 1. The premise, stated once

The library is a **library-science object describing a collection, past and future**. Not a view of a disk, not an index
of files, not what the tags happen to say this morning.

A recording is in it because you decided something about it. Whether you own a copy, ever owned one, or intend to is a
separate fact, and all three are ordinary rows. That is why an unbound recording is never swept: "nothing holds this"
and "I want this" are the same shape, and always were.

Files serialize the library. They do not constitute it.

---

## 2. Nothing is implicit

The one rule the rest follows from.

| what happened                    | what gmuse does          | what it does **not** do         |
| -------------------------------- | ------------------------ | ------------------------------- |
| a file appears in a watched root | notices it is not known  | catalogue it                    |
| a file's bytes change            | notices the audio is new | re-bind it to its old recording |
| a file's **tags** change         | **nothing**              | re-read them, ever              |
| a file moves                     | notices the old is gone  | pair departure with arrival     |
| a file is deleted                | drops that holding       | drop the recording              |

Each of the "does not" columns is an inference, and every one of them was wrong at least once on 2026-09-01. An
inference can be wrong silently; a declaration cannot.

So the operations that change what the library _is_ are named and invoked:

```
gmuse library import <paths…>        this audio is new, catalogue it
gmuse library relink <path> <id>     this file is that recording
gmuse library forget <id>            I no longer have this, keep the entry
gmuse library apply <file.jsonl>     assert these facts
gmuse library query <spec>           ask
```

---

## 3. What a scan is now

Not a rebuild. A **reconciliation of holdings**, which is a much smaller question:

```
for each file under a root:
    unchanged path, mtime and size?   →  nothing to do
    audio already known?              →  it is a holding of that encoding
    audio not known?                  →  report it; do not catalogue it
```

**No tags are read.** Not for a known file, not for an unknown one — the unknown one is _reported_, and reading its tags
is what `import` does when you ask for it.

Two consequences worth naming, because they are the point:

**An external retag is invisible**, which is what "tags are serialization" has to mean if it means anything. A tool that
rewrites `TCON` across the library changes nothing about the library, and should not.

**A rescan costs a `stat` per file.** Local metadata — mtime, size, existence — is a device fact and always fair game;
it is what tells us a hash is unnecessary. Opening 6500 files to ask what they claim about themselves is the thing being
retired.

---

## 4. Query: one language

Every question is a [`ViewSpec`](tree-view-design.md) — a filter, a grouping path, an ordering. There is not a query API
and also a browse API; the browser _is_ the query surface, and a tool asking "which albums have no genre?" is asking the
same kind of question as a person pressing `2`.

```
gmuse library query 'filter: confidence == Provisional; levels: [artist]; sort: -recordings'
gmuse library query 'filter: agg_max(cutoff_khz) < 17' --format jsonl
```

Output is rows. Human-readable by default because a terminal is where this gets read; `--format jsonl` when something is
going to parse it.

---

## 5. Apply: a file of facts

Not a call per fact. A tool proposes in bulk, a person reviews, gmuse applies:

```jsonl
{"fact":"genre_assigned","path":"…/03 Callisto.mp3","genre":"Techno","basis":"discogs","confidence":"matched"}
{"fact":"measured","path":"…/03 Callisto.mp3","cutoff_hz":19800,"analyser":1}
{"fact":"credited","path":"…/03 Callisto.mp3","artist":"Marc Romboy","role":"remixer","position":1}
```

**JSONL because facts are heterogeneous.** A TSV would need a column union across every fact type, and this project has
already been bitten by a TSV whose quoting fought its own content. One object per line keeps it greppable and `jq`-able,
which is what the TSVs were chosen for in the first place.

Three properties this shape has and a per-fact API would not:

- **Bulk.** Nine hundred genre assignments in one process.
- **Offline.** No player need be running. A socket cannot offer that, which is why `--remote` is not this.
- **Reviewable.** The file exists before it is applied, so `genre-uncertain.tsv` stops being a thing thrown away at the
  end of a pass and becomes the artefact that _is_ the pass.

### Tools speak in paths

`Id<Recording>` derives from a MusicBrainz id or from `audio_id`, and `audio_id` is XXH3 over Symphonia's packet
boundaries — not reproducible in Python and not worth trying. So a proposal names a **path**, and gmuse resolves it. The
same shape `migrate::to_records` already has.

A path that resolves to nothing is reported, never guessed at.

---

## 6. The UI edits the library, not the files

**There is no tag editor.** This closes the question `music-player-tui.md` §1 deferred to #41.

What the UI edits is **library metadata**: a credit, a genre, a rating, a pin, a merge. Those are facts about the
collection and they live in the library whether or not any file exists to carry them — which is precisely why a
recording you do not own can still be credited, rated and filed.

Writing tags is a **separate flow, in the other direction**:

```
library  ──serialize──▶  tags on files
```

Invoked deliberately, over a selection, when you want other programs to see what you know. It is an **export**, and it
has the properties an export has: it can be redone, it can be skipped, and the library does not depend on it having
happened.

That is the whole difference from where this started. "Edit the tag and let the scan pick it up" makes the file the
authority and the library a cache of it. "Edit the library and serialize when you want to" makes the library the thing
and the file a copy — and only one of those survives a second machine.

---

## 7. Import is strict, and the tools are what make a file ready

**A file that cannot be identified is not imported.** It is an error, and the import fails rather than cataloguing
something the library cannot describe.

That is not the library being difficult. Getting an input compliant is the _tool's_ job, and the alternative — a library
that accepts anything and marks it provisional — is a library that fills with entries nobody will ever fix, because
nothing ever forced the question.

So the shape is a **pipeline of passes, ending in one import**:

```
bought download
  → normalise the tag formats           tags.py
  → resolve against MusicBrainz         a lookup pass
  → fingerprint what that missed        AcoustID (#57)
  → fill the gaps by hand or by model   whatever the case needs
  → gmuse library import <dir>          strict, and the last step
```

A Bandcamp purchase is the ordinary case: the core tags are there, and a MusicBrainz or Discogs pass and possibly a
fingerprint are what turn "some files with plausible tags" into "something the library can hold". Each of those is a
small tool, and they are expected to multiply — which is fine, because they are all outside the library and none of them
can corrupt it.

**What the minimum actually is has to be learned.** The honest starting point is what the ladder can already answer
with: a recording identity from a MusicBrainz id or a fingerprint, a title, and an artist. Whether an album and a track
number join that list is a question the first few real imports will settle better than this paragraph can.

### Where a proposal file lives

**Wherever you say.** It is an argument, defaulting to the working directory, and there is no magic location — because
it is a work product rather than application state, in the same family as a `terraform plan -out` file or the output of
`git format-patch`.

Three reasons not to hide it:

- `$XDG_STATE_HOME` is, in this project's own words, "exactly what a backup tool skips" — which is why history was moved
  out of it. A proposal somebody has hand-reviewed is work, not cache.
- the library's `data_dir` syncs between machines, and a half-reviewed proposal should not.
- a tool that writes where it is told is scriptable; one that writes to a fixed directory is not.

`out/` in the repository stops being the default. That is the repo, not a workspace.

---

## 8. Open

- **`forget` and `relink`** — whether they are one operation or two, and what each does to holdings, history and the
  encoding that replaced the old one. Wants concrete cases before it wants a design. #88.

---

## 9. Related

`layers.md` — who may talk to whom · `domain-model.md` — what the facts are · `library-sync-design.md` — how the log
merges · `tree-view-design.md` — the query language · #62 (mutation API) · #86 (delete the store) · #87 (a bad copy is a
row)
