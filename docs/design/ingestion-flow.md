# The ingestion flow: which tool touches a file, and in what order

**Status: how it works today**, written after bringing five FLAC albums in by hand and finding out the hard way which
stages exist. Where a stage is missing this says so rather than pretending.

The rule underneath all of it is [`layers.md`](layers.md)'s: **a tool prepares a file, the library takes a declaration,
and nothing infers.** Tools write tags and propose facts; only `gmuse shelf import` and `gmuse shelf apply` change the
library.

---

## 1. The order, and why it is that order

```
a download arrives
  1. paths survey        read what only the filename knows        <- must be first
  2. quality survey      what the bytes are, before you keep them
  3. ingest apply        match the album to MusicBrainz, tag it
  4. tags apply          one format: ID3v2.4, no ID3v1, no APE
  5. genre propose/apply the `Broad; Style` convention
  6. shelf import        catalogue, render, place                 <- the declaration
  7. rsgain easy         ReplayGain, on the album in its final home
  8. quality propose     record what the bytes are, keyed by encoding
  9. identify + apply    MusicBrainz for whatever step 3 missed
 10. identify fingerprint what it sounds like, for what no name reached
 11. joined + apply      is a credit one artist or several
 12. version propose     which rendering of a song this is
 13. ingest tracklist    what the record has, including what you do not
 14. shelf slots --work
     + identify fingerprint  which recording is actually in each seat  <- listens twice
 15. audit               what is still wrong
```

**Two orderings are load-bearing and both were learned by breaking them.**

`paths survey` runs **before** `tags apply`, because its strongest signal for a truncated tag is a value of exactly 30
characters sitting beside a 128-byte ID3v1 block — and stripping that block is precisely what `tags` does. Run them the
other way and the evidence is gone; on the local library, where `tags` has already run, every truncation suspect is now
unconfirmable.

`rsgain` runs **after** `shelf import`, because it computes album gain per folder and import is what puts the album in
its folder.

**Step 6 measures the length, and every later step uses that one.** `audio_id` walks every audio packet to hash it, and
each packet carries the duration of its own frames — so counting them is exact and costs nothing. Every pass that wants
a duration reads it off the work list rather than asking a tagging library, which computes
`size / first frame's bitrate` where a file has no Xing header: measured on this library, **34 files wrong by more than
two seconds, one claiming 1735s for a 287s track**. Duration is what corroborates a MusicBrainz match and what buckets a
duplicate candidate, so one bad number made a file both unidentifiable and undiscoverable.

