# The storage layout: one readable tree, and why it may be rearranged freely

**Status: settled design, partly built.** The layout half of #132.

`library-api.md` is the contract this serves: **nothing is implicit.** The filesystem is storage and the log is the
database, so no directory may carry a fact and no path may be the only place something is recorded.

---

## 1. The content-addressed store is withdrawn

An earlier version of this document specified three trees: a `Store/` addressed by encoding id, a rendered `Browse/` of
hardlinks over it, and a `Derived/`. That is withdrawn, and the reason it existed is worth stating, because it is the
reason it no longer needs to.

**The store was protection against rename churn.** #132's argument was that rendered paths move files whose bytes never
changed — acquire disc 2 and every file on disc 1 is renamed, acquire album B and `%aunique` renames album A — and that
each of those was _"exactly the event #62 exists because we cannot reliably infer"_.

**It is inferred now, exactly and cheaply.** A file is identified by hashing its audio packets (#35), the scan matches
by hash and the locator follows (#141), and history hangs off the recording rather than the file (#81). A file renamed
_and_ moved to another folder keeps its row, its identity, its rating and its plays — asserted by
`moving_a_file_costs_nothing_because_identity_is_the_audio`.

So the store defended against an event that has become free, and it charged for the defence: an unreadable tree, a
second rendered tree to compensate, link machinery to build it, and a NAS copy that is either unreadable or needs a
third mechanism. **One readable tree instead.**

What content addressing would still have bought is freedom from filename collisions and length limits. §5 is what that
costs instead — a cost paid at render time, where it can be reported, rather than paid in readability every day.

---

## 2. One tree, and its shape

```
MusicLibrary/
  .gmuse/                             the log, and it is the database
  Dance/    Stephan Bodzin/ Luna/ 01 Triton.flac
  General/  Beirut/ Lon Gisland/ 02 Elephant Gun.mp3
```

**No `Albums/`, `Classical/` or `Singles/` roots.** That argument stands whatever addresses the files:

- **`Singles/` is not a shape.** 591 of 592 files in the dance collection carry an album tag, and 466 of their releases
  are held as exactly one track. A single is a small release, not a missing one — #133 made that a fact (`release_type`)
  and #135 made it a display rule. Neither needed a directory.
- **`Classical/` routes on a hand-set flag**, which is a fact about the music kept where facts cannot live.
  `Work → Recording` already carries it, for everything.

**The top level is a catalog**, and today there are exactly two: `Dance` holds 761 recordings and `General` the rest. A
catalog is a `Collection` in the model, so this is the one level rendering something the library owns rather than
something the tags say. §4 says what happens when that stops having a single answer.

---

## 3. The path is rendered from the library

```
{catalog}/{file_under}/{album}/{disc-}{track} {title}.{ext}
```

- **`file_under`, not `artist`.** The album-level credit where there is one, so a guest credit does not split an album
  off from the artist it belongs to (#84).
- **`album` is the release group's title**, so two pressings of one record land together and a reissue does not make a
  second folder.
- **The disc prefix only when the release has more than one medium** — knowable now that a release carries its disc
  count (#134). If a second disc arrives later the first disc's files are renamed, which is free.
- **The extension is the format's**, so every tool that opens files by extension still works.

**Rendered from the library, not from the tags**, which is the whole difference from beets. The library knows the
release-group credit rather than a guessed album artist, knows a merge happened, and carries a basis and a confidence
for every claim — and it survives a retag, which the tags by definition do not.

---

## 4. Rearranging is free, and that is the point

Change the template, correct an artist's name, merge two recordings: the files move and **nothing is lost**. The scan
recognises each of them by its audio and updates the locator. What used to be the danger is now the ordinary case.

**A move is not an operation.** #62 asked for `add`/`remove`/`move`/`replace` and three of them dissolved; this is why
`move` was one.

**Re-rendering costs a re-hash.** Files at new paths are unknown until the next scan hashes them: 16.5 s for 6,500
files, proportionally less for one album. Correctness is unaffected; only time.

**A recording in two catalogs has no single path.** None is today — the two are disjoint — but nothing prevents it, and
the render must **report it rather than pick one**. Choosing a primary member is the collapse rejected for genre (#93),
and a folder that decided it would be a fact living where facts cannot.

---

## 5. The rendering rules now apply to the real tree

This is the honest cost of dropping the store. These were `Browse/`'s problem, where being wrong was free to fix; they
are the master tree's problem now:

- **Unicode normalisation.** macOS and Linux disagree about composed and decomposed forms; normalise to NFC on write.
- **Case-insensitive collisions.** APFS and SMB are case-insensitive, so two artists differing only in case are one
  directory.
- **Reserved names and characters**, for a tree read over SMB from Windows: `CON`, `PRN`, `:`, `?`, `*`.
- **The 255-byte component cap**, which a long classical title reaches.

**Every one is checkable before the write, and a collision is a report rather than an overwrite.** The renderer proposes
a path, verifies it is free and legal, and refuses the file to a review list if it is not. Silently mangling a name is
how a library develops files nobody can find.

---

## 6. How a file leaves

**Nothing is deleted on its own.** Stated before the mover exists, because a tree that only grows needs a policy first:

1. `remove` writes a tombstone (#62). The library stops offering the row.
2. The bytes move to `MusicLibrary/.departed/`, keeping their rendered path beneath it.
3. Only an explicit, separate purge deletes them.

Two steps, because the first is reversible and the second is not — and the error this project prefers is the repairable
one. A tombstoned file that turns out to be wanted is one undo away; a deleted one is a re-purchase.

---

## 7. Derived copies

A transcode is the same performance, different bytes, and #132 built the edge that says so: `EncodingDerived`.

- **Derive only from lossless, and only where the saving earns a second copy.** For a 320 kbps MP3 the small copy should
  _be_ the file, because transcoding lossy to lossy loses twice for nothing.
- **`from` must be an encoding this library holds.** No edge means "not known to be regenerable", the safe default: a
  file whose source is gone is not disposable, whatever its format.
- **Not filed by format.** A purchased 320 kbps MP3 is a master and lives in the tree like everything else.
  Lossless-versus-lossy does not answer disposability — this library holds lossless files upsampled from lossy ones,
  which is why `Verdict::Suspect` exists.

Derived copies live outside the library tree, because they are output rather than holdings: a `Sync/` for a phone is a
rendering of a _query_, made where it is used.

---

## 8. The archive is a device, and the NAS copy is a copy

`Holding` is `(device, encoding) -> locator`, so **the archive is the device whose holdings are complete**; laptops and
phones hold subsets. #38's Local / Remote / Absent already describes it.

Concretely, that is now what it sounds like:

```sh
rsync -a --delete ~/MusicLibrary/General ~/MusicLibrary/Dance /Volumes/Media/GMuseLibrary/
rsync -a ~/MusicLibrary/.gmuse/ /Volumes/Media/GMuseLibrary/.gmuse/
```

42 GB of audio and 46 MB of log, readable in Finder, on a phone, from any machine — because it is the same tree rather
than an opaque one. **The log is copied, not hosted**: §1 of the design doc makes local-first non-negotiable, and a data
directory on a share means an unmounted NAS is an empty library rather than a degraded one.

The log's own layout is already the sync design — one file per device, append-only, read as a union, stamps that cannot
collide — so copying the directory _is_ the merge. #39 is this with manifests, vintages and tombstones, and it is worth
building when two machines actually disagree.

---

## 9. Non-audio assets stop being a problem

Cover art, `rip.log` and a cue sheet are per-_release_, and a store keyed by encoding id had nowhere to put them. A
folder tree does: **beside the album, where every other tool already looks.**

```
General/Beirut/Lon Gisland/cover.jpg
General/Beirut/Lon Gisland/rip.log
```

A convention rather than a fact, and the rule holds: nothing may be recorded _only_ there. If which image is the front
cover ever needs to be a claim it becomes one — but a file sitting beside its album needs no model to be found.

---

## 10. What is taken from the drafted spec wholesale

The practical half is good and hard-won, and it is about the bytes, which is the one thing the filesystem is genuinely
responsible for: FLAC padding on rip (`--padding=262144`), ReplayGain 2.0 track _and_ album gain, exactly one embedded
cover at ≤800px with the full-resolution copy external, `flac -t` verification against the STREAMINFO MD5, never
transcoding lossy to lossy, keeping `rip.log` and the cue sheet.

97% of this library already carries embedded art; the external full-resolution copy is nearly absent, at 30 JPEGs across
495 albums.

---

## 11. What is left to build

- **The renderer**: propose a path from the library, check it against §5, move the file, record the holding. One
  component, and the only one #132 still needs.
- **Ingest puts a file where it belongs**, as part of #62's declaration, rather than cataloguing it where it landed.
- **Migration**, which is now a re-render of 6,485 files in place rather than a move into a new scheme.

**One hazard before any of that.** The paths carry facts nothing has recorded: the dance collection encodes era, genre
and BPM band in directories, and one file has its series only in its filename. A re-render is the last moment those
paths exist. Read them for facts, propose them, _then_ move the bytes.

---

## 12. Related

#132 (the issue this settles) · #62 (`import`, the declaration) · #26 (the folder tree's three jobs) · #35, #141
(identity is the audio, which is what makes this safe) · #38 (Local / Remote / Absent) · #39 (bundles) · #133 (release
type) · #134 (disc counts, which decide the disc prefix) · #135 (the tree that made `Singles/` unnecessary)
