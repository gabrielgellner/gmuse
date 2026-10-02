#!/usr/bin/env python3
"""One tag format per file: modern ID3v2.4, and nothing else clinging to it.

Most of this library's mp3s carry **three tag formats at once** — ID3v2, a
128-byte ID3v1 block, and an APEv2 tag appended by mp3gain. That is ordinary
for a collection with history: each tool added its own rather than replacing
what it found. It is also not free.

**APE cost 66 tracks.** A single malformed item in one APE tag makes `lofty`
abandon the whole file — the good ID3 tag beside it, and the audio, which plays
perfectly (gmuse issue #53). Five files did that here, and nothing on screen
said so.

**The APE ReplayGain is stale and wrong.** 4218 files disagree with what rsgain
later wrote to ID3, and `MP3GAIN_UNDO` is non-zero on every one — mp3gain
*altered the audio* by rewriting the MPEG global gain. So the APE numbers
describe bytes that no longer exist. gmuse reads ID3 and is unaffected, but any
tool preferring APE would play 4218 tracks at the wrong level.

**612 files are ID3v2.3, which cannot hold a multi-value frame at all.** That is
why Picard joins artists with `/` there, and why `credits apply` had to skip 17
of them.

What is removed is either duplicated in ID3 or provably stale — checked, not
assumed: of 1503 APE metadata fields, **every one already exists in ID3**, and
where the two disagree the ID3 value is the newer one.

The exception is lyrics, which is why this pass migrates before it deletes.

**A second ID3v2 tag can sit behind the first, and 39% of the staging tree has
one** (#303). A tag declares its own length, so another begins where it ends,
and mutagen's `ID3(path)` reads the leading one and stops — which is why these
files were counted as "already modern and alone". They are not merely untidy:
on the Dido albums the two tags named *different* MusicBrainz recordings, so
which identity the file had depended on which tag a reader preferred. Found by
`id3_chain`, compared by `id3_conflicts`, and cut by `strip_extra_id3_bytes`.
Where the two disagree the row is reported and **not** marked to apply, because
which tag is right is a judgement and not always the first one.

**A malformed APE tag is removed by position** (#144). `ape_of` refuses one,
which is right for reading and was wrong for removing: 64 blocks survived a
full pass because the tool counted them as absent, and every one of them was
unparseable — precisely the files this module exists for. `ape_span` finds the
block by its own footer and `strip_ape_bytes` cuts it off, checking the audio
is untouched afterwards.

    uv run python -m tags survey  ~/MusicLibrary          # -> out/tags-review.tsv
    uv run python -m tags apply   ~/MusicLibrary          # dry run
    uv run python -m tags apply   ~/MusicLibrary --write  # snapshots first
    uv run python -m tags restore ~/MusicLibrary --write
"""

from __future__ import annotations

import collections
import csv
import io
import json
import re
from collections.abc import Iterable
from pathlib import Path
from typing import Final, NamedTuple

import typer
from mutagen import MutagenError
from mutagen.apev2 import APENoHeaderError, APEv2
from mutagen.id3 import ID3, USLT, ID3NoHeaderError
from mutagen.mp3 import MP3

from gmuse_tools.genre.common import out_path, walk, write_tsv
from gmuse_tools.paths import STUMP_LENGTHS

#: APE keys that are mp3gain's own bookkeeping. All superseded by rsgain's ID3
#: frames, and `MP3GAIN_UNDO` is the record of an audio change made years ago
#: that nobody intends to reverse.
GAIN_KEYS: Final = frozenset(
    {
        "mp3gain_minmax",
        "mp3gain_album_minmax",
        "mp3gain_undo",
        "replaygain_track_gain",
        "replaygain_track_peak",
        "replaygain_album_gain",
        "replaygain_album_peak",
    }
)

TRUTHY: Final = frozenset({"y", "yes", "1"})

#: Review column -> the line the survey prints for it. A table rather than a
#: chain of `if`s so adding a column cannot quietly go uncounted.
TALLY_LINES: Final = {
    "upgrade": "ID3v2.3 -> v2.4",
    "drop_ape": "APEv2 to remove",
    "drop_id3v1": "ID3v1 to remove",
    "drop_extra_id3": "stacked ID3v2 to remove",
    "carry_id3": "frames to carry out of a stacked tag",
    "restore_id3": "ID3v1 stumps to restore from a stacked tag",
    "id3_differs": "stacked tags differ on content — the leading one is kept",
    "id3_conflict": "stacked tags disagree — left for a person",
    "salvage_lyrics": "lyrics to migrate into ID3",
}
MODERN: Final = (2, 4, 0)

