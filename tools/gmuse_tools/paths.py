"""What a path says, and whether anything else already says it — issue #143.

A path is the last place in this library where a fact can hide. Directories
carry era, genre and tempo band; filenames carry BPM, key, artist and a
mastering marker; one file has its series in its name and nowhere else. Once a
file is imported the path becomes a rendering the library could regenerate, so
whatever it knew must be **consumed** first — used, or agreed with, or reported.

Storage is going to be re-rendered (`storage-layout.md`), and a re-render is the
last moment the old paths exist.

## Three outcomes, and no fourth

- **agrees** — the tags already say it. Nothing to do, and this is 99% of it.
- **only in path** — nothing else records it. A proposal, for review.
- **differs** — two sources disagree. Reported. Never resolved by rule: "the tag
  wins" would throw away 9 BPMs and 4 genres somebody typed on purpose, and
  "the path wins" is worse.

## Read once, at import, and never as authority

`shelf unfiled` already treats a folder this way — evidence a person left, read
once and proposed. A pass that *filed* by folder would make the filesystem
authoritative again, which #90 and #94 took away deliberately.

## Order

**Before `tags normalise`.** The truncation check below needs the ID3v1 block
that `tags` exists to strip.

    uv run python -m gmuse_tools.cli paths survey ~/MusicLibrary   # -> out/paths-review.tsv
"""

from __future__ import annotations

import collections
import os
import re
import unicodedata
from pathlib import Path
from typing import TYPE_CHECKING, Final, NamedTuple

import typer

from gmuse_tools.genre.common import out_path, tags_of, walk, write_tsv

if TYPE_CHECKING:
    from collections.abc import Iterator

#: ID3v1 fields are exactly this many bytes, and a value of exactly this length
#: is the fingerprint of one — #143. `Acoustica: Alarm Will Sound Performs Aphex
#: Twin` is stored as `Acoustica: Alarm Will Sound Pe`, and on these files the
#: **folder** holds the full name while the tag holds a stump.
ID3V1_FIELD: Final = 30
ID3V1_LEN: Final = 128

#: A beat of slack when comparing tempos, and two when asking whether one is
#: the other doubled — a filename holds a rounded value and a tag an analyser's.
BEAT: Final = 1.0
DOUBLED: Final = 2.0
#: Positions on the circle of fifths.
WHEEL: Final = 12

#: Named patterns, opt-in, rather than a general filename parser. A parser that
#: guesses wrongly manufactures facts, and these are the only two shapes this
#: library actually uses.
PATTERNS: Final = {
    # `132 - C#m - Christopher Just - I'm A Disco Dancer (Radio Edit)`
    "dj": re.compile(r"^(?P<bpm>\d{2,3}) - (?P<key>[A-G][#b]?m?|\d{1,2}[mdAB]) - (?P<artist>.+?) - (?P<title>.+)$"),
    # `Early James - Strange Time To Be Alive - 03 My Sweet Camelia`
    "artist-title": re.compile(r"^(?P<artist>[^-]+?) - (?P<title>.+)$"),
}

#: A directory whose name is one of these is naming a genre, which the tags
#: almost always also say — 493 of 497 agree. Kept as a closed list because a
#: folder called `Trance` is evidence and a folder called `Alex` is not.
GENRE_DIRS: Final = frozenset(
    {
        "ambient",
        "breakbeat",
        "deep house",
        "downtempo",
        "drum & bass",
        "dub",
        "dubstep",
        "electro house",
        "electronica",
        "house",
        "idm",
        "jazz",
        "minimal",
        "progressive house",
        "psytrance",
        "tech house",
        "techno",
        "trance",
        "trip hop",
        "uk garage",
    }
)

#: Marks a file a mastering tool rewrote. Nothing in the tags says it, so it is
#: the one thing on these paths that is genuinely path-only — 144 files.
PROCESSED: Final = "_pn"


class Finding(NamedTuple):
    """One thing a path said, and what became of it."""

    path: Path
    what: str
    from_path: str
    from_tags: str
    verdict: str


def norm(text: str) -> str:
    """Comparable form: NFC, casefolded, and free of runs of spaces."""
    return re.sub(r"\s+", " ", unicodedata.normalize("NFC", text)).strip().casefold()


def loose(text: str) -> str:
    """The same, with punctuation and spacing thrown away.

    **Comparing spellings finds differences that are not there.**
    `Sleepless(Stephan Bodzin Remix)` and `Sleepless (Stephan Bodzin Remix)`
    are one title, and reporting them as a conflict buries the ones that are.
    """
    return re.sub(r"[^\w]+", "", norm(text))


