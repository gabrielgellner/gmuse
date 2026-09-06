# The storage layout: where a file goes, and why it never moves again

**Status: settled design, not built.** This is the layout half of #132 — the conventions, decided, so that the mover has
a specification rather than a direction. What builds it is #62's mutation API, and nothing here can land before it.

`library-api.md` is the contract this serves: **nothing is implicit.** The filesystem is storage and the log is the
database, so no directory may carry a fact and no path may be the only place something is recorded.

---

## 1. Three trees, and only one of them matters

```
Store/     addressed by encoding id. Immutable. What ingest writes. The only tree scanned.
Browse/    hardlinks, rendered from the library. Regenerable, disposable, never scanned, never written through.
Derived/   transcodes, made from the store. Regenerable. Their existence is a fact, not a directory.
```

**The store is the library on disk. The other two are renderings**, and a rendering can be deleted at any moment without
asking anyone. That is the whole shape, and everything below follows from it.

### Why not the drafted spec's three roots

The spec proposed `Albums/`, `Classical/`, `Singles/` and argued the split was structural. #132 rejected it and the
library has since proved the point twice over:

- **`Singles/` is not a shape.** 591 of 592 files in the dance collection carry an album tag; 466 of their releases are
  held as exactly one track. A single is a small release, not a missing one, and #133 made that a fact (`release_type`)
  and #135 made it a display rule. Neither needed a directory.
- **`Classical/` routes on a hand-set flag**, which is a fact about the music encoded in a place that cannot hold facts.
  `Work → Recording` already carries it for everything.

One store, one shape.

---

## 2. A path in the store is derived from the encoding id

```
Store/8f/b0/8fb0dcfb-f145-dfff-6649-784265da0229.flac
```

- **Two levels of hex fanout** from the id's first bytes, so no directory holds more than a few hundred entries at the
  scale this will reach. 6,485 files today; the fanout is for the tens of thousands it will not be surprising to have.
- **The extension is the format's**, so every external tool that opens files by extension still works. Five formats in
  this library: `mp3`, `ogg`, `m4a`, `flac`, `opus`.
- **Nothing else is in the name.** No artist, no title, no year — those are what change, and a name that holds them is a
  rename waiting to happen.

### Why content-addressed, and what it costs

The alternative — beets-style rendered paths — churns, and the drafted spec's own rules are the churn: omit the disc
prefix for single-disc releases and acquiring disc 2 renames every file on disc 1; `%aunique{}` means acquiring album B
renames album A. **Every one of those is a rename of a file whose bytes did not change**, which is exactly the event #62
exists because we cannot reliably infer.

The cost is real and worth saying plainly: **a directory of hashes is not readable.** `Browse/` is the answer, and if
`Browse/` turns out not to be enough in practice then this decision is the one to revisit — not by adding meaning to
store paths, but by admitting the rendering did not work.

**Recovery without gmuse is possible and is a stated requirement.** The log is a plain append-only file; it maps every
id to its title, artist, release and holdings. A store with no log is a pile of hashes, so the log is what must be
backed up first — before the audio, which can at least be re-ripped.

---

## 3. `Browse/` is a rendering, and is never written through

Hardlinks, rendered from the library — not from the tags — into whatever layout reads well:

```
Browse/Stephan Bodzin & Marc Romboy/Luna (2019)/01 Triton.flac
```

**Hardlinks rather than symlinks**, decided this way round:

- A hardlink **is** a real file to every tool, including Finder, which copies a symlink as a link rather than as its
  contents.
- The scanner sets `follow_links(false)` already, so symlinks would be free to ignore — but the scan root is `Store/`
  anyway, so neither tree is walked twice and the argument does not decide anything.
- Where `Browse/` is on another filesystem — an external drive, a share — a hardlink is impossible and it falls back to
  a symlink. That is a degradation, not a second design.

**Never written through, and this is not a convention but a constraint.** Almost every tag writer, `lofty` included,
writes to a temporary file and renames it into place. That replaces the _directory entry_, so a write through a browse
path leaves a divergent regular file and an untouched store copy — silently, and identically for hardlinks and symlinks.
Writing tags is a declared export (`library-api.md`) and it goes to the store path. Re-rendering `Browse/` replaces it
wholesale, which is what makes a stray divergent file harmless.

**This is where §7 of the drafted spec goes.** Unicode normalisation, reserved Windows names, case collisions, the
255-byte cap — all of it applies only here, where being wrong is free to fix by re-rendering. Those same rules over a
master tree are how a library corrupts itself on sync.

---

## 4. `Derived/` holds what can be made again

