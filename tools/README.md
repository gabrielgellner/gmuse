# tools

Out-of-tree helpers for the music library. **None of this is gmuse.**

§1 puts tag editing out of scope, and the pattern is the one rsgain and bliss already set: an external tool writes,
gmuse reads. Reading new files into the library is likewise a CLI job rather than something the app does. Python because
these are I/O-bound, edited between runs, and reviewed by a human — and because keeping them out of `Cargo.toml` is what
keeps the boundary real.

Checked the same way sdmxlib is: `ruff` on `select = ["ALL"]` and `pyrefly` on `preset = "all"`, subtract with a written
reason, **no suppressions**. Both are at zero findings. The full-preset pyrefly run found five defects here and every
one was real — a `str | None` leaking `Any` out of a `.get()` chain, two `list()` calls on unchecked JSON, an empty
container with no inferable type, and three imports that resolved only because Python puts a script's own directory on
`sys.path`. All fixed in code.

```sh
cd tools
./.venv/bin/python -m genre.propose ~/MusicLibrary   # genre/ is a package
./.venv/bin/python audit_library.py ~/MusicLibrary
ruff check . && ruff format --check . && pyrefly check
```

## genre/ — normalise genre tags (#52)

The convention is compound, `; `-separated, **broad term first**, so `genre="*electronic*"` finds every electronic
record. Half the library was bare single words and a fifth was a Discogs slash-export.

| step                      | what it does                                                                                                                         |
| ------------------------- | ------------------------------------------------------------------------------------------------------------------------------------ |
| `genre.fetch`             | MusicBrainz release → its Discogs link → genres and styles. Cached in `genre-cache.sqlite`; re-running fetches only what is missing. |
| `genre.propose`           | cache + current tags → `review.tsv`. Writes no tags.                                                                                 |
| **you edit `review.tsv`** | change `proposed`, or clear `apply` to skip a row                                                                                    |
| `genre.apply`             | `review.tsv` → tags. **Dry run unless given `--write`.**                                                                             |
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