def same_bpm(a: str, b: str) -> bool | None:
    """Whether two tempos are the same. None when one is not a number.

    **Compared as numbers, not as text.** `119` and `119.00` are one tempo, and
    so are `126` and `125.98`; a string comparison called 115 of 503 files a
    conflict when 9 of them were. A whole beat of slack, because a filename
    holds a rounded value and a tag holds an analyser's.
    """
    try:
        x, y = float(a), float(b)
    except ValueError:
        return None
    return abs(x - y) < BEAT


def half_time(a: str, b: str) -> bool:
    """Whether one tempo is the other halved or doubled — a known detector bug."""
    try:
        x, y = float(a), float(b)
    except ValueError:
        return False
    return bool(x and y) and (abs(x - 2 * y) < DOUBLED or abs(y - 2 * x) < DOUBLED)


#: The twelve major keys in Camelot order, 1B first, then the minors from 1A.
#: Mirrors `src/model/key.rs`, which is the authority; this is here so a tool
#: can compare `C#m`, `8A` and `4m` without asking gmuse.
_MAJOR: Final = ["B", "F#", "Db", "Ab", "Eb", "Bb", "F", "C", "G", "D", "A", "E"]
_MINOR: Final = ["Abm", "Ebm", "Bbm", "Fm", "Cm", "Gm", "Dm", "Am", "Em", "Bm", "F#m", "C#m"]
_PITCH: Final = {"C": 0, "D": 2, "E": 4, "F": 5, "G": 7, "A": 9, "B": 11}


def key_position(text: str) -> tuple[int, bool] | None:
    """A key as (wheel position 1-12, is_minor), from any of three notations.

    Musical (`Am`, `Cmin`), Camelot (`8A`) or open key (`4m`). One key spelled
    three ways is one key, and this library's files carry all three — a string
    comparison called 184 of 503 a conflict.
    """
    s = text.strip()
    if not s:
        return None
    if s[0].isdigit():
        return _wheel_position(s)

    found = re.match(r"^([A-Ga-g])([#b♯♭]?)\s*(.*)$", s)
    if not found:
        return None
    pitch = _PITCH[found.group(1).upper()] + {"#": 1, "♯": 1, "b": -1, "♭": -1}.get(found.group(2), 0)
    word = found.group(3).strip(" -_").casefold()
    if word in {"", "maj", "major", "d", "dur"}:
        minor = False
    elif word in {"m", "min", "minor", "moll"}:
        minor = True
    else:
        return None
    anchor = 9 if minor else 0
    return (8 - 1 + 7 * (pitch % WHEEL - anchor)) % WHEEL + 1, minor


def _wheel_position(s: str) -> tuple[int, bool] | None:
    """Camelot (`8A`) or open key (`4m`), which are the same wheel turned."""
    found = re.match(r"^(\d{1,2})\s*([ABmd])$", s)
    if not found:
        return None
    n, ring = int(found.group(1)), found.group(2)
    if not 1 <= n <= WHEEL:
        return None
    if ring in "AB":
        return n, ring == "A"
    return (n + 6) % WHEEL + 1, ring == "m"


def same_key(a: str, b: str) -> bool | None:
    """Whether two spellings name one key. None when either is unreadable."""
    x, y = key_position(a), key_position(b)
    if x is None or y is None:
        return None
    return x == y


def has_id3v1(path: Path) -> bool:
    """Whether a 128-byte ID3v1 block sits at the very end."""
    try:
        with path.open("rb") as handle:
            handle.seek(-ID3V1_LEN, os.SEEK_END)
            return handle.read(3) == b"TAG"
    except OSError:
        return False


def covers(a: str, b: str) -> bool:
    """Whether one of these says everything the other does, and maybe more.

    **A subset is not a conflict.** A DJ filename names the lead artist where
    the tag names all four, and drops the `(Original Mix)` the tag carries.
    Neither contradicts the other, and reporting 276 of them as disagreements
    buries the ones that are.
    """
    x, y = loose(a), loose(b)
    return bool(x) and bool(y) and (x in y or y in x)


def verdict(from_path: str, from_tags: str, *, same: bool | None = None) -> str:
    """Which of the outcomes this is.

    `same` lets a caller answer by *value* — a tempo or a key — where comparing
    the spelling would invent a conflict.
    """
    if not from_tags:
        return "only in path"
    if same is None:
        same = covers(from_path, from_tags)
    return "agrees" if same else "differs"


