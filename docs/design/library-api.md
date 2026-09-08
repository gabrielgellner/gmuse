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

### An archivist, not a completionist — #162

**A reference to something the library does not hold is a fact about the world, not a gap to fill.** A remix was made
out of an original; that is true whether or not the original is ever acquired, and recording it is the archival act.
Treating it as a work list would turn every relationship into a shopping list and make the library's own completeness a
thing it nags about.

The distinction is not the same as #155's greyed row, and the difference is what makes both coherent:

- A **track on a record you own part of** is missing. You hold the album; the row is a gap in something you have.
- A **recording something of yours refers to** is not missing. You hold a remix, and the original is a fact about where
  it came from.

So an unresolved reference is drawn as context where it is relevant, offered nowhere as a task, and never counted
against you. It resolves on its own if the thing ever arrives — `Id::of_basis` is convergent, so the id a reference was
minted with is the id the acquisition mints.

**Built, as of #155.** `Proposal::Tracklisted` catalogues the whole tracklist of a medium, so the library holds a row
for a track it has no file of — drawn under its album, at its number, dimmed. Playability is asked at the moment of
playing and answered `no copy of "…" is held`; a pool steps past such rows rather than stopping at one. The sentence
above stopped being a premise and became a thing on screen.

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
gmuse shelf import <paths…>          this audio is new, catalogue it
gmuse shelf apply <file.jsonl>       assert these facts
gmuse shelf query <spec>             ask
```

`import` is built (#62). A dry run unless given `--write`, like `apply`, and for the same reason — it reads tags, and
what it reads becomes claims. With no paths it takes whatever the scan reported as unknown, which is the answer to the
offer the scan made.

All three run **before the lock**: a library operation opens no device and writes only the log, so refusing one because
a player is running would make `import` unusable exactly when you are listening to the library you are editing.

### 2.1 `relink` and `forget`, after working the cases

Both were listed here as commands to build. Working #88's cases showed that one of them exists already and the other is
not the operation it looked like.

**`relink` is `apply`.** "This file is that recording" is what an identification asserts, and it folds to
`EncodingBound { encoding, from, to }`, carrying what it replaced. 377 of them have been applied to this library. There
is no second operation to design and no second way to say it — which is the outcome this model prefers everywhere else.

**`forget` is not about the holding.** Holdings are published **whole** and supersede per device, so a file that goes
away stops being held at the next scan with nothing said. A command that removed one holding could not work anyway: the
next scan would republish the set and overwrite it. Case 4 in #88 asks what distinguishes a deleted file from an
unmounted drive, and the answer is that neither is a holdings question — it is the device's reachability, which is #38.

That leaves the meaning `forget` still wants: **this is not part of my library**, said about a recording rather than
about a file. The recording stays a row, because "decided about, not owned" is ordinary and always was; what changes is
whether it appears. That is an exclusion — #28's missing cell and #87's quarantine — and it is not a third mechanism.

The cases that looked like they needed an operation:

| case                          | what actually happens                                                                            |
| ----------------------------- | ------------------------------------------------------------------------------------------------ |
| a re-buy, performance named   | one recording, two encodings; the canonical rule picks which represents it                       |
| a re-buy, performance unnamed | **splits**, and the repair is a merge — a split is one merge from repaired                       |
| a re-rip in place             | the old encoding stops being held; the song is unchanged                                         |
| a file vanishes               | the holding goes at the next scan; rating and plays survive, because they hang off the recording |
| two files, one recording      | `RecordingMerged`, which carries the history across                                              |

Each is a test in `src/bridge.rs`. The one that surprised: a re-buy of an **unnamed** recording splits, because the
bytes are the only evidence and they say two things. That is not a bug to route around with `forget` — it is the error
this model prefers, and `RecordingMerged` repairs it without destroying anything.

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
{"fact":"tracklisted","release":"996d1c…","position":1,"format":"CD","tracks":[{"position":1,"recording":"298472…","title":"Elephant Gun","artist":"Beirut"}]}
```

