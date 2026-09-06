# tools

Out-of-tree helpers for the music library. **None of this is gmuse.**

§1 puts tag editing out of scope, and the pattern is the one rsgain and bliss already set: an external tool writes,
gmuse reads. Reading new files into the library is likewise a CLI job rather than something the app does. Python because
these are I/O-bound, edited between runs, and reviewed by a human — and because keeping them out of `Cargo.toml` is what
keeps the boundary real.

## One command

A `uv` project with a Typer CLI, because twelve scripts each with their own `argparse` and its own idea of how to be
invoked — `python -m genre.propose ~/MusicLibrary` beside `python quality.py survey` — is not a thing anyone remembers.

```sh
cd tools
uv sync                                    # once, from uv.lock
uv run gmuse-tools --help

uv run gmuse-tools identify fetch ~/MusicLibrary
uv run gmuse-tools identify propose ~/MusicLibrary
uv run gmuse-tools quality survey
uv run gmuse-tools genre fetch
uv run gmuse-tools cache stats
uv run gmuse-tools audit library
uv run gmuse-tools audit duplicates work.jsonl   # needs fpcalc
```

| group      | commands                                                | subject                 |
| ---------- | ------------------------------------------------------- | ----------------------- |
| `identify` | `fetch` `propose`                                       | what a **recording** is |
| `quality`  | `survey` `albums` `propose` `quarantine` `unquarantine` | what the **bytes** are  |
| `genre`    | `fetch` `propose` `hierarchy` `apply` `snapshot`        | the vocabulary          |
| `cache`    | `stats` `prune`                                         | what an authority said  |
| `audit`    | `library` `dj` `duplicates`                             | what is wrong           |
| `credits`  | `scan` `apply` `restore`                                | who did what            |
| `tags`     | `survey` `apply` `restore`                              | the serialization       |
| `ingest`   | `apply`                                                 | bringing one in         |

**Every pass shares a shape: read something, say what would happen, and do it only when told.** That is
`gmuse library apply`'s shape too, and not by accident — these produce the files it consumes. Anything that writes takes
`--write`; without it you get a dry run and a count.

The groups are nouns on purpose. `identify` and `quality` are the two halves of "what is this file" and they answer
about _different subjects_ — a recording and an encoding. Keeping them apart where someone types is where that
distinction is most often forgotten.

## `audit duplicates` — the same music held twice, when the bytes do not say so

The scan already reports **byte-identical** files: two paths minting one encoding, which costs only disk. That half is
exact and needs no tool.

This is the other half. `~/MusicLibrary` held Thomas Brinkmann's _Studio 1_ in two folders differing only in spacing,
and only four of thirteen tracks were byte-identical. The rest are **separate encodes of the same master** — identical
duration, identical CBR 320, identical size, and 180,518 differing bytes spread to the last one. Different packets mean
different encodings and different recordings, so the library showed **22 rows for a 13-track album**: nine phantoms you
can play and rate, splitting the history of music you own once. No exact hash finds those.

**Two stages, because only one of them is expensive.** Nomination uses what the library already knows — same artist,
lengths within a second — which on 6,311 files is 2,603 candidate pairs over 2,579 files and finds all nine known
Brinkmann pairs. Confirmation fingerprints only those, in parallel: about a minute.

**Nominating on the title as well is the tempting mistake.** It is tighter, it is obviously right, and it scores 0 of 9
— because the copy worth finding is precisely the one whose tags were lost. A test pins that.

### It reports and does not decide, and the first version of it did

The first version scored each pair and printed a verdict. Its verdict on the Brinkmann pair was **"the second buys
nothing"**. The second holds Picard's whole release metadata — barcode, catalogue number, ASIN — and the side it
preferred was better only on usable titles and play history. A judgement between real trade-offs, printed as a fact.

The same overreach sat one level earlier: a 0.90 cutoff meant "below this, not the same music", which silently discarded
every one of these —