**Fingerprinting appears twice, and they are different questions.** At step 10 it asks _what is this_ — the rung for a
file no name reached, which is why it runs after `identify` rather than instead of it. At step 14 it asks _which of
these two_ — a contested position (#168) is the one case a name cannot settle at all, because both claimants are called
the same thing and sit on the same record. Measured over the first 80 such copies here, duration left **36 of 80
undecidable** and a fingerprint identified 36, of which **16 moved the file to a different recording**.
`shelf slots --work` emits exactly those copies in the shape `identify fingerprint` already reads.

**Step 14 can only run after step 13**, for the reason step 7 runs after step 6: a position is not contested until a
tracklist has said what belongs in it. On a clean import nothing reaches step 14 at all — five Die Antwoord albums went
through this flow and produced no contested seat, because the album match at step 3 named all 57 files and nothing later
disagreed.

**Step 6 now refuses what nothing has named** (#160). `shelf import` used to catalogue anything readable and rest on
`Basis::Audio` whatever carried no MusicBrainz id — which is how 479 recordings arrived on their bytes in a single run.
It refuses instead, printing `artist`, `title` and the path for each, and `--unidentified` is the door out for a white
label or anything else no authority holds.

That makes the **step 3 / step 9 split visible as the problem it is.** `ingest` matches a whole _album_ and `identify`
searches for a _recording_, and only the first runs before the declaration — so a single, a DJ-pool rip or a track
sliced off a chart compilation walks past the album-shaped gate with nothing to catch it.

**And a single released _as_ a single does worse than walk past it: it is stamped.** A one-track release makes the
tracklist check one title against one title, which prints `exact (100%)` — the flow's strongest verdict — on evidence
that cannot distinguish two pressings of the same song. Measured in §5.2, and the lesson is `identification-lessons.md`
§20. `Till Dawn` is the worked case in §4: refused by every route here, and found at score 100 by one recording search.
A refused file is not forgotten: its bytes are described and held and its tags' claim is recorded, so it appears on
`shelf query --unidentified` as a row with an encoding, a locator and no recording. An ordinary `identified` proposal
against that encoding catalogues it — from the claim, without re-reading the file. `identify propose` reads that list
directly: a row with no recording is the ordinary case, and the proposal names the encoding either way.

---

## 2. What each stage is for

| stage     | command                                                                           | reads                | writes                           |
| --------- | --------------------------------------------------------------------------------- | -------------------- | -------------------------------- |
| paths     | `gmuse-tools paths survey`                                                        | filenames            | a review TSV                     |
| ingest    | `gmuse-tools ingest apply <dir> --artist X`                                       | tags, MusicBrainz    | **tags**                         |
| tags      | `gmuse-tools tags survey` / `apply`                                               | tags                 | **tags**, with a snapshot        |
| genre     | `gmuse-tools genre propose` / `apply`                                             | MusicBrainz, Discogs | **tags**, with a snapshot        |
| import    | `gmuse shelf import <paths…> --into <collection> --write`                         | tags, audio          | **the log**, and moves the files |
| identify  | `gmuse shelf query --unidentified` → `identify fetch` / `propose` → `shelf apply` | the work list        | **the log**                      |
| joined    | `gmuse-tools joined survey` / `propose` → `shelf apply`                           | tags, MusicBrainz    | **the log**                      |
| tracklist | `gmuse-tools ingest tracklist` → `shelf apply`                                    | tags, MusicBrainz    | **the log**                      |
| audit     | `gmuse-tools audit library` / `identity` / `dj` / `duplicates`                    | everything           | nothing                          |
| merge     | `shelf apply` with `{"fact":"merged",…}`                                          | —                    | **the log**                      |

**Import reports an album credited to nobody who is on it.** A warez site had written `[MARAPCANA.COM]` into
`albumartist` on four files; import took it at face value, `file_under` asks the album-level credit first (#84), and
`shelf render` duly made a folder of that name. Nothing was wrong with any single step.

**A name rule is not the check** — a blocklist of domains missed `minimalfreaks.pw`, which was sitting in this library
the whole time. What is checkable without guessing is _corroboration_: an album credit sharing no member with any track
on it, which no MusicBrainz id vouches for, is a claim nothing supports. Measured here, **11 of 733 album credits and
every one of them wrong** — two site stamps, a netlabel, a bare `Various`, and two singles credited to an artist who is
not on them.

**Reported, never refused**, because the same shape is sometimes right: a netlabel in `albumartist` is how Bandcamp
writes one, and `Through My Speakers` is in this library legitimately. It is the one finding that survives the repair,
which is the check working rather than failing.

**Import takes the collection, because bringing music in and saying what it is _for_ are one act** (#94). `--into Dance`
files every recording it catalogues, at the moment a person declares it — the same shape a package manager gives a
dependency group. Deriving it afterwards from the folder is the inference #90 and #94 took away, and `shelf unfiled` is
the repair for material that arrived before this existed rather than the intended route.

**Importing from outside a root works, and did not until it was measured.** Holdings are published whole, so the walk
that publishes them has to cover the roots _and_ what was named: walking the roots alone says the device holds nothing
in a staging folder, which is false the moment a file is taken from one, and the planner then has no `from` for it.
Importing 592 files from `~/DanceMusic-staging` catalogued every one and moved five. The same walk runs again after the
move, because a file refused a destination is still catalogued and still openable (§10), and dropping its holding turns
a playable row into a greyed one.

**A command that moves music throws the player's cache away.** A launch deliberately does not scan — a change to the
library is something you make, not something a launch notices on your behalf — so the cache is believed until something
replaces it. `shelf import --write` renames the files it places, so it discards the cache and the next launch rescans.
Without that the player went on opening pre-rename paths and showed newly imported copies as nothing at all.

**`ingest` and `identify` are both MusicBrainz and they are not the same stage.** This is the single easiest mistake to
make, and it is the one that cost a day here:

- **`ingest`** is **before** import and **album-shaped**. It matches a whole release and accepts **only an exact
  tracklist agreement**, then writes ids into the tags. Strong, because a tracklist is hard to match by accident.
- **`identify`** is **after** import and **recording-shaped**. It reads `shelf query --unidentified` and matches each
  track by name and duration. Weaker: MusicBrainz holds several recordings per song, so a name search returns _a_
  plausible answer rather than _the_ right one. Measured here — of 43 confident proposals for tracks whose MP3 was
  already tagged, **19 agreed with the held id and 4 did not**.

Use `ingest` for anything arriving as an album. `identify` is for the residue.

---

## 3. Why identity is the whole point

A recording with no MusicBrainz id is minted from its **audio bytes**. A second copy of the same music — a FLAC bought
to replace an MP3, a re-rip, a friend's copy — hashes differently by design, so it becomes a **second recording**
carrying its own ratings and plays.

That is what makes step 3 matter more than it looks. Five FLAC albums brought in without it produced **67 new recordings
for music the library already had**. Retagging both copies from one MusicBrainz release turned 30 of them into what they
actually are: **one recording, two encodings**, which is the shape `library-api.md` §2.1 settled on for `replace`.

`gmuse-tools audit identity` reports what has no id, worst first — 141 albums and 545 files on this library today.

---

## 4. What the stages do not cover

Written down because each of these was hit while doing an ordinary import, and each is a place the flow silently falls
back to hand-work.

- **`paths survey` output mostly has no route into the library, and one part now does.** Its 394 findings are all on
  `/Volumes/Media/DanceMusic` — **not one is a file the library holds** — so it is a _pre-import_ report, which is why
  it has no `apply` column: there is nothing to apply it to.

  The **144 mastering markers** are the part that lands, and they land at ingest rather than through a proposal:
  `shelf render` names a placed file from the library, so the `_pn` suffix is gone the moment it is imported — measured,
  147 marked files on the NAS and in staging, **0 in the library**. `read` takes it off the name while the name still
  says it, and records it at `Source::Path`.

  The other **250 are disagreements between a filename and a tag** — 139 keys, 89 artist/title, 10 truncation suspects,
  8 tempos — and they stay unrouted **on purpose**. Neither side is authoritative: a filename and a tag are both
  evidence, and where they disagree and no unique id settles it, the answer is a person. A rule that picked a winner
  would be right about `_pn` and wrong about `37 Long Chicken` on a two-track release.

- **No stage means "same album, better format".** The FLAC-beside-MP3 case is only reachable by getting both sides onto
  one MusicBrainz release and letting the ids converge. It works, but nothing names the operation, so nothing checks it.
- **`ingest` handles mp3 and flac.** Anything else — m4a, opus, ogg — still has no front door.
- **A retag behind the library's back changes nothing** (#140), which is correct and surprising. After `tags` or
  `ingest` edits a file already catalogued, the library keeps its old facts until the file is named to `shelf import`
  explicitly. `shelf import` with no arguments deliberately ignores files it already knows.
- **No proposal names an artist** (#154), so an artist correctly identified by a MusicBrainz id can have no label and be
  unbrowsable.
- **~~A row is a copy, not a performance.~~** Settled by #155: a row is an appearance on a medium, or a recording no
  release holds. An album held in two formats draws one row per track.
- **Nothing owns unpacking.** A download arrives as a zip and the flow starts at a folder, so getting from one to the
  other is hand work every time — and the shape varies by source. Two bought in September 2026 were nested
  `Artist/Album/Artist - NN. Title.flac`; the ones the conventions note describes are flat
  `Artist - Album - NN Title.mp3`. Neither is wrong and no rule covers both.

- **Nothing owns tag _content_.** `tags` normalises the _format_ — one ID3v2.4, no ID3v1 — and `paths survey` reports a
  filename disagreeing with a tag and has nowhere to apply it. Between them there is no stage for "this tag says
  something wrong".

  The worked case: a store had welded `Explicit` into all twelve titles of _Feed the Family_ —
  `Four Men From the Row Explicit`, and so on for every track. `ingest` matched nothing. Stripping the suffix took the
  same album to **100% on the first try**, and one regex was the whole difference between an album that could not enter
  and one that entered exactly. The uniformity is what makes it safe to strip: twelve of twelve is a source artefact,
  and a title that merely contains the word is a different thing.

- **~~Nothing asks whether the library already holds the album.~~** `shelf import` says so now, before its summary and
  on a dry run — which is where the question is worth answering, because it decides which MusicBrainz release to match
  against and what to do with the copy already there. Two signals, and the second is a hint on purpose:

  - **Recording ids**, exact: `already held: 13 of 13 track(s) in "Classics" join recordings this library already has`.
  - **Artist, album and track count**, when no id connects them — which is the material most likely to be a duplicate,
    because an untagged re-rip binds no id and joins nothing.
    `possibly already held: 13 track(s) are filed under "Aphex Twin" / "Classics", and this is 9`.

  An id beats a name, so an album whose recordings join is one finding rather than two. Reported, never decided: one
  record in two formats is a supported shape and often wanted.

  It was the hard one because §5's FLAC-beside-MP3 case had no step that raised it — `shelf import` says "joins a
  recording already held" _after_ you have decided to import. Bringing in _Classics_ meant noticing by hand that a
  13-track MP3 copy was already there, which decided both the release to match against and what to do with the old copy.

- **`shelf remove` takes the folder, artwork and all.** Retiring thirteen MP3s named explicitly moved the whole album
  directory to `.departed`, including thirty-one images and sleeve notes, leaving the FLAC album with no cover while
  every neighbouring album has one. Nothing is lost and it is reversible, which is §6 working; but a format swap wants
  the art to stay, and copying it across is hand work.

- **A one-track tracklist cannot refuse anything, and says `exact` while not refusing.** This is the singles case of
  #163, and it fails in the opposite direction to the one #163 describes: not a miss, but full confidence over one
  comparison. Two one-track `Burn` pressings both satisfy it, and the pass took the **Withdrawn** one over the
  **Official** one because MusicBrainz scored it first. `identification-lessons.md` §20 has the measurement and the free
  second axis (`isrcs`) that nothing reads. Until something does, **assert the pressing by hand for a single** — the
  `--release` escape hatch, used the way §5.1 says it should be, after looking at the release group.

- **The fingerprint is the adjudicator for a single, and it cannot run until after the declaration.** Step 10 reads
  `shelf query --unidentified`, so it is a post-import pass by construction. But for a one-track release the question
  _which recording is this_ is sharpest **before** import, because that is the question step 3 just answered without
  evidence. On `Rattle` it was decisive: AcoustID returned `ead84d80` (`Bingo Players — Rattle`, 287.0s, on 11 releases)
  and `67c985f7` (`Bingo Players feat. Far East Movement — Get Up (Rattle) (original remix)`, 287.0s, on 1) —
  **identical lengths**, so §12 separates nothing. What separated them was §4's rule that a credit is a list: ours has
  one member. The same response also carried `Lana Del Rey — Video Games (Joy Orbison remix)` at 289s, a §19 bad link
  the name check correctly refuses.

- **`genre` has nothing to reach for on a single.** Discogs is reached through a MusicBrainz _release_ relationship, and
  none of four single pressings had one — `source: current-only` on all four. So `Rattle` was filed with **no genre at
  all** and `Come With Me Now` with **`Blues`**, both reported as "already correct", because the fallback compares
  against what is already there and there was nothing better. Picking the Official pressing did not help; re-running
  after the re-match produced the same four `current-only` rows. §4's release/genre coupling, in its null form.

- **`rsgain` runs after import, and #140 means the library does not see it.** Step 7 is after step 6 for a good reason —
  album gain is computed per folder and import is what makes the folder. But writing ReplayGain is a retag, and a retag
  behind the library's back changes nothing: the loudness reaches the log only when the album is named to `shelf import`
  again. §5 does exactly that for tags and the step list does not say to do it for gain.

- **~~A refusal can be the network.~~** `ingest` reported "no MusicBrainz release matched that artist and album" for a
  release MusicBrainz holds and returns at score 100 — the server was throttling, `bronze` returned "no answer was
  obtained", and the caller turned that into a claim about what exists (#262). The refusal was right and the _reason_
  was not, which is worse than it sounds: acting on it means reaching for `--unidentified`, which rests a recording on
  its bytes — the one outcome §3 says this stage exists to prevent.

  It says which happened now:

  ```text
  MusicBrainz did not answer (https://musicbrainz.org/ws/2/release)
  Nothing was searched, so nothing is known about this album yet.
  ```

  `bronze` had always drawn the distinction — `UNANSWERED` is named and its docstring says a caller must not cache it —
  and every caller folded it into "not a 200" anyway. So `bronze.UnansweredError` is raised rather than a third value
  returned: the bug was a caller ignoring a distinction it had been handed, and one that must be caught cannot be
  ignored by accident.

  **What each pass does with it differs, which is why it is raised rather than decided centrally.** `ingest apply` is
  one album and stops. The passes that walk the library survive the row and tally it apart, because losing the run loses
  every rate limit spent reaching it:

  | pass                   | used to record it as               |
  | ---------------------- | ---------------------------------- |
  | `ingest tracklist`     | `release not found`                |
  | `identify propose`     | `no candidates` / `nothing`        |
  | `identify fingerprint` | `nothing MusicBrainz has linked`   |
  | `joined`               | MusicBrainz not knowing the artist |

  The fingerprint one is the worst of them: _nothing has linked this audio_ is the finding that rung exists to produce,
  so a silence could manufacture the conclusion the pass is trusted for. `joined`'s is next, because it memoises per
  name — one throttle stood for that artist for the rest of the run.

- **~~The spectral measurement was in no step of this list.~~** Fixed above, and recorded because of what the absence
  cost: `quality` was mentioned nowhere in this document — not the order, not the table, not the gaps — while
  `CLAUDE.md` says in as many words that **the measurement is the record**. The two halves go in different places:
  `quality survey` reads bytes on disk, so it can refuse a bad file **before** it is kept; `quality propose` measures
  library copies and is keyed by encoding, so it has to come after the declaration.

  **And putting it in the list is not enough to make it run** — the next bullet is why: the seventeen files imported on
  2026-09-25 are still `cutoff_hz=None` after step 8 was run over the whole library.

- **`quality propose` cannot see the music that was just imported, and calls it "nothing to measure".** It asks the
  library for every copy and skips a row carrying no locator, on sound reasoning: nothing here holds a copy, so there is
  nothing to measure, and a missing measurement and a measurement of nothing are different facts. But the view it asks
  is not the one `shelf query` answers from, and the two disagree about holdings:

  | asked of                          | Possessed by Paul James | Ellie Goulding        |
  | --------------------------------- | ----------------------- | --------------------- |
  | `shelf query --every-copy` (Rust) | 25 rows, 25 locators    | 1 row, 1 locator      |
  | `lib.query(every_copy=True)` (py) | 25 rows, **0 locators** | 1 row, **0 locators** |

  Measured: **361 of 7,542 rows carry no locator** in the view `quality propose` reads, and every one of the seventeen
  files imported that day is among them — so a full pass proposed **7,181 measurements, 0 new, 7,181 already known**,
  and the music it was run for was not in it. Nor is it only new music: Billy Bragg, Bob Dylan and Bruce Springsteen
  rows sit in the 361 while `shelf query` locates all of theirs.

  **The skip is silent and indistinguishable from a real one.** `361 row(s) had nothing to measure` is the same sentence
  for a file that could not be decoded and a file the view cannot see, so the stage reports success and the rows stay
  unmeasured with nothing to surface them. `quality survey` measures the same thirteen files in seconds, because it is
  handed a directory and never asks the library where anything is.

- **`quality propose` also takes no work list, so bringing in one album re-asks the whole library.** By design — a
  measurement is keyed by an encoding (#183, #198), and `--analyser` is what makes a re-measure find stale rows. The
  effect at ingest time is that step 8 for a 13-track album is a pass over every copy the library holds: about fourteen
  minutes here, against seconds for `quality survey` on the thirteen. The stage is right and its granularity is wrong
  for this position in the order.

- **No stage will take an album you have most of.** MusicBrainz's only self-titled _Possessed by Paul James_ has 14
  tracks; the download has 13, and the 14th is `[unknown]`. `ingest` refuses at `close (86%)`, and asserting the release
  refuses harder — **`REFUSED — the track counts differ, which no override should paper over`**, which is right, because
  positional track numbers from a 14-track release could land on the wrong 13 files. So `--release` is not the escape
  hatch here, and there is no other.

  What worked was outside the flow: fingerprint all 13, check the recordings against the release, and write 13
  `identified` proposals. All 13 returned exactly one candidate at 0.92-0.93 with durations inside half a second, and
  they are tracks 1-13 **in order** — so the answer was never in doubt and no stage could act on it. §5.3 is the run.

- **A refusal for want of an identity is the door, and it is a good one.** `shelf import` refused all 13 and still held
  their bytes and recorded their tags' claim, which put them on `shelf query --unidentified` with an encoding and a
  locator and **no recording minted**. So the partial album never needed `--unidentified` and nothing rested on its
  bytes. This is the one part of the above that the flow does describe, and it did exactly what it says.

- **An `identified` proposal binds an identity and asserts no credit.** After applying the 13, every row had the right
  MusicBrainz recording, its title and a displayed artist — and `--filter 'artist="Possessed by Paul James"'` returned
  **12**, the album's 13 invisible, because the filter resolves through credits and `RecordingCatalogued` is not one.
  The credit arrived only with the import that followed. So the state between an identification and the next import is
  catalogued and unbrowsable, which is #154's shape reached from the other side.

- **`shelf import` reports "resting on their bytes" for files whose binding held.** Re-importing the 13 after they were
  identified printed `13 resting on their bytes, 0 confirmed` and then placed them correctly, with the MusicBrainz ids
  intact and no second row: the count describes **what the tags claimed**, not what landed, and the binding somebody
  already made is what actually won (§5). It is the one number a person checks to see whether an import went well, and
  on every re-import of a file identified out of band it reads as a failure that did not happen.

- **Choosing a release means choosing a medium, and the tool shows the tracklist.** _Classics_ has twenty pressings in
  its release group. `ingest`'s own search surfaced a 2×12" vinyl at 85%, and asserting it with `--release` wrote
  `media=12" Vinyl` onto a 7digital download and split thirteen tracks across two discs. The **2008 Digital Media**
  pressing in the same group matched at **100%**. The tracklist diff the tool prints does not mention the medium, and
  the medium is what made the difference.

  **And picking the better release made the genre worse.** `genre propose` reaches Discogs through the release id: the
  vinyl had a Discogs link and proposed `Electronic; Techno; Hardcore; IDM`; the digital one has none, so the fallback
  declared the store's own `Dance - Electro` already correct. Two stages, one input, opposite directions.

- **`ingest tracklist` runs last and wants the album id in the tags.** It proposes against the release the _files_
  claim, because that is what `import` read to mint the pressing. A folder whose files carry no `musicbrainz_albumid` is
  skipped, and one whose files disagree about which pressing they are from is skipped too — two records got mixed, and
  guessing which is right would catalogue the wrong tracklist over both.

---

## 5. Worked example: five FLAC albums against MP3s already held

What actually happened, including the parts that failed.

```sh
# 1. the shape they arrive in: `Artist - Album - NN Title.flac`, flat in a zip
#    -> one folder per album, named by the album

# 2. album-level identification, one album at a time
uv run gmuse-tools ingest apply staged/1999 --artist Cassius --album 1999 --write
#    exact (100%), 16 ours vs 16 theirs      -> tagged
#    Malibu: weak (69%)                      -> refused, nothing written
#    Studio1-Variationen: theirs is [untitled] -> refused

# 3. the copies already held were tagged from a *different* release, so
#    retag them from the same one, or they will not converge
uv run gmuse-tools ingest apply ~/MusicLibrary/Cassius/1999 --artist Cassius --album 1999 --write

# 4. the declaration
gmuse shelf import ~/MusicLibrary/_inbox --write
#    38 file(s), 38 confirmed, 30 joining a recording already held

# 5. the copies whose tags changed have to be re-read
gmuse shelf import ~/MusicLibrary/Cassius/1999 … --write

# 6. and where a retag changed the id, re-reading is NOT enough — see below
gmuse shelf apply merges.jsonl --write
```

**Step 5 does less than it looks.** Re-importing a retagged file adds facts but **cannot move a binding**: `ingest`
refuses to mint over a binding somebody already made. So a file whose MusicBrainz id changed keeps the recording its
_old_ tag minted while the new copy beside it takes the new one, and the album splits in half. Eight of these 38 did.

**The repair is a merge, not a relink.** Both ids are correct — MusicBrainz really does hold two recordings, because the
copies were tagged from different releases. `Identified` would move the binding and strand what hung off the old row:
measured here, 1 play, 8 collection memberships and 53 concept assertions. `RecordingMerged` folds in a pre-pass and
every fact after it resolves through the result, so ratings, plays, memberships, concepts and credits follow the winner
— and so does the loser's encodings' binding.

```jsonl
{
  "fact": "merged",
  "loser": "6739b413-…",
  "winner": "06ea25b7-…"
}
```

Result: **16 recordings for 32 Cassius files, 12 for 24, 10 for 20** — one recording per track holding both formats, and
playback picks the FLAC because `for_playback` prefers the higher bitrate. Two albums were refused by step 2 and are
still MP3-only, which is the correct outcome — an unverified match is worse than no match.

---

## 5.1 Worked example: two FLAC albums, September 2026

The second run through, done to find out how much of this a person still has to do. Both albums ended up correct; what
follows is where the flow stopped and waited.

```sh
# 1. unpack — no stage owns this, and the shape was not the documented one
unzip -q "Aphex Twin - Classics.zip" -d staged/

# 2. paths survey — named exactly the 7 truncated filenames, and needed nothing
uv run gmuse-tools paths survey staged/            # 7 differs: title

# 3. ingest — refused both, correctly, and for different reasons
uv run gmuse-tools ingest apply "staged/Aphex Twin/Classics" --artist "Aphex Twin" --album Classics
#    tracklist: close (85%)          -> refused; the search had surfaced a 2x12" vinyl
uv run gmuse-tools ingest apply "staged/Possessed…/Feed the Family (Explicit)" …
#    no MusicBrainz release matched  -> the server was throttling (#262)

# 4. the two repairs nothing owns
#    a) enumerate the release group, take the Digital Media pressing -> 100%
#    b) strip the store's `Explicit` off twelve titles              -> 100%

# 5. genre: Discogs was right for one and had nothing for the other
# 6. import, into the collection the artist was already in
# 7. rsgain, then import again — a retag changes nothing (#140)
```

**Six stages ran with no input**: `paths survey`, `tags survey`, `shelf import`, `shelf remove`, `shelf apply`,
`rsgain`. Every refusal was correct and nothing wrong entered the library, which is the property worth having.

**The interventions were not mostly judgement.** Two were — which collection, and a genre Discogs had nothing for, and
both are the kind a person should make. The rest were the gaps above: unpacking, tag content, the already-held check,
the artwork, the re-import after gain. Those are mechanical, and two of them were what actually blocked an album from
entering at all.

**One was a wrong call I made, and it is instructive.** `--release` is the documented door out of an 85% refusal, and
the first release I put through it was the pressing `ingest`'s own search had surfaced — a vinyl, for a digital
download. The refusal was protecting against something real, and the escape hatch does not ask what the 15% was.

## 5.2 Worked example: four dangling singles, September 2026

The first run through with no album anywhere in it — four loose MP3s from 2014 off an external drive, which is the
material #20 and #51 are about. All four entered and all four are correct. What follows is what that cost, because the
flow's own verdicts were the least reliable part of it.

```sh
# 1. paths survey FIRST — and on one of the four it was the whole ballgame
uv run gmuse-tools paths survey ~/staged-singles
#    2 only-in-path: artist + title of the untagged file

# 2. ingest — accepted two at `exact (100%)`, refused two, and the acceptances
#    were worth less than the refusals (see §4, and identification-lessons §20)

# 3. the two repairs nothing owns
#    a) strip a DJ-pool `(Original Mix)` off one title   -> 0% becomes 100%
#    b) promote the filename into tags on a file with none -> searchable at all

# 4. fingerprint every one of them BY HAND, outside the flow, before importing
# 5. assert the Official pressing with --release where step 2 took a worse one
# 6. import, into the collection each one is actually for
# 7. rsgain, then import again — a retag changes nothing (#140)
```

**Each file failed differently, and the four together cover the shape.**

| file                        | what it arrived with                           | what step 2 did                              |
| --------------------------- | ---------------------------------------------- | -------------------------------------------- |
| `Ellie Goulding — Burn`     | full tags, ISRC, album `Burn - Single`         | `exact (100%)` onto a **Withdrawn** pressing |
| `01 Stay`                   | full tags, but only a track number in the name | `exact (100%)`, and correct                  |
| `Bingo players — Rattle`    | artist + title only, `(Original Mix)`          | `weak (0%)` — refused on the suffix          |
| `KONGOS — Come With Me Now` | **no tags at all**, a DASH remux               | nothing to search with                       |

**The untagged file is why step 1 is step 1.** Its artist and title existed only in its filename, `paths survey` named
exactly that, and `shelf render` renamed it to `01 Come With Me Now.mp3` on the way in — so the evidence was consumed in
the one window where it existed. Rule 9, on a file where the rule was the only thing standing between the library and a
row with no name.

**Two independent routes agreed on all four recordings**, which is the property worth having and the reason this run is
recorded as correct rather than as done: the tracklist match reads _tags_ and the fingerprint reads _audio_, so their
agreement is §1's corroboration rather than a rule agreeing with itself. Where they would have disagreed is the case
step 2 cannot see, and on `Rattle` they very nearly did.

**Six stages ran with no input**: `paths survey`, `tags survey` (4 already modern and alone), `genre propose`,
`shelf import`, `rsgain`, and the re-import. **Five interventions were needed**, and only one was judgement — which
collection. The other four were the gaps in §4: a title suffix, a file with no tags, a pressing asserted by hand twice,
and a fingerprint run outside the flow that the flow only offers after import.

**And #20's layout question got answered by accident.** `shelf render` filed each single as
`{artist}/{title}/01 {title}.mp3` — a one-track album folder named after the song, which is none of #20's three
candidate layouts. It also **sidesteps the `rsgain` trap #20 raises**: album gain is computed per folder, and a folder
holding one track makes album gain equal track gain (`-10.89 dB` for both, on `Burn`), so `replaygain=auto` has nothing
meaningless to average. That is worth deciding on purpose rather than inheriting.

**One oddity, reported and not acted on.** The step 7 re-import printed
`possibly already held: 1 track(s) are filed under "Kongos" / "Come With Me Now", and this is 1` followed by
`No MusicBrainz id connects them, so this is a name agreeing with a name` — for a file whose `UFID` recording id it had
written itself minutes earlier. Nothing acted on it, the hint is documented as a hint, and the row count moved by
exactly +4 (7,283 → 7,287) with one encoding per recording. But the id-level arm of the already-held check did not
connect a file to itself across a retag, which is either a deliberate exclusion of the file's own encoding or a gap, and
nothing written down says which.

---

## 5.3 Worked example: an album you have 13 of 14 of, September 2026

_Possessed by Paul James_, self-titled, bought from the same store as §5.1's _Feed the Family_ and carrying the same
`(Explicit)` marker on the folder. It went in correctly and **no stage could take it** — every route through §1 refused,
each for a defensible reason.

```sh
# 1. paths survey — 2 differs, both filesystem substitutions, tag right both times
#    `Fiddle F**k` -> `F__k`, `Foot in Heaven/Hell` -> `Heaven_Hell`

# 2. quality survey — 13 of 13 `no sharp cutoff`, cliffs 6-13 dB. Real lossless.

# 3. ingest — found the right release unaided and refused
#    tracklist: close (86%), 13 ours vs 14 theirs
#    ingest --release <id>  ->  REFUSED — the track counts differ

# 4. fingerprint all 13 by hand, and check them against the release
#    13/13 one candidate each, 0.92-0.93, tracks 1-13 in order

# 5. shelf import --write  ->  13 refused for want of an identity,
#    bytes held, claims recorded, on the work list, nothing on bytes
# 6. shelf apply 13 `identified` proposals  ->  the recordings are MusicBrainz's
# 7. shelf import --write again  ->  places them AND asserts the credits
# 8. rsgain, genre, import once more for #140
```

**The `(Explicit)` trap did not repeat, and that is the point.** §5.1 stripped a suffix welded into twelve of twelve
titles, and §4 says the uniformity is what licensed it. Here the album **tag** is clean — `Possessed by Paul James`, the
marker is on the folder only — and just **one of 13** titles carries it: `Fiddle F**k Explicit`, censored by the store
against MusicBrainz's `Fiddle Fuck`. A rule that had learned §5.1's lesson too well would have stripped a word off a
title that means it.

**Five of the six tracklist differences were typography.** `Warden's` against `Warden’s`, `Men Men Men` against
`Men, Men, Men`, `Foot in Heaven/Hell` against `Foot in Heaven / Hell`, `Diddly Bo Traditional` against
`Diddly Bo (traditional)`, `Color of My Bloody Nose` against `Colorofmybloodynose`. One — the censored title — was
content. See `identification-lessons.md` §4: the comparison is reading spellings.

**The count is the part no fold can fix.** 13 against 14 is not a spelling, and the missing track is MusicBrainz's
`[unknown]` — a hidden track this copy does not have. Even a perfect title match could never reach `exact`, which is why
`--release` was reached for and why refusing it was right.

**What a person did, and what nothing owns:** unpacking, running the fingerprint, checking 13 recordings against a
release, and writing the proposals. The judgement was one decision — the collection — plus merging the artist first,
which held.

---

## 6. Related

`library-api.md` §7 (import is strict, and the pipeline that feeds it) · `layers.md` (who may call whom) ·
`storage-layout.md` §10 (ingest renders on the way in) · #143 (paths first) · #140 (a retag changes nothing) · #62 (a
second copy is reported, not swallowed) · #154 (naming an artist) · #20, #51 (singles, and the DJ crate at scale) · #163
(the album-shaped passes) · #100 (an id in a frame shape nothing reads) · `identification-lessons.md` §20 (why `exact`
on one track is not evidence)