#: What an APEv2 tag looks like from the outside — #144. Found this way it can
#: be removed without being understood.
#:
#: **Scanned rather than found at a fixed offset**, because the end of an mp3
#: is a queue: mp3gain's APE, a 128-byte ID3v1 block, and on this library a
#: Lyrics3v2 tag too, in whatever order the tools that wrote them chose. The
#: first attempt looked only at the last 32 and 160 bytes and found nothing on
#: files whose APE header sat 374 bytes from the end.
APE_MAGIC: Final = b"APETAGEX"
FOOTER_LEN: Final = 32
APE_VERSIONS: Final = frozenset({1000, 2000})
TAIL_SCAN: Final = 64 * 1024

#: What an ID3v2 tag looks like from the outside — #303, and the same reading
#: as `APE_MAGIC` above for the same reason.
#:
#: **A tag carries its own length, so a second one begins where the first
#: ends.** Nothing that parses only the leading tag can see it: mutagen's
#: `ID3(path)` reads one and stops, so a stacked file reported as "alone". On
#: the staging tree 39% of mp3s are stacked, and the two tags can name
#: *different* MusicBrainz recordings — so which identity a file has depends
#: on which tag a reader happens to prefer.
ID3_MAGIC: Final = b"ID3"
ID3_HEADER_LEN: Final = 10
ID3_FOOTER_FLAG: Final = 0x10
#: An ID3v2 header this tool can size. A v2.5 would frame its length some other
#: way, and guessing at one is how a strip cuts into audio.
ID3_MAX_MAJOR: Final = 4
#: A stop, so a file of nonsense cannot spin here.
ID3_MAX_CHAIN: Final = 8
#: One tag is the ordinary case; this many means something is stacked behind it.
ID3_STACKED: Final = 2


def ape_of(path: Path) -> APEv2 | None:
    """The APEv2 tag, or None. A malformed one counts as absent.

    Absent **for reading**, which is right — a tag that will not parse says
    nothing. It is not absent from the file, and `ape_span` is what sees it.
    """
    try:
        return APEv2(path)
    except (APENoHeaderError, MutagenError, OSError, ValueError):
        return None


def ape_span(path: Path) -> tuple[int, int] | None:
    """Where the APEv2 tag sits in the file, found by position — issue #144.

    **The one that reaches a malformed tag.** `ape_of` refuses those, so they
    were passed over as already clean: of the 64 APE blocks left in this
    library after a full pass, *every one* was unparseable. They are also
    precisely the files this whole module exists for — a single bad item makes
    `lofty` abandon the file, audio and all (gmuse #53).

    The footer is 32 bytes and carries its own length, so the block can be cut
    off without understanding a byte of its contents. It sits either at the
    very end or just before a 128-byte ID3v1 block, which is the order mp3gain
    writes.
    """
    size = path.stat().st_size
    with path.open("rb") as handle:
        handle.seek(max(0, size - TAIL_SCAN))
        base = max(0, size - TAIL_SCAN)
        tail = handle.read()

    at = -1
    while (at := tail.find(APE_MAGIC, at + 1)) >= 0:
        block = tail[at : at + FOOTER_LEN]
        if len(block) < FOOTER_LEN:
            break
        version = int.from_bytes(block[8:12], "little")
        length = int.from_bytes(block[12:16], "little")
        flags = int.from_bytes(block[20:24], "little")
        if version not in APE_VERSIONS or not 0 < length < size:
            continue
        has_header = bool(flags & (1 << 31))
        is_header = bool(flags & (1 << 29))
        # A header sits before its items; a footer after them. Either one gives
        # the whole span, which is what a strip needs.
        start = base + at if is_header else base + at + FOOTER_LEN - length
        end = start + length + (FOOTER_LEN if has_header else 0)
        if 0 <= start < end <= size:
            return start, end
        # **A tag that runs past the end of the file** — one here, whose header
        # claims 420 bytes with 98 present, holding two mp3gain keys. It was
        # cut off by something. Nothing can follow a tag that never ends, so
        # everything from the header to EOF is the tag, and truncating there is
        # the only reading of it.
        if is_header and 0 <= start < size < end:
            return start, size
    return None