| score |                                               |                    |
| ----- | --------------------------------------------- | ------------------ |
| 0.883 | Emmylou Harris — `Hello Stranger`             | `Hello Stranger`   |
| 0.812 | Nelly Furtado — `Te busqué (Spanish version)` | `Te busqué`        |
| 0.754 | Pixies — `Holiday Song`                       | `The Holiday Song` |
| 0.734 | Woody Guthrie — `Brown Eyes`                  | `Brown Eyes`       |

— and a further 38 pairs scored **0.0**, which was not a fingerprint disagreeing but `fpcalc` failing to read the file
at all. "Could not check" and "definitely unrelated" were the same answer.

So the fingerprint is reported and never thresholded into a conclusion. Two lists whose names describe the evidence, a
third for what could not be read, and a count of what sits below the noise floor:

```
2603 candidate pair(s) over 2579 file(s)

── identical audio — 9 ──

  Dance/Thomas Brinkmann/Studio 1 - Variationen/… - 01 Variation1.mp3
  Dance/Thomas Brinkmann/Studio1-Variationen/01 Variation1.mp3
    - fingerprint 1.000
    - same length 320.03s
    - both mp3 at 320k
    - same size to within a tag
    - folder names differ only in spelling: "Studio 1 - Variationen" vs "Studio1-Variationen"
    - titled differently: "[untitled]" vs "Variation1"
    - tag count 34 vs 13
    - MusicBrainz ids on the first only

── close — 26 ──
── could not fingerprint — 38 ──

2530 pair(s) scored below 0.60 and are not listed.
Nothing here is a recommendation. Which copy to keep, and whether to keep both, is yours.
```

The last two evidence lines are the ones a person decides on, and the first version left them out entirely — choosing
between the Brinkmann copies meant hand-inspecting both for Picard tags and ReplayGain **after** the tool had announced
which was worthless.

The `could not fingerprint` list earns its place immediately: the 38 pairs are 48 files, and **22 of them will not
decode** — `fpcalc` reports `Invalid data found when processing input`, mostly across Ween and the Pixies. gmuse reads
them, so this is a stricter decoder finding something gmuse's does not, and it is worth knowing either way.

**It needs `fpcalc`** (`brew install chromaprint`), the only external binary beyond `ffprobe`. Feed it a work list:
`gmuse shelf query > work.jsonl`.

## Checks

`uv` is the cargo of this subproject: `pyproject.toml` declares the dependencies and `uv.lock` pins them, so `uv sync`
reproduces the environment rather than approximating it.

Checked the same way sdmxlib is: `ruff` on `select = ["ALL"]` and `pyrefly` on `preset = "all"`, subtract with a written
reason, **no suppressions**. Both are at zero findings.

```sh
uv run ruff check . && uv run ruff format --check . && uv run pyrefly check
```

Two subtractions were added for Typer, and both are the framework's interface rather than a smell the rule was written
for:

- **`B008`** — `typer.Argument(...)` in a parameter default _is_ how a parameter is declared. `extend-immutable-calls`
  exists for this.
- **`TC003`** — Typer builds the CLI by reading annotations **at runtime**, so a type moved into a checking block is one
  Typer cannot see.

`FBT001` was not subtracted. Its complaint is about _positional_ booleans, and making the flags keyword-only satisfies
it honestly — Typer is happy either way.

The full-preset pyrefly run found five defects here and every one was real: a `str | None` leaking `Any` out of a
`.get()` chain, two `list()` calls on unchecked JSON, an empty container with no inferable type, and three imports that
resolved only because Python puts a script's own directory on `sys.path`. That last one is why this is a package now.

## bronze.py — a request cache, not library state (#58)

Every MusicBrainz and Discogs call goes through it. The rule that decides what belongs here:

> Deleting this may cost time. It must never cost a fact.

So it lives in `~/Library/Caches/gmuse/` (or `$XDG_CACHE_HOME/gmuse`) — **not** the library's `data_dir`, which syncs
between machines and would turn one machine's lookups into the other's facts, and not `out/`, which is the repository.
Anything worth keeping has to be extracted into the library before the cache goes; that is what the proposal files are
for. If a claim only exists in here, it is not yet known.

