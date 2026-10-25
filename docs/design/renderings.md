# Renderings: what leaves the library, and why it is never a master

**Status: proposal, partly built.** The Plex half of #18, the first target of #165, and the subset device of #38.
`gmuse-tools render` copies a query into a target with a manifest (#332); the rest is the Renderings milestone.

The library has one complete copy of what was acquired and one log of what is known about it. Everything else a device
holds — the tree Plex serves, a work laptop's few albums, an MP3 for a phone — is **made from those two**, and can be
made again. This document is about that second kind of file: what it is, how it is made, and what it is not allowed to
become.

---

## 1. One rule

> **The master holds what was acquired. A file gmuse can regenerate is a rendering, and a rendering is never read
> back.**

Two halves, and both are load-bearing.

**What was acquired, not what is unique.** The rule is about _derivation_, not redundancy. A FLAC and a purchased 320
kbps MP3 of one recording are two acquisitions, and both are masters — `storage-layout.md` §7 says so, and the standing
example in `CLAUDE.md` is exactly this pair. A transcode gmuse made from that FLAC is not an acquisition: it is a
function of bytes the library already holds, and storing it beside them would put a cache in the archive. §7 has said
this since #132 — _derived copies live outside_ — and this document is where outside is.

**Never read back.** A rendering carries the library's conclusions in its tags. A scan that read them would be reading
its own output as evidence, which is the loop #165 spends most of its length on (§5). So nothing scans a rendering, and
no rendering is a root.

---

## 2. A rendering is a device, a query and a policy

```
rendering = (device, query, policy)
```

| target      | device                              | query             | policy                                |
| ----------- | ----------------------------------- | ----------------- | ------------------------------------- |
| Plex        | `asnas:/volume1/Media/PlexMusic`    | everything        | lossless → MP3, tags from the library |
| work laptop | that machine's music directory      | a filter you pick | the same                              |
| archive     | `asnas:/volume1/Media/GMuseLibrary` | —                 | **none: a copy, not a rendering**     |

**The archive is not a rendering**, and the table holds it to make the difference visible. It is the device whose
holdings are complete (`storage-layout.md` §8), copied byte for byte with the log beside it, and it is what you restore
from. A rendering is what you _use_ somewhere. Restoring from a rendering would make a transcode a master, which is the
thing §1 forbids.

**A target is a directory here or `HOST:/PATH` over ssh (#336)**, parsed once at the edge, and `just plex` renders the
library to `asnas:/volume1/Media/PlexMusic` beside the archive. Over ssh for #328's reasons, never the SMB mount. Files
are built and tagged on this machine and sent by rsync in batches of 50, with the manifest saved after each; what the
target holds is listed once before planning and once after writing, and **a path the manifest names that is not there is
reported as missing** — rsync has exited 0 on this NAS having skipped files. The NAS's `find` and `xargs` are BusyBox,
so the remote commands use only `find -print0`, `xargs -0`, `rm -f --` and `rmdir --`.

Measured, 2026-10-24: a dry run of the whole library against the NAS takes 2.6 s and plans 8,440 files, about 59 GiB —
some 31 minutes at 34 MB/s, plus the encodes. Three artists, 32 files and 289 MB, landed in 29 s with their six encodes;
a second run sent nothing; narrowing the query deleted 29 on the NAS and pruned the folders they left.

**The device you import on stays complete.** A partial device is a rendering of a query; the place new music arrives is
not partial, because `import` binds a file into the library that the archive then copies.

**A query is the filter language**, the one `shelf query` and the player already speak. A work laptop's subset is
`collection=Dance` or a saved rule, not a list of folders, so it follows the library as it changes rather than going
stale against it.

---

## 3. The policy

Four decisions per recording, made in this order.

1. **Which encoding — per place, not per recording** (#333). A place is the rendered path without its extension: a FLAC
   and an MP3 of one track compete for one, and the best this device holds wins, by `canonical_key` — the order a view
   ranks copies by (#325). **One recording on an album and on a compilation is two places**, and so is one recording
   twice on one album (`Furious Angels` holds an instrumental at 1-08 and 2-06); choosing per recording left a hole in
   one of them, and Plex shows albums whole. Two copies of one recording that the layout calls contested for one path
   are one place too, and the better is rendered; two _recordings_ wanting one path stay refused. Where only an id
   separates the copies, the pick is **reported as a tie**, and the manifest records which copy each path came from. It
   never retires the other — choosing for a target and choosing for the library are different decisions.
2. **Whether to transcode.** **Lossless only**, to the target's lossy format (MP3 320 for Plexamp). A lossy master is
   copied as it is: transcoding Vorbis to MP3 loses twice for nothing, and a client that cannot play the source has the
   server transcode it on the fly, which is cheap for audio.
3. **The tags.** Written from the library, not copied from the master (§5).
4. **The path.** `storage-layout.md` §2's layout, rendered from the library. The masters do not conform yet; a rendering
   can, today, because it is generated whole.

**Measured, 2026-10-24**, on the whole library: 8,440 places from the 8,523 copies held here, no contest left refused
and no tie. Choosing per recording had rendered 8,081 and reported 17 ties, every one of them an album and a
compilation.

**Measured, 2026-10-24.** One 9-minute FLAC to MP3 320 with ffmpeg/LAME took **3.8 s on one core**, about 140× real
time. Every FLAC the library holds is **24.8 hours** of audio — about eleven minutes of one core, a minute or two across
ten. A subset of a few albums is seconds. **The network is the slower half**: a FLAC is about 2.5× its transcode, so the
transcode happens where the FLAC already is and the MP3 travels.

**So transcoding happens on render, not ahead of it**, and a cache keyed by `(audio id, encoder, settings)` means each
FLAC is encoded once however many targets want it. Changing the encoder or its settings changes the key, which
re-renders everything that used it — the right answer, since the old files really are a different encoding.

**Built (#334).** A target names its policy — `--encode mp3-320`, `mp3-v0` or `copy` — and remembers it like its query.
The key is the encoder and its settings, **not its version**: an upgraded LAME makes the same kind of file, and keying
on it would re-render the library on every `brew upgrade`. The cache is `~/Library/Caches/gmuse/transcodes`, beside
`bronze`'s, on the machine that renders: a device fact, what a backup skips, and never `data_dir`, which the archive
copies. Each encode is checked against its source's length with `ffprobe`; a short one fails and nothing lands. An
encode can collide where the layout saw nothing — `05 Lush.flac` becomes `05 Lush.mp3` — so two files wanting one final
path are both refused.

**Measured again, 2026-10-24, on the whole library**: 314 lossless copies, 25.9 hours, encoded to MP3 320 in **210 s
wall** across ten workers, none failing their length check, 3.47 GiB. That is slower than the single-file estimate above
— efficiency cores, an `ffprobe` per file, and the disk — and still a one-off: a second pass is all cache hits.

---

## 4. The manifest

Each rendering keeps a manifest: one row per target path.

```
target path  →  source audio id · encoder key · tag-set hash · which copy was chosen
```

**It makes rendering incremental.** A file is written again only when one of those inputs changes, and a path that
leaves the manifest is deleted from the target. Plex sees the changes that happened, not nine thousand rewritten files
after every run.

**It is the identity bridge**, which is what #18 could not find. That issue's hardest question was identity across the
boundary: Plex has its own ids, gmuse binds by audio, and the paths differ. The manifest answers it exactly, because
every file in a rendering is one gmuse wrote: _Plex's path → manifest → audio id → recording._ Nothing is matched by
name, which is the point — a name match is how the wrong rating lands on the right title.

**It lives with the target**, so a rendering describes itself. A target with no manifest is one gmuse did not make, and
the renderer refuses to write into it rather than adopting what is there.

---

## 5. Tags go into renderings first

#165 serialises the library into the files, and its 2026-09-11 comment assumes **in place**: retag the masters, change
nothing else. Writing into a rendering instead removes the two costs that issue names.

**The loop goes away by construction.** #165's subtle failure is corroboration: once the library writes its conclusions
into a file, a pass treating that file as an independent witness is checking its own homework
(`identification-lessons.md` §19). A rendering is never scanned (§1), so the library cannot read its own output. That is
enforced by which roots exist, not by remembering which tags were ours.

**The cold cache goes away too.** Retagging a master changes its size and mtime, which the read cache guards on; #165
measured a whole-library retag turning the next `shelf query` from 1.3 s into 24 s. A rendering leaves the masters
alone.

**What to write is #165's answer unchanged**: a value at a rung that outranks `TagVariant` — `MusicBrainz`, `Discogs`,
`Manual` — is something the library knows and the file does not. A rendering can go further than an in-place retag
could, because it is not overwriting anything: every field the library has, whatever its rung, since the tags on a
rendering are the library's statement and nothing else's.

**Retagging the masters stays possible**, as a later and separate choice, for players that read `~/MusicLibrary`
directly. It inherits both costs and the loop, which is why it is not first.

**Built (#335).** `gmuse_core.Library.statement` hands out what the library says about each copy's appearance — title,
credited artist, album, album artist, track, disc and disc count, year, label, genres, compilation, the MusicBrainz
recording and release, and the source's ReplayGain — and `stated.py` writes it with mutagen, on the temporary, before
the file lands. **Not `lofty`, as #335 said**: `src/python.rs` keeps tag writing in Python, the tools already write with
mutagen, and a second writer behind the binding would be a second answer. The library states; the tool writes.

- **What the library does not say is removed**, so no stale value contradicts it. **Pictures, lyrics and comments are
  kept**, because the library does not model them.
- **An MP3 ends with one ID3v2.4 tag**: the ID3v1 block, the APE tag and any stacked second ID3v2 tag are cut with
  `tags.py`'s own verified cuts.
- **A transcode carries its master's pictures**, which ffmpeg's audio-only encode drops.
- **The manifest row carries the statement's hash**, so a retitle rewrites exactly the files it changed.

Measured on three albums, 2026-10-24: the 45 masters byte-identical before and after; all 19 copies tagged in place
still hash to the encoding they were made from; James Blake's _The Bells Sketch_, whose master tags have two titles
transposed, renders with the library's; a second run writes nothing. Art beside an album was not carried by #335 —
Orbital's FLACs hold none embedded, and their `cover.jpg` stayed behind.

**The cover beside each album is carried (#348)**: `cover.jpg` or `cover.png`, the names `covers apply` writes and Plex
reads, and not rip logs or cue sheets, which help nobody listening. It comes from the master folders the album's files
came from, and is in the manifest like a track — keyed by the source's size and mtime, rewritten when that changes,
deleted with its album. **Two folders offering different covers is a question**, reported, and the album gets none until
a person picks. A track on no release lands directly under its artist and gets no cover, which would be the artist's.
Measured on the whole library: 1,135 rendered album folders, 1,078 with a cover, 56 with none offered, and one contest —
Enya's _The Celts_, filed from the _Enya_ and _The Celts_ folders, whose covers differ.

**A missing or small cover is filled in the masters, not the rendering** (#352): `covers fetch` offers the Cover Art
Archive's 1,200 px front for each album whose release the library names, falling back to its release group, and
`covers fetch-apply` writes the rows a person marked — an upgrade keeping the old cover as `cover-small.*`. A rendering
then carries it like any other cover; art that lived only in a rendering could not be rebuilt from the masters.

---

## 6. Plex

**Plex is a reader of a rendering and nothing more.** The target is mounted read-only for Plex, so it can delete nothing
and rename nothing whatever its settings say.

**The second truth is not in the files.** Plex does not, as far as we know, write tags or move music. What it does is
store an edit made in its own UI in its own database, and **lock that field**, so a locked field ignores the tags from
then on. No file permission prevents it. So: **metadata is not edited in Plex**, and a field found locked through Plex's
API is reported, never unlocked on its own (`CLAUDE.md`, _reported, never decided_).

**Settings that follow from the rule**, to verify against the running server rather than trust from here: prefer local
metadata over Plex's online match, which is known to merge and split artists; do not empty the trash automatically after
a scan, so a half-pushed rendering cannot drop items; scan after a push rather than on every change.

### Reading Plex back: Plex is one of your devices

**Settled in #337.** A play is `RecordingPlayed` and a rating `RecordingRated`; neither carries a `Source`, because
neither is a claim about a name — who said it and when is the record's **stamp**, `(wall, counter, device)`. So a play
or a rating read from Plex is recorded **as the Plex server's own**: in a log of its own, `log-<device>.jsonl` like
every device's, the id derived from `plex:<machineIdentifier>`, stamped with Plex's time for it (`viewedAt`,
`lastRatedAt`) and with the server as the device — `DeviceKind::PlexServer`, which the model has carried since #75 for
this. Each item is found through the manifest (§4), by audio, never by name.

- **Provenance is the stamp.** "This came from Plex" is the device; no fact grows a field to say it.
- **Times are true.** A play on the phone last Tuesday sorts last Tuesday, and "last played" is right whichever device
  played it.
- **A rating is the latest, as across any two devices.** Rating a track in Plexamp is the same act as rating it at the
  laptop, with the same standing; `RecordingRated` already resolves two devices by the later stamp and keeps both in the
  log. **This replaces "a rating set in Plex is a claim"**, which would have made the phone a second-class device and
  needed machinery to show and settle claims nobody disputes.
- **Reading twice is harmless by construction.** The library's records are a set, so a record with the same stamp and
  fact is one record; a stamp built from Plex's own time and device is the same on every read. The reader also starts
  from the newest stamp already in the Plex log, so it does not ask for what it has.

**Two rules come with it.**

- **One machine reads Plex** — the one that renders. Every log has one appender (#218), and two machines reading the
  same server would both be writing the Plex device's log.
- **No echo.** A rating gmuse pushes to Plex comes back with a new `lastRatedAt`. The reader records a Plex rating only
  where it **differs** from what the library already says, so a push never returns as a new rating; a re-rate to the
  same stars is nothing to record.

**Built (#338).** `gmuse-tools plex read` asks the server for every play and rating in its music libraries, resolves
each item's file through the rendering's manifest, and writes proposals carrying Plex's stamps; `gmuse shelf apply`
records them in the Plex device's log — `Proposal::Played` and a `Rated` with an `elsewhere` stamp, judged as records
rather than by whether the fact was ever said, because two plays of one track are one fact told apart by when. While an
engine holds the log they are refused rather than handed over, since an engine stamps with its own clock. A half star
(Plex counts to ten) is reported, not rounded.

Measured against the server, 2026-10-24: 14,122 plays and 63 ratings, all of files in the old libraries and none in the
rendering, which Plex does not read yet — so the first read proposes nothing, and #339 is how those come in. A read
takes 7 s.

**And the old libraries, by their audio (#339).** A file outside the rendering is placed by hashing its bytes through
the share with the library's own hash (`encoding_at`) and finding that encoding — never by its name. Hashes are kept by
path, size and mtime, so a re-read hashes nothing. What cannot be placed goes to `tools/out/plex-unplaced.tsv` with its
plays and rating attached, because for a file the library does not hold that history is its only record.

Measured, 2026-10-24: **12,957 of 14,122 plays and 53 of 63 ratings placed** — 1,868 files hashed through the mount in
about seven minutes. Left: 927 plays of 335 files whose audio the library does not hold (often a different download of a
recording it does — Lewis Del Mar's _AUGUST_, the standard edition of _Speak for Yourself_), 223 plays of 12 files whose
audio could not be read (WMA, which Symphonia does not decode), and 15 plays of items Plex no longer has.

### Playlists (#349)

**A playlist is an ordered collection** (#118), so a Plex playlist comes in as one, each entry placed like a play — the
manifest, then the audio — at **its index in Plex's list**, so an entry that cannot be placed leaves a gap rather than
shifting the rest. `CollectedInto` gained a `position` for it; a seeded catalog still shares one, because a folder has
no running order. A smart playlist is a rule Plex re-evaluates, not a list, and is not read.

**The playlists Plex holds today become gmuse's**, against #349's default of "a Plex playlist stays Plex's": they point
at the old libraries' items, which the switch (#340) removes, so a playlist left at home in Plex would empty. gmuse
pushes them back once Plex reads the rendering. Mirroring a playlist made in Plexamp _after_ the switch — read-only
here, home in Plex — is still #349's to build.

Measured, 2026-10-25: two hand-made playlists, 159 of 170 entries placed (_Did someone say Pop?_ 144 of 153, _Some dance
favs_ 15 of 17); the eleven left are audio the library does not hold or cannot read. Five smart playlists left as rules.

**Read before every push.** A push that renames a path makes Plex start that track over, and the plays it held for the
old path go with it. Reading first means they are already in the log.

**And once, before the switch.** The Plex library on `Media/Music` holds years of plays and ratings keyed to paths that
no rendering will reproduce. They are read out and recorded before Plex is pointed at the rendering, or they are lost.

---

## 7. Two acquisitions of one recording

A rendering prefers lossless, so a FLAC held beside an MP3 never sends the MP3 anywhere. **The pair costs disk and
nothing else**, and the transcode model works identically with both present.

Measured with `shelf copies`, 2026-10-24: **73 recordings** are held lossless and lossy. **69 sit in one folder, across
six albums** — Orbital's _Diversions_, Imogen Heap's _Speak for Yourself_, Anderson .Paak's _Malibu_ among them — and
look like a FLAC bought over an MP3 already held. **4 are on different releases**, an album and a compilation, which is
the supported shape and not a pair at all. The lossy copies total 872 MB.

**One case does sit badly with §1: an MP3 that is provably a transcode of the FLAC beside it.** That file is regenerable
in every sense that matters, and a purchase in both formats from one shop is close to it. So there are two checks, and
both are reports:

- **Is this lossy copy a transcode of that lossless one?** Same length to the sample and correlated audio say yes; a
  different length or a different master says it is a second acquisition, and the question does not arise. A yes is a
  candidate to retire with `shelf remove`, which `.departed/` makes reversible — chosen per album by a person.
- **Before trusting either answer, is the FLAC lossless at all?** A lossy source in a FLAC container makes the MP3 the
  better file and a transcode of the FLAC a second generation. `Fact::EncodingMeasured` is where that is already
  recorded.

**At import, a notice and not a gate.** A lossy copy arriving for a recording held lossless, or the reverse, is named
and pointed at `replace` (#159). It is not refused, because sometimes keeping both is the point, and the import cannot
know which time this is.

---

## 8. Where it runs

**A tool, not a core operation.** #200 settled that a layout is presentation and moving files is a tool's job, and the
#165 comment that a retagger is the purer case: read the library, write files, stop. A renderer appends no facts — the
manifest is the target's own record, not the library's — so it needs read access, which `gmuse_core` (#197) now gives.

The one exception is §6's readback, which **does** produce facts — plays and rating claims — and so enters through a
declared operation like any other import, never by a renderer writing to the log.

---

## 9. Open

- ~~**The Plex source.**~~ Settled in #337: not a `Source` at all. Plex is a device, its plays and ratings live in its
  own log under its own stamps, and a rating from Plex has the standing of a rating from any device (§6).
- ~~**The encoder setting.**~~ Settled in #334: `mp3-320` for Plex, as asked, with `mp3-v0` available. Either is a cache
  key, so the choice can change later at the cost of one re-render.
- ~~**Where the transcode cache lives.**~~ Settled in #334: on the machine that renders, in the tools' cache directory.
  The FLAC is there, and the network is the slower half (§3).
- ~~**Tracks on no release.**~~ Settled in #335: no album tag, because the library names none, and Plex will file it as
  "Unknown Album". That is Plex's word for the truth, and a rendering must not invent a release to avoid it.
- **A target's subset changing.** Narrowing a work laptop's query deletes from that device. That is what a rendering
  means, and it wants a dry run the way `archive write` does.