def _synchsafe(four: bytes) -> int:
    """An ID3v2 size: four bytes, seven bits each."""
    return (four[0] << 21) | (four[1] << 14) | (four[2] << 7) | four[3]


def id3_chain(path: Path) -> list[tuple[tuple[int, int], int, int]]:
    """Every ID3v2 tag stacked at the head of the file — #303.

    Returns `(version, start, span)` per tag, in file order. One entry is the
    ordinary case; more than one is the thing this exists to find.

    **By position, like `ape_span`**, and for the neighbouring reason: mutagen
    reads the leading tag and stops, so everything behind it was invisible to
    the check that then called the file "alone".
    """
    found: list[tuple[tuple[int, int], int, int]] = []
    size = path.stat().st_size
    try:
        with path.open("rb") as handle:
            off = 0
            while off + ID3_HEADER_LEN <= size and len(found) < ID3_MAX_CHAIN:
                handle.seek(off)
                head = handle.read(ID3_HEADER_LEN)
                if len(head) < ID3_HEADER_LEN or head[:3] != ID3_MAGIC:
                    break
                major, minor, flags = head[3], head[4], head[5]
                if major > ID3_MAX_MAJOR:
                    break
                span = ID3_HEADER_LEN + _synchsafe(head[6:10])
                if flags & ID3_FOOTER_FLAG:
                    span += ID3_HEADER_LEN
                # A tag that claims to end past EOF frames nothing, and a
                # zero-length one would not advance: either way, stop rather
                # than compute an offset into the audio.
                if span <= ID3_HEADER_LEN or off + span > size:
                    break
                found.append(((major, minor), off, span))
                off += span
    except OSError:
        return []
    return found


def extra_id3_span(path: Path) -> tuple[int, int] | None:
    """Where everything after the *first* ID3v2 tag sits, or None — #303."""
    chain = id3_chain(path)
    if len(chain) < ID3_STACKED:
        return None
    start = chain[1][1]
    _, last_start, last_span = chain[-1]
    return start, last_start + last_span


def _identity_of(blob: bytes) -> dict[str, str]:
    """The fields in one tag that say what the file *is*."""
    try:
        tag = ID3(io.BytesIO(blob))
    except (MutagenError, ID3NoHeaderError, OSError, ValueError):
        return {}
    said: dict[str, str] = {}
    for frame in tag.getall("TXXX"):
        if frame.desc.lower().startswith("musicbrainz"):
            said[frame.desc] = str(frame)
    for frame in tag.getall("UFID"):
        said[f"UFID:{frame.owner}"] = frame.data.decode("ascii", "replace")
    return said


#: The frames that say *which recording this is*. A disagreement here decides
#: what `shelf import` mints, so it is the one that stops the pass (#303).
#: Everything else a tag holds is content, and this module does not own
#: content — `ingestion-flow.md` §4.
def is_identity(key: str) -> bool:
    """Whether a frame name is one that settles identity."""
    return key.startswith("UFID:") or key.upper().startswith("TXXX:MUSICBRAINZ")


#: How many frame names a review cell lists before it says "and N more". The
#: point of the cell is to let a person judge, and a hundred names does not.
KEYS_SHOWN: Final = 8


def key_label(keys: Iterable[str]) -> str:
    """Frame names fit to sit in a TSV cell.

    **A frame name can hold raw bytes.** `PRIV:WM/WMCollectionID` carries a
    binary owner, tabs and newlines included, and writing one into the review
    file ended the survey with `holds a tab or newline` — the column broke the
    format it was describing the file in.
    """
    clean = sorted({re.sub(r"[^\x20-\x7e]+", "?", k)[:60] for k in keys})
    if len(clean) > KEYS_SHOWN:
        return "; ".join(clean[:KEYS_SHOWN]) + f"; and {len(clean) - KEYS_SHOWN} more"
    return "; ".join(clean)


def _frames_of(blob: bytes) -> dict[str, object]:
    """Every frame in one tag, keyed the way ID3 itself keys them."""
    try:
        tag = ID3(io.BytesIO(blob))
    except (MutagenError, ID3NoHeaderError, OSError, ValueError):
        return {}
    return dict(tag.items())