**Nothing is evicted automatically.** Keeping it a year and dropping it tomorrow are both correct, and neither is a
decision the module makes — which is how it can be as persistent as you like without being coupled to anything.

```sh
./.venv/bin/python bronze.py stats
./.venv/bin/python bronze.py prune --older-than 90
```

Three things it fixes:

|                             | before                                                                                      | now                                 |
| --------------------------- | ------------------------------------------------------------------------------------------- | ----------------------------------- |
| `discogs_release`           | never cached — up to 3 per album, every run                                                 | cached, whole response kept         |
| a search that finds nothing | printed, never written down; re-asked forever at 4 requests each                            | recorded, so the second run is free |
| `identify`'s key            | `(artist, title)` — a cache of the _current guess_, thrown away whenever the query improved | the URL actually fetched            |

**Secrets never land in it.** Discogs takes its token as a query parameter; it is stripped from the key and the stored
URL, and a check at write time refuses the row if one gets through anyway.

## identify.py — find the MusicBrainz recording id (#89)

The gap that looked like an identity problem is a **tagging** problem. Of 6500 files in `~/MusicLibrary`, 477 carry no
MusicBrainz recording id — and they are Adele, Stromae, James Blake, Stephan Bodzin and Burial. The DJ crate is the same
story from a worse start: zero of 592 tagged, and MusicBrainz has about 62% of them.

