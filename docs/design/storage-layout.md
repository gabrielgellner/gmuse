# The storage layout: where files live, and why it is not a browsing tree

**Status: settled design, not built.** The layout half of #132.

This says what the **desired** state is, not what is on disk. Nothing in the library today conforms, and that costs
nothing, because **the path is not the binding** — a file is bound to the library by the hash of its audio (#35, #141),
so where it sits is a convenience for people and never a fact the system depends on.

---

## 1. One rule

#26 found that the folder tree was doing three jobs and was qualified for one: **storage**. The other two — browsing and
classification — are what every root on disk today is attempting, and what all of them fail at.

> **The storage tree may use structural facts only: who made it, and what release it is on. Never descriptive ones —
> genre, era, mood, catalog, tempo, or how a file was acquired.**

The reason is not taste. A tree has one dimension and description has many: a file is Jazz _and_ Christmas, Techno _and_
music-to-dance-to, from 2016 _and_ at 128 BPM. A folder must choose one and lie about the rest. Descriptive questions
are answered by `levels` and the filter language, where a row can hold every answer at once.

This deletes, from the NAS and the local library alike:

`Classical/` · `Jazz/` · `Christmas/` · `Experimental/` · `Dance/` · `General/` · `1980-2014/` · `2015-2016/` ·
`Tech House/` · `120 - 125/` · `Singles/`

**Including the catalogs.** `Dance` and `General` are curated collections, a recording may be in several, and the
membership is already 6,311 facts in the log. A folder rendering a collection is a folder that has to pick one.

---

## 2. The layout

```
MusicLibrary/
  .gmuse/                                      the log, and it is the database
  Stephan Bodzin & Marc Romboy/ Luna/ 01 Triton.flac
  2Pac/ All Eyez on Me/ 2-10 All Eyez on Me.mp3
  Various Artists/ 80s Hits/ 04 Tainted Love.mp3
  Jeremie Albino/ Rollin'.mp3                  no release, so no album level
```

```
{file_under}/{album}/{disc-}{track} {title}.{ext}
```

- **`file_under`** — the album-level credit where there is one, else the track's. A guest credit must not split an album
  off from the artist it belongs to (#84).
- **`album`** — the release group's title, so two pressings land together. **Absent when the tags name no release**;
  there is no `Singles/` and no stand-in, because a track on no album is an ordinary thing and not a category.
- **`disc-`** — only where the release has more than one medium, which is knowable now that a release carries its disc
  count (#134).

**Two levels deep, and that is deliberate.** An album folder is the unit people copy — to a stick, to a phone, to a
friend. That is the entire human requirement, and it is the only reason this is not addressed by hash.

### The hard cases stop being cases

**Classical.** `Music/Classical/` has five conventions in twelve folders — composer, surname, composer-and-performer,
ensemble-and-conductor, and a compilation title. Under this rule the library answers "who is this credited to" once, and
all five render the same way. Composer → work → performance is a **view**, over the `Composer` role the model already
has, not a directory.

**Dance singles.** A one-track release gets an album folder like anything else. 466 of them, and that is fine: storage
is one rule with no exceptions, and #135 already collapses one-track groups in the tree so the noise never reaches the
place you actually look.

---

## 3. Why not content-addressed

The earlier draft of this document specified `Store/ab/cd/<uuid>.flac` with a hardlinked `Browse/` over it. Withdrawn.

It existed to prevent rename churn, and #132 called that _"exactly the event #62 exists because we cannot reliably
infer"_. **It is inferred now**: identity is the audio hash, the scan matches by hash, the locator follows, and history
hangs off the recording. A file renamed _and_ moved keeps its row, rating and plays —
`moving_a_file_costs_nothing_because_identity_is_the_audio`.

So it defended against something that had become free, and charged an unreadable tree, a second tree to compensate, link
machinery, and a NAS copy that is unreadable or needs a third mechanism.

What it did buy was a **guarantee** against collisions and length limits. That becomes a **check** — §5 — and that is
the one trade in this document worth arguing about.

---

## 4. Rendered from the library, never from the tags

This is not a preference. The tags on this library cannot support it:

```
folder   Alarm Will Sound performs Aphex Twin/Acoustica
tag      album = "Acoustica: Alarm Will Sound Pe"     ← 30 bytes, an ID3v1 field
```

Several NAS files carry **only ID3v1**, whose fields are 30 bytes, so the folder name holds the full title and the tag
holds a stump. A renderer reading tags would replace good names with truncated ones. The same files show DJ-pool track
numbers — `37 Long Chicken` on a two-track release — where the number is a chart position and not a place on a record.

So the order is forced, and it is the order #143 and #62 already describe:

1. **Consume the path** (#143) — it is evidence, and on these files it is _better_ evidence than the tags.
2. **Identify** — MusicBrainz where possible, so the library's answer is better than either.
3. **Then render**, from the library.

**A file the renderer is not confident about is reported and left where it is.** That is the safety property, and it
works only because the path is not the binding: the library tolerates a file in the wrong place indefinitely.

---

## 5. The check that replaces the guarantee

These were a rendered tree's problem all along; they are now the only tree's problem:

- **Unicode normalisation** — NFC on write, **built** (#151). Measured here, the disagreement is total: all 48 non-ASCII
  names are composed on APFS and decomposed on the share. Both fold composition on lookup so nothing is lost today;
  `ext4` and `btrfs` do not, and there the same album is two directories.
- **Case-insensitive collisions** — APFS and SMB fold case, so two artists differing only in case are one directory.
- **Reserved names and characters** — `CON`, `PRN`, `:`, `?`, `*`, for a tree read over SMB from Windows.
- **The 255-byte component cap** — already reached here: Fiona Apple's album folder is truncated mid-word.
- **Trailing dots**, and carefully: stripping them is a Windows rule, and applied bluntly it turns
  `Manhattan Research, Inc.` into `Manhattan Research, Inc`. The dot is meaning. Strip only where the component would
  otherwise be invalid, and say so.
- **Typographic marks** — folded to ASCII on write, **built** (#167). MusicBrainz catalogues the sleeve and a sleeve is
  typeset: `Doin’ Time` with U+2019, `Froggie Went a‐Courtin’` with U+2010, `Live 1975–85` with an en dash. A path is
  not typeset — it is typed, globbed and tab-completed — and a U+2010 in a name that looks exactly like it has a hyphen
  is a name nobody can reach. Quotes, hyphens, dashes and the no-break space are written as the ASCII they stand for.

  Three limits, and each one is the rule's edge rather than an exception to it. **Accents are spelling**, so `Sinéad`
  and `Björk` keep every mark — `’` and `'` are two renderings of one apostrophe, where `é` and `e` are two letters.
  **Nothing folds into a forbidden character**: `"` is refused here, so `“ ” ″` stay curly rather than land four real
  titles on `_`.

- **An ellipsis is dropped, and no name starts with a dot** — 2026-10-25, **reversing** #167's "`…` is not folded". That
  kept the ellipsis because folding it would meet the trailing-dot rule and delete it, and deleting it is now the point:
  `…`, `...` and `. . .` are typesetting, annoying to type, and a leading one **hides the folder** — Britney Spears'
  `...Baby One More Time` was the one album the Plex rendering held that Plex never showed, because Plex does not scan a
  hidden folder. So an ellipsis goes wherever it stands and the space around it closes to one (`I Am Sasha Fierce`,
  `Oops! I Did It Again`), and a name loses a leading dot or space as it already lost a trailing one. Three dots or
  more: `Vol. 2` and `Jr.` are not ellipses. The `…` that `fit` writes on a name it shortened is not this — it is added
  after, and means "this was cut". Measured: 5 names, 35 paths.

  The library keeps the sleeve's own text either way. This is how a filename is written, at the same standing as
  replacing a colon — and it is what stops the tree churning every time a better catalogue arrives with better
  typography.

**The renderer proposes, verifies, and refuses.** A path that is illegal, too long, or already taken by a different
recording goes to a review list. Silently mangling a name is how a library grows files nobody can find, and this library
already has some.

---

## 6. How a file leaves

**Nothing is deleted on its own**, stated before the mover exists because a tree that only grows needs a policy first:

1. `remove` writes a tombstone (#62); the library stops offering the row.
2. The bytes move to `MusicLibrary/.departed/`, keeping their rendered path beneath it.
3. Only an explicit, separate purge deletes them.

Two steps, because the first is reversible and the second is not.

**Steps 1 and 2 are built** (`gmuse shelf remove`, #159); step 3 is not, and that is the point of it being separate. The
tombstone is `EncodingRetired` and it is a shadow, undone by `EncodingRestored` — one fact, not a re-import.

**Said about the encoding, because that is what left.** The recording stays and so does everything hanging off it: a row
the library holds no copy of is ordinary, and since #155 it is a row drawn on purpose. What stops is the library
claiming a _copy_ it has retired.

**And it refuses to retire bytes the library still holds elsewhere.** Identity is the audio, so two paths can be one
encoding — `Early James` had two folders of the same rip, ten files byte-identical across them, and retiring "the
departed copy" retired the live one too. Measured before the check existed: 6,648 settled became 6,638. A file whose
bytes are still in the library has not left, whatever the folder it is in is called.

---

## 7. Derived copies live outside

A transcode is the same performance, different bytes, and #132 built the edge that says so: `EncodingDerived`.

- **Derive only from lossless**, and only where the saving earns a second copy. For a 320 kbps MP3 the small copy should
  _be_ the file.
- **`from` must be an encoding this library holds.** No edge means "not known to be regenerable", the safe default.
- **Nothing is filed by format.** A purchased 320 kbps MP3 is a master and lives in the tree like everything else;
  lossless-versus-lossy does not answer disposability, which is why the edge exists.

A `Sync/` for a phone is a rendering of a _query_, made where it is used, and it is not part of the library tree.
[`renderings.md`](renderings.md) is where outside is: the Plex tree, a subset device, and the transcode cache.

---

## 8. The archive is a device, and the copy is a copy

`Holding` is `(device, encoding) -> locator`, so **the archive is the device whose holdings are complete**; laptops and
phones hold subsets (#38).

```sh
just archive          # dry run: what would be sent and deleted
just archive write    # rsync -a --no-perms --delete ~/MusicLibrary/ asnas:/volume1/Media/GMuseLibrary/
```

**Over ssh, not through the share** (#328). With rsync on both ends the NAS compares its own disk, so the comparison is
the cheap half: measured on ~12,000 files, a dry run took **0.5 s over ssh against 118 s through the SMB mount**, and
the copy moved 34 MB/s against 3.5–6.5. Two SMB costs went with the mount. It decomposed accented names, so every run
reported ~250 deletes that were not, and it could not keep a file's mtime, so each sync ended re-stamping what it had
just sent. `--no-perms` keeps the 777 the share gave every file rather than rewriting ~9,400 of them to this machine's
modes.

`--delete` is why every dry run is read before a write: a file that left `~/MusicLibrary` without a `remove` would leave
the archive too.

**Striping is no longer the advice.** Over SMB one rsync moved 1.6–1.8 files/s, because each file cost a round trip the
transfer could not overlap, and six workers over disjoint artist folders reached 8.2 files/s. That was the mount's
latency, and one rsync over ssh already runs at 34 MB/s.

**The log is copied, not hosted.** §1 of the design doc makes local-first non-negotiable, and a data directory on a
share means an unmounted NAS is an empty library rather than a degraded one. The log's own layout is already the sync
design — one file per device, append-only, read as a union, stamps that cannot collide — so copying the directory _is_
the merge. #39 is that with manifests, vintages and tombstones, worth building when two machines actually disagree.

**SMB refuses hardlinks** (measured), which costs nothing here: there is one tree and no links in it.

---

## 9. Non-audio assets

Cover art, `rip.log` and a cue sheet are per-_release_, and a hash-addressed store had nowhere to put them. A folder
tree does — beside the album, where every other tool already looks:

```
Beirut/Lon Gisland/cover.jpg
Beirut/Lon Gisland/rip.log
```

A convention, not a fact, and the rule holds: nothing may be recorded _only_ there.

**What counts as one is a named list, because it had to be** (#239). `carry_assets` moved _everything_ left beside the
audio after a placement, which is right for a release folder and wrong for anywhere else — and `shelf import --write`
places from arbitrary paths, so "the folder the audio has gone from" can be `~/Downloads`. It carried a `config.toml`
out of one three times in a single session before the cause turned up.

The list is measured rather than imagined. Every non-audio file in the live library, by extension:

| kind                        | extensions                                          | count |
| --------------------------- | --------------------------------------------------- | ----- |
| cover art                   | `jpg` `jpeg` `png` `bmp` `gif` (and `webp` `tif`)   | 190   |
| what a rip leaves           | `log` `txt` (and `cue` `nfo` `accurip` `md5` `sfv`) | 28    |
| a playlist over one release | `m3u8` (and `m3u`)                                  | 2     |
| Ableton's track analysis    | `asd`                                               | 8     |
| **not a record's**          | `ini` `db` — `desktop.ini` and `Thumbs.db`          | 6     |

So on this library the rule leaves exactly six files behind, all of them something Windows Explorer wrote. `txt` is the
one arguable entry and the measurement decides it: fourteen of them here are rip logs and track lists.

**And a carried file is counted in the report**, separately from the audio placed — a rename nobody asked for and nobody
was told about is how this went unnoticed.

### An import takes the audio and nothing else

The allowlist answered the worst of it. What answers the rest is asking **whose folder it is**: a release folder is in
the library, and nothing outside it is this program's to move. So `carry_assets` runs only when the source is under the
root — a `render` move takes a release from one place in the tree to another and the cover follows (#149's case, where
both folders are the library's), while an `import` from `~/Downloads` takes the audio and leaves the folder as it found
it.

### The system's own files are removed, not carried

`desktop.ini`, `Thumbs.db`, `.DS_Store`, an AppleDouble `._` fork. Each was written without being asked for, means
nothing to anything but the file browser that wrote it, and is remade the moment that browser looks again — so a move
drops them rather than spreading them, and `shelf tidy` sweeps the ones sitting in folders nothing is moving.

**This is the one thing in a library that is removed rather than reported**, and it stays narrow deliberately. The
standing rule is that a measurement marks a file and never moves it: a low-bitrate rip is recorded and queried, never
quarantined, because what looks redundant depends on what the library is _for_. Nothing here depends on that —
`Thumbs.db` is not a copy of anything and answers no question — and
`junk_is_what_the_system_wrote_and_never_a_records_own_file` asserts both halves, because a list that grew into artwork
would delete a record's own files.

`shelf tidy` is a dry run that **names** every file before `--write` removes any, and it skips `.departed`: a
tombstone's folder is not a place to sweep. Measured on this library: 7 removed — 3 `desktop.ini`, 2 `Thumbs.db`, 2
`.DS_Store` — with one left inside `.departed` on purpose.

---

## 10. What is left to build

- **The renderer proposes and checks — built** (`gmuse shelf render`). It says where every file belongs and what stands
  in the way, and moves nothing. On this library: 6,471 would move, 14 refused, 0 settled — everything moves because the
  `General/` and `Dance/` catalog levels go.
- **The mover — built.** `shelf render --write` carries a plan out, and `shelf import --write` places what it takes in.
  A rename rather than a copy, so no byte is touched and a half-finished run is simply finished by the next one; nothing
  is ever overwritten; emptied folders are pruned.
- **Ingest renders on the way in — built.** The declaration places the file, and the holding recorded is the one it will
  actually have rather than wherever it landed. Safe because import is strict: everything in the library is describable,
  so the renderer cannot fail for want of metadata — measured, 0 of 6,485 files lack a name to render. What it can
  refuse is a _destination_, and such a file is catalogued and left where it is, because the library's contents must not
  depend on path lengths.
- **Migration**, which is a re-render — and which must not start until #143 has consumed the paths, because on the ID3v1
  files the path is the better record and a re-render is the last moment it exists.

---

## 11. Related

#132 (the issue this settles) · #26 (the folder tree's three jobs — this is the rule it implied) · #62 (`import`) · #143
(consume the path first, and on this library that is not optional) · #35, #141 (identity is the audio, which is what
makes all of this safe) · #38 (Local / Remote / Absent) · #39 (bundles) · #134 (disc counts) · #135 (the tree that made
`Singles/` unnecessary)