class Stacked(NamedTuple):
    """What the tags behind the leading one say that it does not — #303."""

    #: Identity frames both tags carry, differently. No rule settles these.
    conflicts: list[str]
    #: Other frames both carry differently. The leading tag is kept and these
    #: are listed, because this module normalises format and not content.
    differs: list[str]
    #: Frames only the stacked tags hold. Carried across, never dropped.
    salvage: dict[str, object]
    #: Frames where the leading value is an ID3v1 stump of the stacked one —
    #: #315. The stacked value replaces it; see `is_stump_of`.
    restore: dict[str, object]


def is_stump_of(kept: object, behind: object) -> bool:
    """Whether the leading value is an ID3v1 cut of the one behind it — #315.

    **Not "the shorter is a prefix of the longer".** `Hurt` against
    `Hurt (Live)` is a prefix and a different recording, and preferring the
    longer there changes which version the file claims. What makes a cut is the
    **length**: 30 is the ID3v1 field and 29 the same cut once a reader strips
    the pad, which is `paths.STUMP_LENGTHS` and #302's definition. A 29 is a
    stump only when something continues it, and here the stacked value is that
    something.

    Text frames only, compared as their single value: a frame carrying a list
    is not the shape an ID3v1 round trip leaves.
    """
    a, b = getattr(kept, "text", None), getattr(behind, "text", None)
    if not (isinstance(a, list) and isinstance(b, list) and len(a) == len(b) == 1):
        return False
    short, full = str(a[0]), str(b[0])
    return len(short) in STUMP_LENGTHS and len(full) > len(short) and full.startswith(short)


def id3_split(path: Path) -> Stacked:
    """Sort the stacked tags into what blocks, what is noted, and what moves.

    Three outcomes, and the middle one was learned twice:

    - **conflicts** — two MusicBrainz ids for one file, as the Dido albums had.
      Which one the file means depends on who reads it, so a person decides.
    - **differs** — `1` against `1/10`, `1988` against `1988-04`. `paths.py`
      already paid for this one: *comparing spellings finds differences that
      are not there*. Every one of 93 stacked files on one album reported a
      `TRCK` conflict and not one of them was a disagreement, which buried the
      case that was.
    - **salvage** — frames only the stacked tags hold, including the art and
      the ids. Carried over before the cut, the way lyrics are carried out of
      an APE tag. The first cut of this fix dropped them, which on this
      material meant deleting the better record.

    And a fourth that is not a difference at all: **restore** — the leading
    value is an ID3v1 stump of the stacked one (#315). `Flushed From the
    Bathroom of Y` in front of `…of Your Heart`, three times in one day's
    imports. Keeping the leading tag there writes the stump into the one tag
    that survives and deletes the only whole copy with the stacked one.
    """
    chain = id3_chain(path)
    if len(chain) < ID3_STACKED:
        return Stacked([], [], {}, {})
    data = path.read_bytes()
    keeping = _frames_of(data[chain[0][1] : chain[0][1] + chain[0][2]])
    conflicts: list[str] = []
    differs: list[str] = []
    salvage: dict[str, object] = {}
    restore: dict[str, object] = {}
    for _, start, span in chain[1:]:
        for key, frame in _frames_of(data[start : start + span]).items():
            if key not in keeping:
                salvage.setdefault(key, frame)
            elif str(keeping[key]) != str(frame):
                if not is_identity(key) and is_stump_of(keeping[key], frame):
                    restore.setdefault(key, frame)
                    continue
                into = conflicts if is_identity(key) else differs
                if key not in into:
                    into.append(key)
    return Stacked(sorted(conflicts), sorted(differs), salvage, restore)


def id3_conflicts(path: Path) -> list[str]:
    """Identity frames the stacked tags disagree about. See `id3_split`."""
    return id3_split(path).conflicts


def strip_extra_id3_bytes(path: Path) -> bool:
    """Cut every ID3v2 tag after the first. True if anything was removed.

    **Verified the way `strip_ape_bytes` is**, and for the stronger reason:
    this cuts bytes out of the middle of a file rather than off its end, so a
    mis-sized tag would take audio with it. The MPEG duration must agree
    before and after, and no stacked tag may remain; otherwise the file goes
    back exactly as it was.
    """
    span = extra_id3_span(path)
    if span is None:
        return False
    start, end = span
    before = mp3_seconds(path)
    original = path.read_bytes()
    path.write_bytes(original[:start] + original[end:])
    if mp3_seconds(path) != before or extra_id3_span(path) is not None:
        path.write_bytes(original)
        why = f"refused: cutting the stacked ID3v2 tag changed {path.name}"
        raise ValueError(why)
    return True


