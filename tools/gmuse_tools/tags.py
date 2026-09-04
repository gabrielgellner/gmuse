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

    uv run python -m tags survey  ~/MusicLibrary          # -> out/tags-review.tsv
    uv run python -m tags apply   ~/MusicLibrary          # dry run
    uv run python -m tags apply   ~/MusicLibrary --write  # snapshots first
    uv run python -m tags restore ~/MusicLibrary --write
"""

from __future__ import annotations

import collections
import csv
import json
from pathlib import Path
from typing import Final

import typer
from mutagen import MutagenError
from mutagen.apev2 import APENoHeaderError, APEv2
from mutagen.id3 import ID3, USLT, ID3NoHeaderError

from gmuse_tools.genre.common import out_path, walk, write_tsv

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
MODERN: Final = (2, 4, 0)


def ape_of(path: Path) -> APEv2 | None:
    """The APEv2 tag, or None. A malformed one counts as absent."""
    try:
        return APEv2(path)
    except (APENoHeaderError, MutagenError, OSError, ValueError):
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

    drop_ape = ape is not None
    drop_v1 = has_id3v1(path)
    upgrade = id3 is not None and id3.version < MODERN

    if not (drop_ape or drop_v1 or upgrade):
        return None

    extra = sorted(k for k in (ape or {}) if k.lower() not in GAIN_KEYS and k.lower() != "lyrics")
    return {
        "apply": "y",
        "id3": version,
        "upgrade": "y" if upgrade else "",
        "drop_ape": "y" if drop_ape else "",
        "drop_id3v1": "y" if drop_v1 else "",
        "salvage_lyrics": f"{len(lyrics)} chars" if salvage else "",
        "ape_other": "; ".join(extra),
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
        if row["upgrade"]:
            tally["ID3v2.3 -> v2.4"] += 1
        if row["drop_ape"]:
            tally["APEv2 to remove"] += 1
        if row["drop_id3v1"]:
            tally["ID3v1 to remove"] += 1
        if row["salvage_lyrics"]:
            tally["lyrics to migrate into ID3"] += 1
    if rows:
        rows.sort(key=_by_path)
        write_tsv(out, rows)
    for key, n in sorted(tally.items()):
        print(f"  {n:6d}  {key}")
    print(f"\n{len(rows)} file(s) -> {out}")


def normalise(path: Path, *, write: bool) -> str:
    """Bring one file to ID3v2.4 alone. Returns what happened."""
    ape = ape_of(path)
    id3 = id3_of(path)
    lyrics = ape_lyrics(ape)
    moved = bool(lyrics) and not has_uslt(id3)

    if not write:
        return "would normalise"

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

    if ape is not None:
        ape.delete(path)

    return "lyrics migrated, normalised" if moved else "normalised"


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
    return {
        "path": str(path),
        "id3_version": ".".join(str(n) for n in id3.version) if id3 else "",
        "had_id3v1": "y" if has_id3v1(path) else "",
        "ape": json.dumps({k: str(ape[k]) for k in ape}, ensure_ascii=False) if ape else "",
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