def truncation(path: Path, tags: dict[str, str]) -> Iterator[Finding]:
    """A tag of exactly 30 characters, which is an ID3v1 field's width — #143.

    The strong signal is the length **and** an ID3v1 block present, which is why
    this must run before `tags normalise` strips them. Where the folder name
    continues the tag the truncation is confirmed rather than suspected, and the
    folder is the better record.
    """
    v1 = has_id3v1(path)
    for field, against in (("album", path.parent.name), ("artist", path.parent.parent.name)):
        value = tags.get(field if field == "album" else "album_artist", "") or tags.get(field, "")
        if len(value) != ID3V1_FIELD:
            continue
        confirmed = len(against) > ID3V1_FIELD and norm(against).startswith(norm(value)[:25])
        # **Confirmed is when the folder continues the tag.** Without that all
        # this has is a length, and some albums really are 30 characters — so
        # the rest are `suspect`, which is a quieter thing to be.
        yield Finding(
            path,
            f"{field} truncated at 30 chars" + (" (ID3v1 present)" if v1 else ""),
            against if confirmed else "",
            value,
            "only in path" if confirmed else "suspect",
        )


def from_filename(path: Path, tags: dict[str, str]) -> Iterator[Finding]:
    """What the file's own name claims, by the named patterns only."""
    stem = path.stem
    if stem.endswith(PROCESSED):
        stem = stem[: -len(PROCESSED)]
        yield Finding(path, "processed by a mastering tool", PROCESSED, "", "only in path")

    for name, pattern in PATTERNS.items():
        found = pattern.match(stem)
        if not found:
            continue
        got = found.groupdict()
        # **A DJ name whose key this build does not know still is not an
        # artist.** `117 - Cm7 - …` falls through the dj pattern and the looser
        # one then reads `117` as the artist. A number or a key is neither.
        if name == "artist-title" and (got["artist"].strip().isdigit() or key_position(got["artist"]) is not None):
            break
        if "bpm" in got:
            tag_bpm, tag_key = tags.get("tbpm", ""), tags.get("tkey", "")
            what = "bpm (half or double time)" if half_time(got["bpm"], tag_bpm) else "bpm"
            yield Finding(
                path, what, got["bpm"], tag_bpm, verdict(got["bpm"], tag_bpm, same=same_bpm(got["bpm"], tag_bpm))
            )
            yield Finding(
                path, "key", got["key"], tag_key, verdict(got["key"], tag_key, same=same_key(got["key"], tag_key))
            )
        yield Finding(
            path,
            f"artist ({name})",
            got["artist"],
            tags.get("artist", ""),
            verdict(got["artist"], tags.get("artist", "")),
        )
        yield Finding(
            path, f"title ({name})", got["title"], tags.get("title", ""), verdict(got["title"], tags.get("title", ""))
        )
        break


def from_folders(path: Path, tags: dict[str, str]) -> Iterator[Finding]:
    """What the directories above a file claim."""
    for part in path.parts[:-1]:
        if norm(part) in GENRE_DIRS:
            yield Finding(path, "genre (folder)", part, tags.get("genre", ""), verdict(part, tags.get("genre", "")))


def findings(path: Path) -> list[Finding]:
    """Everything this path says, and whether anything else says it too."""
    tags = tags_of(path)
    if not tags:
        return [Finding(path, "unreadable", "", "", "differs")]
    return [*truncation(path, tags), *from_filename(path, tags), *from_folders(path, tags)]


def survey(root: Path, out: Path) -> None:
    """Report what the paths under `root` know. Writes nothing but the report."""
    rows: list[dict[str, object]] = []
    tally: collections.Counter[str] = collections.Counter()
    for path in walk(root):
        tally["files"] += 1
        for found in findings(path):
            tally[f"{found.verdict}: {found.what}"] += 1
            if found.verdict == "agrees":
                continue
            rows.append(
                {
                    "verdict": found.verdict,
                    "what": found.what,
                    "from_path": found.from_path,
                    "from_tags": found.from_tags,
                    "path": str(found.path),
                }
            )

    write_tsv(out, sorted(rows, key=lambda r: (str(r["verdict"]), str(r["what"]), str(r["path"]))))
    for key, count in sorted(tally.items()):
        print(f"  {count:6d}  {key}")
    print(f"\n{len(rows)} row(s) needing a person -> {out}")
    print("`agrees` rows are consumed and not listed.")


LIB = typer.Argument(Path("~/MusicLibrary"), help="library root")


def register(app: typer.Typer) -> None:
    """Attach the path-evidence pass."""
    paths_app = typer.Typer(no_args_is_help=True, help=__doc__)
    app.add_typer(paths_app, name="paths")

    @paths_app.command("survey")
    def survey_cmd(root: Path = LIB) -> None:
        """Report what the paths say. Run this **before** `tags normalise`."""
        survey(root.expanduser(), out_path("paths-review.tsv"))