def strip_ape_bytes(path: Path) -> bool:
    """Cut the APEv2 tag out by position. True if anything was removed.

    **Verified against what the removal is for.** The audio must be untouched,
    so the MPEG stream's duration is read before and after and must agree; if
    it does not, the file is put back exactly as it was. Truncating a file is
    destructive in a way rewriting a tag is not, and this is the one place in
    this module that does it.
    """
    span = ape_span(path)
    if span is None:
        return False
    start, end = span
    before = mp3_seconds(path)
    original = path.read_bytes()
    path.write_bytes(original[:start] + original[end:])
    if mp3_seconds(path) != before or ape_span(path) is not None:
        path.write_bytes(original)
        why = f"refused: cutting the APE tag changed {path.name}"
        raise ValueError(why)
    return True


def mp3_seconds(path: Path) -> int | None:
    """The audio's length in whole seconds, for checking nothing moved."""
    try:
        return int(MP3(path).info.length)
    except (MutagenError, OSError, ValueError):
        return None


def id3_of(path: Path) -> ID3 | None:
    """The ID3 tag, or None."""
    try:
        return ID3(path)
    except (ID3NoHeaderError, MutagenError, OSError, ValueError):
        return None


def has_id3v1(path: Path) -> bool:
    """Whether a 128-byte ID3v1 block sits at the very end of the file."""
    try:
        with path.open("rb") as handle:
            handle.seek(-128, 2)
            return handle.read(3) == b"TAG"
    except OSError:
        return False


def ape_lyrics(ape: APEv2 | None) -> str:
    """The lyrics an APE tag holds, if it holds any."""
    if ape is None:
        return ""
    for key in ape:
        if key.lower() == "lyrics":
            return str(ape[key]).strip()
    return ""


def has_uslt(id3: ID3 | None) -> bool:
    """Whether ID3 already carries unsynchronised lyrics."""
    return id3 is not None and any(k.startswith("USLT") for k in id3)


def plan(path: Path) -> dict[str, object] | None:
    """What this file needs, or None when it is already modern and alone.

    The `salvage` column is the one to read: it is the only thing here that
    moves data rather than removing a copy of it.
    """
    ape = ape_of(path)
    id3 = id3_of(path)
    version = ".".join(str(n) for n in id3.version) if id3 else ""
    lyrics = ape_lyrics(ape)
    salvage = bool(lyrics) and not has_uslt(id3)

    # **By position, not by parse** (#144). `ape_of` refuses a malformed tag,
    # so those files read as already clean and were passed over — and they are
    # the ones this module exists for.
    drop_ape = ape is not None or ape_span(path) is not None
    drop_v1 = has_id3v1(path)
    upgrade = id3 is not None and id3.version < MODERN
    # **"Alone" has to mean alone** (#303). It used to mean no APEv2 and no
    # ID3v1, and said nothing about a second ID3v2 tag stacked behind the
    # first — which mutagen never reads and 39% of the staging tree carries.
    stacked = max(len(id3_chain(path)) - 1, 0)
    split = id3_split(path) if stacked else Stacked([], [], {}, {})

    if not (drop_ape or drop_v1 or upgrade or stacked):
        return None

    extra = sorted(k for k in (ape or {}) if k.lower() not in GAIN_KEYS and k.lower() != "lyrics")
    unreadable = drop_ape and ape is None
    return {
        # **A conflict is not marked for applying.** Keeping the leading tag is
        # the right answer only when something authoritative wrote it; on an
        # untouched download the stacked one can be the better-sourced. Two
        # tags that agree are merely redundant and go through as usual.
        "apply": "" if split.conflicts else "y",
        "id3": version,
        "upgrade": "y" if upgrade else "",
        "drop_ape": "y" if drop_ape else "",
        "drop_id3v1": "y" if drop_v1 else "",
        "drop_extra_id3": str(stacked) if stacked else "",
        "carry_id3": key_label(split.salvage),
        "restore_id3": key_label(split.restore),
        "id3_differs": key_label(split.differs),
        "id3_conflict": key_label(split.conflicts),
        "salvage_lyrics": f"{len(lyrics)} chars" if salvage else "",
        "ape_other": "unreadable — stripped by position" if unreadable else "; ".join(extra),
        "path": str(path),
    }