| step               | what it does                                                                      |
| ------------------ | --------------------------------------------------------------------------------- |
| `identify fetch`   | ask MusicBrainz about every file with no recording id. Raw responses cached (#58) |
| `identify propose` | cache + tags → jsonl, each row graded `matched` / `probable` / `uncertain`        |

```sh
./.venv/bin/python identify.py fetch ~/MusicLibrary
./.venv/bin/python identify.py propose ~/MusicLibrary
jq -r 'select(.confidence=="uncertain")' out/identify-musiclibrary.jsonl
```

**The query has to be normalised or it reports a gap that is not there.** `artist:` does not match a joined credit, so
`"Stephan Bodzin & Marc Romboy"` scores zero where `"Stephan Bodzin"` scores 100; and `(Original Mix)` is Beatport's
word for "not a remix", not a MusicBrainz convention. Fixing both took a sample of the crate from 3/30 to 19/30. The
suffix strip is narrow on purpose — `(Nolah Remix)` names a different recording and must survive.

**Duration is the second opinion.** A name match can be wrong: the first run turned up a "Vienna Calling" matching by
name at 100 and differing by 172 seconds, which is a different mix. A candidate is only `matched` when its length agrees
within five seconds.

## genre/ — normalise genre tags (#52)

The convention is compound, `; `-separated, **broad term first**, so `genre="*electronic*"` finds every electronic
record. Half the library was bare single words and a fifth was a Discogs slash-export.

| step                      | what it does                                                                                                                         |
| ------------------------- | ------------------------------------------------------------------------------------------------------------------------------------ |
| `genre.fetch`             | MusicBrainz release → its Discogs link → genres and styles. Cached in `genre-cache.sqlite`; re-running fetches only what is missing. |
| `genre.propose`           | cache + current tags → `review.tsv`. Writes no tags.                                                                                 |
| **you edit `review.tsv`** | change `proposed`, or clear `apply` to skip a row                                                                                    |
| `genre.apply`             | `review.tsv` → tags. **Dry run unless given `--write`.**                                                                             |
| `genre.hierarchy`         | the curated taxonomy → `genre-hierarchy.jsonl`, for `gmuse shelf apply`. **Dry run unless given `--write`.**                         |
| `genre.snapshot`          | every file's genre _before_ a change, so the apply can be undone                                                                     |

`apply` edits in place and keeps no record of what was there, so take a snapshot first and keep it until you are happy:

```sh
./.venv/bin/python -m genre.snapshot ~/MusicLibrary
./.venv/bin/python -m genre.apply ~/MusicLibrary --write
# and if it was wrong:
./.venv/bin/python -m genre.snapshot ~/MusicLibrary --restore --write
```

MusicBrainz is the _bridge_, not the source: it had genres for 3 of 40 releases, but 33 of them carry a Discogs link.

### Reading genre-review.tsv

**Only albums that would change.** The survey covers all 504; 396 already match and asking you to scroll past them to
find the 108 that don't is not a review. `--all` writes the full survey if you want it.

| column         | what it is                                                             |
| -------------- | ---------------------------------------------------------------------- |
| `current`      | what the files say now — `\|`-separated if the album's tracks disagree |
| `proposed`     | what `apply` would write                                               |
| `source`       | which of the three paths below decided it                              |
| `release_mbid` | the **release** id from the file's `MusicBrainz Album Id` tag          |
| `apply`        | `y` to write it; clear it to skip that row                             |

`release_mbid` is a _release_ — one pressing of one album. Not the artist, not the track, and **not the release group**,
which is a different id and the one genres actually hang off. Conflating those two is the bug this pipeline already made
once: querying the release found almost no genres, and the group had them all along.

To override a proposal, edit `proposed`. To reject one, clear `apply`.

### `genre hierarchy` — the taxonomy stops being a string

The `Broad; Style` convention exists because a view could not name a level of a vocabulary. gmuse can now
(`:view levels genre@1,genre@2`, `genre*="Electronic"`), so the edges belong in the library rather than in the tag:

```sh
uv run gmuse-tools genre hierarchy ~/MusicLibrary          # dry run, writes the review file
uv run gmuse-tools genre hierarchy ~/MusicLibrary --write  # writes genre-hierarchy.jsonl
gmuse shelf apply tools/out/genre-hierarchy.jsonl --write
```

`hierarchy.NARROWER` is the curation and `propose.IMPLIES` is **derived from it**, so the broad term a tag string gets
and the term the model climbs to cannot drift apart. A child may have several parents — `Bossa Nova` is Jazz and Latin —
and the first listed is the one the tag gets, because a `; ` value has room for one.

Where curation has not reached, Discogs' own split decides: it separates fourteen _genres_ from hundreds of _styles_, so
a release naming exactly one genre teaches an edge for every style on it. Releases naming two teach nothing, because
which style belongs to which genre is exactly what they do not say.

**What is left goes to `genre-hierarchy-review.tsv` rather than being guessed.** On this library that is eleven terms,
and every one is a real judgement: `Gospel` is Folk and Blues and Funk depending on the record, `Lo-fi` is a production
aesthetic rather than a place in a taxonomy, and `Tom Waits` is an artist name somebody typed into a genre field.

Two rules learned by getting them wrong:

- **No shortcut past a parent that already reaches the top.** `Metal` is under `Rock`; also naming `Rock` on
  `Stoner Rock` made it one hop from the top, and `genre@1` then reported `Metal` as a broad term.
- **The log is append-only, so a revision is not a replacement.** A pass that moves an edge has to withdraw the old one
  or both stand. `hierarchy` diffs against its own last output and emits the retractions.

### `Folk, World, & Country` is one genre, not three

Discogs' 15 top-level Genres include a name with two commas in it. Joined with the convention's `; ` it reads as three
terms and invites `& Country` to be taken for a genre code:

```
Blues; Folk, World, & Country; Country; Folk      ← four terms, reads as six
Blues; [Folk, World, & Country]; Country; Folk    ← what the candidate column shows
```

No such term ever reaches `proposed` — `RENAME` maps the whole string to `Folk`. But `candidate` in
`genre-uncertain.tsv` is raw evidence from the source, so it keeps the source's spelling and brackets the boundary.

Brackets rather than quotes, because the field is written by `csv`, which escapes a quote by doubling it: quoting the
term wrapped it in three quote characters on each side, which was harder to read than the problem it fixed.

### Where the metadata comes from

Three paths, in order, and the third is the only one that guesses:

1. **The file's MusicBrainz id → its Discogs link.** An identification: nothing is matched by name.
2. **The MusicBrainz release _group_.** Genres live there, not on the release — a release is a pressing, the group is
   the album. Querying only the release made MusicBrainz look empty (3 of 49); it is 130 of 429.
3. **A Discogs search by name, verified by tracklist.** Only an `exact` match — same count, same normalised titles — is
   used unattended. Anything else lands in `uncertain.tsv` and is **not** proposed.

That third rule is not caution for its own sake. The top hit for Stephan Bodzin's _Boavista_ is a **2-track single** of
the same name, and _Adele — 21_ matches the 11-track standard against the 15-track deluxe this library holds.

A **503 is not an answer**: MusicBrainz throttles with it, and treating every non-200 as "no data" cached records that
have genres as having none. A 404 is final; a 503 waits and asks again.

### Why TSV and not CSV

The fields are full of commas and free of tabs. 23 of 504 rows carry one inside a field — `Basscad, EP`,
`Hardcore Devo, Vol. 2: 1974-1977` — and Discogs' own vocabulary has `Folk, World, & Country` as a **single term**. CSV
quotes all of those; TSV leaves the file readable as plain text, which matters because `uncertain.tsv` is meant to be
read by a person, and it keeps `cut -f2` and `awk -F'\t'` correct rather than plausible.

TSV's cost is that it has no escape mechanism, so `write_tsv` refuses a row instead of emitting one that cannot be read
back. That is the boundary check §12 asks for: the failure downstream would be a _dropped_ row, not a wrong one —
`apply.py` gates on a column that must read `y`, and no column shift puts a genre onto an album it doesn't belong to —
but a row that vanishes silently is worth three lines to prevent.

### Rules the review taught us

Each of these was a bug that destroyed good data, caught by reading the proposal rather than by a test:

- **Union with the existing tags, never replace them.** A source knows things the tags do not; it does not know better
  than them.
- **Do not reorder a `; ` value.** The slash form is machine output sorted alphabetically, so its order means nothing;
  the `; ` form is a human's judgement. Same operation, opposite verdict, decided by provenance.
- **Compilations are per track, and are identified by `albumartist` alone.** Unioning a `Various Artists` album replaces
  several correct genres with one wrong shared one — but testing "do the `artist` tags vary" instead catches a _guest
  credit_, which silently exempted six real albums until the outcome was checked.
- **No cap on the number of terms.** The filter matches substrings, so more terms strictly improve recall; length is a
  rendering problem `{genre:.40}` already solves.

## credits.py — one spelling for multi-artist credits (#60)

The problem is not missing data, it is data written four different ways. This library carries **four spellings of the
MusicBrainz artist id field at once** — `TXXX:MusicBrainz Artist Id`, `TXXX:musicbrainz_artistid`, the Vorbis
`musicbrainz_artistid`, the iTunes freeform atom — and they disagree _within a single file_. `ARTISTS` has three
spellings and two shapes: a real multi-value list in Vorbis, a `/`-joined single string in ID3.

```sh
uv run python -m credits scan  ~/MusicLibrary          # -> out/credits-review.tsv, writes no tags
uv run python -m credits apply ~/MusicLibrary          # dry run
uv run python -m credits apply ~/MusicLibrary --write  # snapshots first
uv run python -m credits restore ~/MusicLibrary --write
```

### Why `/` cannot simply be split

`AC/DC` is one artist and this library owns **107 of its tracks**. Splitting on the separator would invent a band called
DC.

The arbiter is the MusicBrainz id count: two names and two ids agree, so the slash is a separator; two names and one id
do not, so it is a character. Where they disagree the row is **not** proposed — it goes to the review file with its
evidence, on the discipline the genre pipeline uses.

Columns are ordered for reading: `apply`, `verdict`, `note`, `artists`, `album`, `id_conflict`, `path`. **`path` is last
on purpose** — it is the longest and least scannable field, and putting it early pushes the reason a row exists off the
right-hand edge of a wide window, which is how a populated `note` came to look like an empty one.

| verdict     | meaning                                | proposed?       |
| ----------- | -------------------------------------- | --------------- |
| `list`      | a real multi-value list already exists | nothing to do   |
| `split`     | slash-joined, and the id count agrees  | **yes**         |
| `ambiguous` | slash-joined, id count disagrees       | no — look at it |
| `single`    | one artist                             | nothing to do   |

First run over 6511 files: 188 proposed, 125 ambiguous. The ambiguous set is almost entirely `AC/DC` (107) and
`Roni Size / Reprazent` (13), both of which are **one act** and correctly left alone.

### When the two id spellings disagree

`TXXX:musicbrainz_artistid` and `TXXX:MusicBrainz Artist Id` coexist on some files and do not always agree. The
`id_conflict` column names which kind it is, because they want opposite responses:

| kind        | what it means                                               | what to do                                       |
| ----------- | ----------------------------------------------------------- | ------------------------------------------------ |
| `subset`    | one spelling has the lead artist, the other the full credit | nothing — `decide()` already takes the wider one |
| `different` | the spellings name **different artists**                    | look it up                                       |

`different` is not a tagging style clash, it is **rot**. Every case in this library is a MusicBrainz merge: two artist
entries were combined and only the older spelling still points at the id that was merged away.

```
Beyonce, I Am... Sasha Fierce
  musicbrainz_artistid   183105b5-3e68-4748-9086-2c1c11bf7a3d   -> redirects, merged away
  MusicBrainz Artist Id  859d0860-d480-4efd-970c-c05d5f1776b8   -> canonical
```

Both resolve to the same artist through the API, so nothing is _wrong_ downstream today — but the stale id will silently
stop resolving if MusicBrainz ever drops the redirect. Fixing it needs a lookup per id rather than a rule, so this pass
reports and does not touch them: 6 files, all `different`, plus 5 `subset`.

### ID3v2.3 cannot hold a multi-value frame

Which is why the slash-joining exists at all — it is Picard working around the format, not a mistake. 612 files here are
v2.3; fixing them means rewriting the tag as v2.4, so it takes `--upgrade-id3` rather than happening quietly. 17 of the
188 were skipped for this reason.

### Undo, and how to read the snapshot

`apply --write` snapshots every field it is about to touch, key spelling included, and `restore` puts it back exactly —
`['Billy Bragg/Wilco']` under `TXXX:ARTISTS`, not the canonical spelling it was changed to. This is the condition §1's
2026-09-01 amendment attaches to tag editing.

`out/credits-snapshot.tsv` is that record. It is written for `restore` rather than for reading, but it should still say
what it means, so a row reads left to right as _this field held this, and I made it that_:

| path                                   | was_field      | was_value                 | became                     |
| -------------------------------------- | -------------- | ------------------------- | -------------------------- |
| `Dance/Amon Tobin/…/07 …Precursor.mp3` | `TXXX:ARTISTS` | `Amon Tobin/Quadraceptor` | `Amon Tobin; Quadraceptor` |

Only `was_field` and `was_value` are needed to undo. **`became` is there for you** — without it the file is a list of
strings with no indication of what happened to them, which is a poor thing to hand someone whose undo depends on it.

Note the shapes: `was_value` shows the single `/`-joined string that was there, `became` shows the real multi-value list
that replaced it. That difference _is_ the change this pass makes.

## tags.py — one tag format per file (#53)

Most of this library's mp3s carry **three tag formats at once**:

```
3770  ID3v2.4 + ID3v1 + APEv2      456  Vorbis   (ogg/opus)
1225  ID3v2.4 + ID3v1               29  MP4      (m4a)
 527  ID3v2.3 + ID3v1 + APEv2       13  (none)
 395  ID3v2.4                       22  ID3v2.3
  63  ID3v2.3 + ID3v1
```

Ordinary for a collection with history — each tool added its own rather than replacing what it found. Not free, though:

- **APE cost 66 tracks.** One malformed item and `lofty` abandons the whole file, ID3 and audio included (#53).
  Verified: a file that read `failed to parse Mpeg file` reads `Beirut — Carousels` once the APE tag is gone.
- **The APE ReplayGain is stale.** 4218 files disagree with rsgain's ID3 values, and `MP3GAIN_UNDO` is non-zero on all
  of them — mp3gain _altered the audio_, so the APE numbers describe bytes that no longer exist. gmuse reads ID3 and is
  unaffected; a tool preferring APE would play them at the wrong level.
- **612 files are ID3v2.3**, which cannot hold a multi-value frame — the reason `credits apply` skipped 17.

```sh
uv run python -m tags survey  ~/MusicLibrary          # -> out/tags-review.tsv
uv run python -m tags apply   ~/MusicLibrary          # dry run
uv run python -m tags apply   ~/MusicLibrary --write  # snapshots first
uv run python -m tags restore ~/MusicLibrary --write
```

### Nothing is lost, and that was checked rather than assumed

Of **1503** APE metadata fields, every one already exists in ID3 — and where the two disagree the ID3 value is the newer
one, because the genre pass wrote it:

```
Genre   APE='Rock'   ID3='Rock; Pop; Pop Rock; Art Rock'
Year    APE='1980'   ID3='1980-09-08'
```

**Lyrics are the exception.** 629 files carry them in APE; 328 also have an ID3 `USLT` frame, but **301 have them only
in APE**. Those are migrated into ID3 _before_ the APE tag is deleted, and saved first — a crash between the two should
lose the deletion, never the words.

| step                     | files |
| ------------------------ | ----: |
| APEv2 removed            |  4297 |
| ID3v1 removed            |  5585 |
| ID3v2.3 → v2.4           |   612 |
| lyrics migrated into ID3 |   301 |
| already modern and alone |   893 |

`restore` puts the APE tags back from the snapshot — content, not bytes, so a restored file is equivalent rather than
identical. The audio is never touched.

## quality.py — what the audio actually is, not what the header claims

A file transcoded from 128 kbps and re-encoded at 320 still _says_ 320. The tags cannot tell you, and the LAME header
records the last encode rather than the provenance. The **lowpass cutoff** can: every lossy encoder discards everything
above a frequency chosen by bitrate, and re-encoding cannot put back what was thrown away.

```sh
uv run python -m quality survey ~/MusicLibrary          # -> out/quality-review.tsv
uv run python -m quality survey ~/MusicLibrary --tier 320
```

### It looks for the wall, not the edge

The first version took the highest frequency still within 45 dB of the 1-5 kHz peak. That measures _level_, so it
measures the music: a track with a strong midrange and a soft top end reported a 3 kHz "cutoff", and **57% of the
library came back unjudgeable** with nothing wrong with any of it.

An encoder's lowpass is a **shape** — a near-vertical fall of 30-60 dB inside a kilohertz. So the detector walks the
spectrum above 8 kHz in half-kilohertz steps and takes the steepest single drop. Content-independent: quiet music has a
gentle slope wherever it ends, and a brick wall is a brick wall whatever is playing.

An even earlier attempt using ffmpeg's band filters ranked 96 kbps _above_ 128 and was abandoned. The FFT is not
gold-plating; the cheap versions do not work.

### What it found here

| verdict         |    files |
| --------------- | -------: |
| ok              |     5024 |
| **TRANSCODE?**  | **1040** |
| no sharp cutoff |      373 |
| unmeasured      |       63 |

Of the 1040, **441 declare 320 kbps** and cut off between 11.6 and 19 kHz. Another 413 declare 192.

### The lo-fi question, and why the answer is still "transcode"

The worst-hit albums are Ween's _GodWeenSatan_ and _The Pod_, Beck's _Stereopathetic Soulmanure_, _The Basement Tapes_,
the White Stripes' debut — all famously recorded to cassette. The obvious worry is that the detector is flagging tape
rolloff.

It is not. Ween, _Strap on That Jammy Pac_:

```
10 kHz   -15.8 dB       16 kHz   -75.8 dB
13 kHz   -39.8 dB       17 kHz  -109.1 dB     <- 33 dB in 1 kHz
```

The gradual fall from 10 to 13 kHz **is** the cassette. The cliff at 16 kHz is not — nothing analogue drops that fast.
Both are present, and only the second is an encoder. Median cliff strength is 57 dB for the lo-fi albums against 54 dB
for everything else, so they are not a softer case; they are old low-bitrate rips that were later re-encoded.

### albums — the list to buy again

```sh
uv run python -m quality albums --limit 100     # -> out/quality-albums.tsv
```

Rolls the per-file findings up, ranked by `share x deficit`. Both halves matter: one bad track among twelve is a bad
track, but an album where _every_ track cuts at 16 kHz is a bad **source**, and that is the one worth money. **52 of the
worst 100 here are flagged end to end.**

The deficit is measured against clean audio, not against the declaration. Ranking by how much a file _lied_ put a
mislabelled 320 above a genuine 64 kbps rip, which is backwards for a list about what to re-buy — the 64 kbps album
sounds worse. `declared` stays in the output, so the lie is still visible.

```
 12.0  10/10 100%   64k decl    8.5 kHz  Emmylou Harris/Luxury Liner
 10.9  10/10 100%  192k decl    9.6 kHz  Bob Dylan/The Times They Are A-Changin'
  6.7  15/15 100%  128k decl   13.8 kHz  Beck/Guero
  5.0  10/10 100%  320k decl   15.5 kHz  Fiona Apple/Tidal
```

`--under` filters on what the audio _sounds like_, not on what it claims — an album declaring 320 whose cutoff says 128
is under 190 by the only measure that matters here:

```sh
uv run python -m quality albums --under 190 --limit 500
```

104 albums here sound like they came from under 190k: 15 at 96, 5 at 112, 68 at 128, 16 at 160.

Album totals come from the directory rather than from the review file, which lists only what was flagged — dividing
flagged by flagged would score every album 100%.

### Verdicts

| verdict           | meaning                                                             |
| ----------------- | ------------------------------------------------------------------- |
| `ok`              | cutoff is where the declared bitrate would put it                   |
| `TRANSCODE?`      | a sharp wall well below the declared bitrate's lowpass              |
| `no sharp cutoff` | no wall found — a fact about the recording, not a charge against it |
| `unmeasured`      | too short or silent in the sampled window                           |

The question mark is meant. A wall this steep can only be an encoder, but _which_ encoder and when is not something a
spectrum can say — only that the audio does not contain what the header promises.

Needs `numpy` and `ffmpeg`.

## audit_library.py — tags, naming, ReplayGain, NFC (#49)

```sh
./.venv/bin/python audit_library.py ~/MusicLibrary/Dance
```

Read ffprobe's **`stream_tags` as well as `format_tags`** — Vorbis comments live on the stream, and reading only the
latter reports every Opus file as completely untagged. That looked like a real finding for a while.

## audit_dj.py — do filenames agree with tags? (#51)

For the DJ crate, whose names are `BPM - KEY - Artist - Title`. 41% of them disagree with their own tags about the key.
Run it against a local copy; over SMB it takes minutes.

## out/ — everything generated

Review tables, snapshots and the API cache are written to `tools/out/`, never beside the code, and each name is prefixed
by the pass that owns it:

```
out/genre-review.tsv        propose -> you edit this -> apply
out/genre-uncertain.tsv     matched a release whose tracklist differs; not proposed
out/genre-snapshot.tsv      every genre before an apply, for --restore
out/genre-cache.sqlite      raw API responses, so a re-run costs no requests
out/credits-review.tsv      artist splits: proposed, ambiguous, and why
out/credits-snapshot.tsv    every artist field before an apply, for restore
```

The whole folder is gitignored. Nothing in it is a source file, and nothing in it is precious — every one is regenerable
from the cache or the library.

## Credentials

`common.secret()` reads, in order: the environment, the macOS Keychain, then a `.env` beside the scripts (gitignored,
and last because it is the one that gets committed by accident).

```sh
security add-generic-password -a "$USER" -s gmuse-discogs -w
```

Omitting `-w VALUE` prompts, so the token never enters shell history.
