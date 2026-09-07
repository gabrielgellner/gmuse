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
  2. ingest apply        match the album to MusicBrainz, tag it
  3. tags apply          one format: ID3v2.4, no ID3v1, no APE
  4. genre propose/apply the `Broad; Style` convention
  5. shelf import        catalogue, render, place                 <- the declaration
  6. rsgain easy         ReplayGain, on the album in its final home
  7. identify + apply    MusicBrainz for whatever step 2 missed
  8. joined + apply      is a credit one artist or several
  9. ingest tracklist    what the record has, including what you do not
 10. audit               what is still wrong
```

**Two orderings are load-bearing and both were learned by breaking them.**

`paths survey` runs **before** `tags apply`, because its strongest signal for a truncated tag is a value of exactly 30
characters sitting beside a 128-byte ID3v1 block — and stripping that block is precisely what `tags` does. Run them the
other way and the evidence is gone; on the local library, where `tags` has already run, every truncation suspect is now
unconfirmable.

`rsgain` runs **after** `shelf import`, because it computes album gain per folder and import is what puts the album in
its folder.

---

## 2. What each stage is for

| stage     | command                                                                           | reads                | writes                           |
| --------- | --------------------------------------------------------------------------------- | -------------------- | -------------------------------- |
| paths     | `gmuse-tools paths survey`                                                        | filenames            | a review TSV                     |
| ingest    | `gmuse-tools ingest apply <dir> --artist X`                                       | tags, MusicBrainz    | **tags**                         |
| tags      | `gmuse-tools tags survey` / `apply`                                               | tags                 | **tags**, with a snapshot        |
| genre     | `gmuse-tools genre propose` / `apply`                                             | MusicBrainz, Discogs | **tags**, with a snapshot        |
| import    | `gmuse shelf import <paths…> --write`                                             | tags, audio          | **the log**, and moves the files |
| identify  | `gmuse shelf query --unidentified` → `identify fetch` / `propose` → `shelf apply` | the work list        | **the log**                      |
| joined    | `gmuse-tools joined survey` / `propose` → `shelf apply`                           | tags, MusicBrainz    | **the log**                      |
| tracklist | `gmuse-tools ingest tracklist` → `shelf apply`                                    | tags, MusicBrainz    | **the log**                      |
| audit     | `gmuse-tools audit library` / `identity` / `dj` / `duplicates`                    | everything           | nothing                          |
| merge     | `shelf apply` with `{"fact":"merged",…}`                                          | —                    | **the log**                      |

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

That is what makes step 2 matter more than it looks. Five FLAC albums brought in without it produced **67 new recordings
for music the library already had**. Retagging both copies from one MusicBrainz release turned 30 of them into what they
actually are: **one recording, two encodings**, which is the shape `library-api.md` §2.1 settled on for `replace`.

`gmuse-tools audit identity` reports what has no id, worst first — 141 albums and 545 files on this library today.

---

## 4. What the stages do not cover

Written down because each of these was hit while doing an ordinary import, and each is a place the flow silently falls
back to hand-work.

- **`paths survey` output has no route into the library.** 394 findings sit in a TSV — mastering markers, key and BPM
  disagreements, truncation suspects — and nothing consumes them. The pass that must run first is the one whose answers
  go nowhere.
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

## 6. Related

`library-api.md` §7 (import is strict, and the pipeline that feeds it) · `layers.md` (who may call whom) ·
`storage-layout.md` §10 (ingest renders on the way in) · #143 (paths first) · #140 (a retag changes nothing) · #62 (a
second copy is reported, not swallowed) · #154 (naming an artist)