def _by_path(row: dict[str, object]) -> str:
    """Sort key for the review file, so a re-run diffs cleanly."""
    return str(row["path"])


def survey(root: Path, out: Path) -> None:
    """Look at every file and write the plan. Changes nothing."""
    rows: list[dict[str, object]] = []
    tally: collections.Counter[str] = collections.Counter()
    for path in walk(root):
        row = plan(path)
        if row is None:
            tally["already modern and alone"] += 1
            continue
        rows.append(row)
        for column, line in TALLY_LINES.items():
            if row[column]:
                tally[line] += 1
    # **Written even when it is empty** — a survey that found nothing used to
    # leave the last one that found something standing, so a clean library went
    # on being described by a stale file listing work already done.
    rows.sort(key=_by_path)
    write_tsv(out, rows)
    for key, n in sorted(tally.items()):
        print(f"  {n:6d}  {key}")
    print(f"\n{len(rows)} file(s) -> {out}")


def merge_stacked(path: Path) -> tuple[bool, int]:
    """Carry what only the stacked tags hold, then cut them off — #303.

    Returns `(stripped, frames carried)`. The salvage is read before the cut
    because after it those tags are gone, and written after it because
    `id3.save` rewrites the leading tag and would move every offset the cut
    depends on.
    """
    split = id3_split(path)
    if not strip_extra_id3_bytes(path):
        return False, 0
    if split.salvage or split.restore:
        carried = id3_of(path) or ID3()
        # `add` replaces a frame with the same hash key — mutagen's strict
        # path — so a restored stump is overwritten rather than kept beside the
        # whole value (#315).
        for frame in [*split.salvage.values(), *split.restore.values()]:
            carried.add(frame)
        carried.save(path, v2_version=4, v1=0)
    return True, len(split.salvage) + len(split.restore)


def normalise(path: Path, *, write: bool) -> str:
    """Bring one file to ID3v2.4 alone. Returns what happened."""
    if not write:
        return "would normalise"

    # **Stacked tags go first** (#303), before anything is read to be written
    # back: `id3.save` rewrites the leading tag and treats whatever follows as
    # audio, so stripping afterwards would be measuring offsets against a file
    # that had already moved underneath them.
    #
    # **And what only they hold is carried across before they go**, the way
    # lyrics are carried out of an APE tag below. On this material the leading
    # tag is the poorer one — no MusicBrainz ids, no art — so a strip that did
    # not salvage would delete the better record.
    stripped, carried_n = merge_stacked(path)

    ape = ape_of(path)
    id3 = id3_of(path)
    lyrics = ape_lyrics(ape)
    moved = bool(lyrics) and not has_uslt(id3)

    # Lyrics first, and saved before the APE tag goes: a crash between the two
    # should lose the deletion, never the words.
    if moved:
        if id3 is None:
            id3 = ID3()
        id3.add(USLT(encoding=3, lang="eng", desc="", text=lyrics))

    if id3 is not None:
        # `v2_version=4` upgrades v2.3 in place; `v1=0` removes the 128-byte
        # block rather than rewriting it.
        id3.save(path, v2_version=4, v1=0)

    # **Every removal is named, not just the last one.** Returning early on
    # the APE branch hid the stacked merge on 9 of 11 files that had both:
    # the work happened and the line said something else, which is the kind
    # of report that makes a pass look like it did less than it did.
    did = []
    if stripped:
        did.append(f"stacked ID3v2 merged in ({carried_n} frame(s) carried)" if carried_n else "stacked ID3v2 stripped")
    if ape is not None:
        ape.delete(path)
    elif strip_ape_bytes(path):
        # A tag mutagen would not parse, cut off by its own length (#144).
        did.append("unreadable APE stripped by position")
    if moved:
        did.append("lyrics migrated")
    return ", ".join([*did, "normalised"])