A transcode is the same performance, different bytes. #132 built the edge that says so — `EncodingDerived` — and the
rule it turns on:

- **Derive only from lossless, and only where the saving earns a second copy.** For a 320 kbps MP3 the small copy should
  _be_ the file, hardlinked, because transcoding lossy to lossy loses twice for nothing.
- **`from` must be an encoding this library holds.** No edge means "not known to be regenerable", which is the safe
  default: a file whose source is gone is not disposable, whatever its format.
- **Nothing is filed here by format.** A purchased 320 kbps MP3 is a master and lives in the store.
  Lossless-versus-lossy does not answer disposability, which is why the edge exists — and this library holds lossless
  files that were upsampled from lossy ones, which is why `Verdict::Suspect` exists.

---

## 5. Ingest is one declared operation

Today files arrive wherever they arrive and a scan reconciles afterwards. Under this layout, importing a file is #62's
declaration: **the bytes move into the store and the holding is recorded in one operation**, not a copy followed by a
scan that guesses what happened.

**Nothing about storage gates on how much is known about a file.** The drafted spec's §11 checklist — MusicBrainz ids,
sort tags, ReplayGain, artwork — is a _report_, never a gate. 7.3% of this library has no MusicBrainz id and some of it
never will; a row nothing has identified is ordinary, and a store that refused it would be refusing music you own.

---

## 6. How a file leaves

**The store never deletes on its own.** A store that only ever grows needs a stated policy before it is built, and this
is it:

1. `remove` writes a tombstone (#62). The library stops offering the row.
2. The bytes move to `Store/.departed/`, still content-addressed, still readable.
3. Only an explicit, separate purge deletes them.

Two steps, because the first is reversible and the second is not — and the error this project prefers is the repairable
one. A tombstoned file that turns out to be wanted is one undo away; a deleted one is a re-purchase.

---

## 7. The archive is a device, not a tree

The NAS holding everything needs no new concept. `Holding` is `(device, encoding) -> locator`, so **the archive is the
device whose holdings are complete**; laptops and phones hold subsets. #38's Local / Remote / Absent already describes
it and #39's bundles are how the facts travel beside the bytes.

That also collapses the drafted spec's `Sync/`: it is not a root, it is a browse tree whose query happens to select the
small copies.

**The archive does not hold derived copies.** They are regenerable by definition, and storing what can be remade is what
the derivation edge exists to let us stop doing. Expressible as a per-device policy now that derivation is a fact; the
default is that an archive holds masters.

---

## 8. What is taken from the drafted spec wholesale

The practical half is good and hard-won, and none of it conflicts with anything here — it is about the bytes, and the
bytes are the one thing the filesystem is genuinely responsible for:

FLAC padding on rip (`--padding=262144`), ReplayGain 2.0 track _and_ album gain, exactly one embedded cover at ≤800px
with the full-resolution copy external, `flac -t` verification against the STREAMINFO MD5, never transcoding lossy to
lossy, keeping `rip.log` and the cue sheet.

97% of this library already carries embedded art, so that half is mostly satisfied. The external full-resolution copy is
not: 30 JPEGs across 495 albums.

---

## 9. What this does not settle

**Non-audio assets have no home yet, and that is deliberate.** Cover art, `rip.log`, cue sheets are per-_release_, and a
store addressed by encoding id has nowhere natural to put them. Today it barely matters — 41 non-audio files against
6,485 audio — but ripping FLAC changes that, because a rip log and a cue sheet arrive with every disc.

The shape that would fit is the one everything else here follows: **content-address the asset too, and make its
attachment to a release a fact.** A directory holding art beside an album is a fact in a place that cannot hold one, and
it dies the moment the album's files are addressed by hash. That needs an entity and a fact family, so it is #139 rather
than a paragraph here.

**Migration is blocked on #62.** Moving the 6,485 files in `~/MusicLibrary/{General,Dance}` into a store is exactly the
mutation #62 exists to declare.

And one hazard worth naming before that migration runs: **the paths carry facts nothing has recorded.** A file called
`GemsOnVHS Field Recordings - Jeremie Albino, "Rollin'".mp3` has the series in its filename and nowhere else; the dance
collection encodes era, genre and BPM band in directories. Migration is the last moment those paths exist. Read them for
facts, propose them, _then_ move the bytes.

---

## 10. Related

#132 (the issue this settles) · #62 (the mutation API that builds it) · #26 (the folder tree's three jobs) · #37 (path
is local state) · #38 (Local / Remote / Absent) · #39 (bundles) · #133 (release type, so a single needs no folder) ·
#134 (partial holdings) · #135 (the tree that made `Singles/` unnecessary)