**A proposal may be about something you do not have.** `tracklisted` is the first one that is: it says what is on a
record, and most of what it says the library will already know. Its subject is the **medium**, which must already exist
— a tracklist for a pressing nothing here has seen is refused as stale rather than applied, because the ids only line up
where the files carried the album's MusicBrainz id, and a mismatch would catalogue a phantom disc beside the real one.
`ingest tracklist` writes these.

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
  → read the path for facts             paths.py     #143, and it must be first
  → normalise the tag formats           tags.py
  → resolve against MusicBrainz         a lookup pass
  → fingerprint what that missed        AcoustID (#57)
  → fill the gaps by hand or by model   whatever the case needs
  → gmuse library import <dir>          strict, and the last step
```

**`paths.py` runs first, and the order is load-bearing** (#143). A path holds facts nothing else records — a mastering
marker on 144 files, a series in one filename — and its strongest signal for a _truncated_ tag is a value of exactly 30
characters **beside a 128-byte ID3v1 block**. `tags.py` exists to strip those blocks. Run it first and the evidence is
gone: on the local library, where it has already run, every truncation suspect is unconfirmable for exactly that reason.

A Bandcamp purchase is the ordinary case: the core tags are there, and a MusicBrainz or Discogs pass and possibly a
fingerprint are what turn "some files with plausible tags" into "something the library can hold". Each of those is a
small tool, and they are expected to multiply — which is fine, because they are all outside the library and none of them
can corrupt it.

**What the minimum actually is has to be learned.** The honest starting point is what the ladder can already answer
with: a recording identity from a MusicBrainz id or a fingerprint, a title, and an artist. Whether an album and a track
number join that list is a question the first few real imports will settle better than this paragraph can.

### What the gate does, and the one door out of it — #160

Stated here since the beginning, and implemented in the opposite direction until #160: `shelf import` catalogued
whatever it could read and rested on `Basis::Audio` whatever no authority named. It then _reported_ the count, which is
how **522 of 6,927 recordings came to rest on their bytes with 479 of them arriving in a single import**. Nobody chose
that. Nothing asked.

`populate` now refuses a file no authority names, and the refusal prints `artist`, `title` and the path — because a
refusal has to be answerable. Every external route in the flow is album-shaped and this material's album fields are its
_least_ reliable: a chart name, a warez stamp, a DJ-pool folder. `artist` and `title` are its most. `Till Dawn` was
refused by every album-shaped route and comes back at score 100 from one MusicBrainz **recording** search on exactly
those two fields.

**The gate is in `populate`, not in the command.** Seven of its eight callers pass the library roots to bring their view
up to date, and `render --write` persists what it walked — so a refusal made in `shelf import` would be undone by the
next render walking the same file straight back in. Reading is exempt on purpose: `query` and `apply` build a scratch
catalogue that is discarded, and a work list has to show the rows that _do_ rest on their bytes, or the pass that exists
to identify them cannot see them.

**`--unidentified` is the door out, and it is not a formality.** A white label, a live rip, a friend's record:
MusicBrainz has never heard of any of them and they belong here. `Basis::Audio` stays. What changed is that landing on
it is something a person says.

**What this does not do yet.** The ladder §7 wants runs `MusicBrainz id → fingerprint → recording search → refuse`, and
only the first and last rungs are at the front door. The fingerprint rung is #57 and needs a key. The recording-search
rung is `identify`, which today runs two stages later against rows that are already catalogued — so the refused file has
no id to propose against and cannot be handed to it. Closing that is the other half of #160.

### A second copy is reported, not refused — #62

Strictness is about files the library **cannot describe**. A file it describes perfectly well, and has already described
once, is a different question: a re-encode has different bytes by design, so it mints a new encoding and reaches the
same recording through the MusicBrainz id.

That is one recording with two encodings, which is exactly the shape §2.1 settled on for `replace`, and history hangs
off the recording — so it crosses to whichever copy is eventually kept. Nothing is damaged and nothing needs refusing.

What cannot be inferred is **intent**, and there are three of them:

- an **upgrade**, and the old encoding should be retired
- a **duplicate**, and this one should not have come in
- a deliberate **second format**, and both belong

So `import` says so and moves on: `joins a recording already held as N other encoding(s)`. This is the `Uncertain`
answer #62 asks for, and it is the same discipline as the contradiction check (#148) — a rule for picking a winner is a
guess that is wrong sometimes and invisibly so. #35 already cost a 4-star rating to exactly that guess.

**A re-import is not a second copy.** An encoding already in the log says nothing, or the line would print on every
no-op run and stop being read.

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

_Nothing here at present. `forget` and `relink` moved to §2.1 once the cases were worked (#88)._

---

## 9. Related

`layers.md` — who may talk to whom · `domain-model.md` — what the facts are · `library-sync-design.md` — how the log
merges · `tree-view-design.md` — the query language · #62 (mutation API) · #86 (delete the store) · #87 (a bad copy is a
row)