def snapshot_row(path: Path) -> dict[str, object]:
    """Everything this pass is about to remove, as text.

    **Honest about its limits.** This records the APE tag's keys and values and
    the ID3 version, which is enough to put the *content* back. It is not a byte
    copy, so a restored file is equivalent rather than identical.

    The audio is never touched, and everything removed was checked to exist
    elsewhere first — so the thing a snapshot usually guards against cannot
    happen here. It is kept because §1's amendment says an edit must be
    undoable, and "the data was redundant" is a reason, not an exemption.
    """
    ape = ape_of(path)
    id3 = id3_of(path)
    chain = id3_chain(path)
    # **What the stacked tags claimed** (#303). Stripping one drops fields no
    # other tag holds, and the identity fields are the ones worth being able
    # to look up afterwards — the whole point of the strip is that they may
    # have disagreed.
    data = path.read_bytes() if len(chain) >= ID3_STACKED else b""
    stacked = [_identity_of(data[s : s + n]) for _, s, n in chain[1:]]
    return {
        "path": str(path),
        "id3_version": ".".join(str(n) for n in id3.version) if id3 else "",
        "had_id3v1": "y" if has_id3v1(path) else "",
        "ape": json.dumps({k: str(ape[k]) for k in ape}, ensure_ascii=False) if ape else "",
        "extra_id3": json.dumps(stacked, ensure_ascii=False) if stacked else "",
    }


def apply(review: Path, snap: Path, *, write: bool) -> None:
    """Normalise the rows marked `y`. Dry run unless `--write`."""
    if not review.exists():
        print(f"no {review.name} — run `survey` first")
        return
    with review.open(encoding="utf-8") as handle:
        rows = [r for r in csv.DictReader(handle, delimiter="\t") if r["apply"].strip().lower() in TRUTHY]

    if write and rows:
        write_tsv(snap, [snapshot_row(Path(r["path"])) for r in rows])
        print(f"snapshot of {len(rows)} file(s) -> {snap}")

    tally: collections.Counter[str] = collections.Counter()
    for row in rows:
        try:
            tally[normalise(Path(row["path"]), write=write)] += 1
        except (MutagenError, OSError) as err:
            tally[f"failed: {type(err).__name__}"] += 1
    for key, n in sorted(tally.items()):
        print(f"  {n:6d}  {key}")
    if not write:
        print("dry run — pass --write to actually edit")


def restore(snap: Path, *, write: bool) -> None:
    """Put the APE tags back from the snapshot."""
    if not snap.exists():
        print(f"no {snap.name}")
        return
    tally: collections.Counter[str] = collections.Counter()
    with snap.open(encoding="utf-8") as handle:
        for row in csv.DictReader(handle, delimiter="\t"):
            if not row["ape"]:
                tally["nothing to restore"] += 1
                continue
            if not write:
                tally["would restore an APE tag"] += 1
                continue
            path = Path(row["path"])
            try:
                ape = APEv2()
                for key, value in json.loads(row["ape"]).items():
                    ape[key] = value
                ape.save(path)
                tally["APE tag restored"] += 1
            except (MutagenError, OSError, ValueError):
                tally["failed"] += 1
    for key, n in sorted(tally.items()):
        print(f"  {n:6d}  {key}")
    if not write:
        print("dry run — pass --write to actually restore")


LIB = typer.Argument(Path("~/MusicLibrary"), help="library root")
WRITE = typer.Option(default=False, help="actually edit files; without this, a dry run")


def register(app: typer.Typer) -> None:
    """Attach the tag normaliser."""
    tags_app = typer.Typer(no_args_is_help=True, help=__doc__)
    app.add_typer(tags_app, name="tags")

    @tags_app.command("survey")
    def survey_cmd(root: Path = LIB) -> None:
        """Report what would be normalised. Writes nothing."""
        survey(root.expanduser(), out_path("tags-review.tsv"))

    @tags_app.command("apply")
    def apply_cmd(root: Path = LIB, *, write: bool = WRITE) -> None:
        """Normalise the rows marked `y`, taking a snapshot first."""
        _ = root
        apply(out_path("tags-review.tsv"), out_path("tags-snapshot.tsv"), write=write)

    @tags_app.command("restore")
    def restore_cmd(root: Path = LIB, *, write: bool = WRITE) -> None:
        """Put the tags back from the snapshot."""
        _ = root
        restore(out_path("tags-snapshot.tsv"), write=write)
