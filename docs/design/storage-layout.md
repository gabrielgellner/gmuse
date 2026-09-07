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

- **Unicode normalisation** — NFC on write, since macOS and Linux disagree about composed forms.
- **Case-insensitive collisions** — APFS and SMB fold case, so two artists differing only in case are one directory.
- **Reserved names and characters** — `CON`, `PRN`, `:`, `?`, `*`, for a tree read over SMB from Windows.
- **The 255-byte component cap** — already reached here: Fiona Apple's album folder is truncated mid-word.
- **Trailing dots**, and carefully: stripping them is a Windows rule, and applied bluntly it turns
  `Manhattan Research, Inc.` into `Manhattan Research, Inc`. The dot is meaning. Strip only where the component would
  otherwise be invalid, and say so.

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

---

## 7. Derived copies live outside

A transcode is the same performance, different bytes, and #132 built the edge that says so: `EncodingDerived`.

- **Derive only from lossless**, and only where the saving earns a second copy. For a 320 kbps MP3 the small copy should
  _be_ the file.
- **`from` must be an encoding this library holds.** No edge means "not known to be regenerable", the safe default.
- **Nothing is filed by format.** A purchased 320 kbps MP3 is a master and lives in the tree like everything else;
  lossless-versus-lossy does not answer disposability, which is why the edge exists.

A `Sync/` for a phone is a rendering of a _query_, made where it is used, and it is not part of the library tree.

---

## 8. The archive is a device, and the copy is a copy

`Holding` is `(device, encoding) -> locator`, so **the archive is the device whose holdings are complete**; laptops and
phones hold subsets (#38).

```sh
rsync -a --delete ~/MusicLibrary/ /Volumes/Media/GMuseLibrary/
```

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

---

## 10. What is left to build

- **The renderer proposes and checks — built** (`gmuse shelf render`). It says where every file belongs and what stands
  in the way, and moves nothing. On this library: 6,471 would move, 14 refused, 0 settled — everything moves because the
  `General/` and `Dance/` catalog levels go.
- **The mover**: take a plan and carry it out. The irreversible half, and deliberately separate.
- **Ingest renders on the way in**, as part of #62's declaration, rather than cataloguing a file where it landed.
- **Migration**, which is a re-render — and which must not start until #143 has consumed the paths, because on the ID3v1
  files the path is the better record and a re-render is the last moment it exists.

---

## 11. Related

#132 (the issue this settles) · #26 (the folder tree's three jobs — this is the rule it implied) · #62 (`import`) · #143
(consume the path first, and on this library that is not optional) · #35, #141 (identity is the audio, which is what
makes all of this safe) · #38 (Local / Remote / Absent) · #39 (bundles) · #134 (disc counts) · #135 (the tree that made
`Singles/` unnecessary)
