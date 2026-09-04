#!/usr/bin/env python3
"""Write the reviewed genres back onto the files. The only script here that writes.

**mutagen, editing in place.** An ffmpeg remux would rewrite the file and take
the ReplayGain frames with it, and rsgain is a slow pass to repeat.

Dry run unless given ``--write``, and running it twice changes nothing the
second time.
"""

from __future__ import annotations

import csv
from pathlib import Path

import typer
from mutagen import File as MutagenFile
from mutagen.id3 import TCON
from mutagen.mp4 import MP4Tags

from gmuse_tools.genre.common import album_key, out_path, tags_of, walk

TRUTHY = frozenset({"y", "yes", "1"})


def wanted_from(review: Path) -> dict[tuple[str, str], str]:
    """Read the review file, keeping only rows marked to apply.

    **An empty `proposed` on a row marked `y` means clear it**, and that is a
    deliberate reading rather than an oversight. The pipeline drops terms that
    are not genres — `Other`, and a value that merely repeats the artist — so
    an album tagged `Tom Waits` composes to nothing. It could diagnose that and
    not act on it: the row said "this is junk" and the apply skipped it for
    having nothing to write.

    An empty genre is the right answer when there is no good one. `y` is an
    explicit mark on one row, and the snapshot makes it reversible.
    """
    with review.open(encoding="utf-8") as handle:
        return {
            (row["artist"], row["album"]): row["proposed"].strip()
            for row in csv.DictReader(handle, delimiter="\t")
            if row["apply"].strip().lower() in TRUTHY
        }


def set_genre(path: Path, value: str) -> bool:
    """Set one file's genre in place. Returns False if mutagen cannot read it.

    Three tag formats, three spellings, and **MP4 has to be named explicitly**.
    An earlier version had two branches — ID3, and "everything else takes
    ``genre``" — which is true of Vorbis comments and false of MP4: its genre
    atom is ``©gen``, so ``audio["genre"]`` quietly created a *freeform* ``genr``
    atom beside the real one and left it untouched. The write reported success
    and changed nothing, which only the idempotence re-run caught.
    """
    audio = MutagenFile(path)
    if audio is None:
        return False
    tags = getattr(audio, "tags", None)
    # An empty value removes the field rather than writing an empty one: a
    # present-but-blank genre is a value that filters and format strings then
    # have to special-case, and it reads as "" rather than as absent.
    if isinstance(tags, MP4Tags):
        tags.pop("genr", None)  # the bogus atom the earlier version wrote
        if value:
            tags["\xa9gen"] = [value]
        else:
            tags.pop("\xa9gen", None)
    elif tags is not None and hasattr(tags, "setall"):
        if value:
            tags.setall("TCON", [TCON(encoding=3, text=[value])])  # ID3
        else:
            tags.delall("TCON")
    elif value:
        audio["genre"] = value  # Vorbis comments
    elif "genre" in audio:
        del audio["genre"]
    audio.save()
    return True


def apply_all(root: Path, *, write: bool) -> None:
    """Apply the review file, or show what it would do."""
    root = root.expanduser()
    wanted = wanted_from(out_path("genre-review.tsv"))
    if not wanted:
        print("nothing marked to apply")
        return

    changed = skipped = unreadable = 0
    for path in walk(root):
        tags = tags_of(path)
        target = wanted.get(album_key(tags))
        if target is None:
            continue
        if (tags.get("genre") or "").strip() == target:
            skipped += 1  # already right: idempotent by construction
            continue
        if not write:
            print(f"  would set {path.relative_to(root)}\n      {tags.get('genre', '')!r} -> {target!r}")
            changed += 1
        elif set_genre(path, target):
            changed += 1
        else:
            unreadable += 1

    verb = "changed" if write else "would change"
    print(f"\n{verb} {changed} files, {skipped} already correct, {unreadable} unreadable")
    if not write:
        print("dry run — pass --write to actually edit")
    else:
        print(
            "Now launch gmuse once before any reorganisation: it recognises a moved "
            "file by size, duration and tags, and a retag changes that signature."
        )


LIB = typer.Argument(Path("~/MusicLibrary"), help="library root")


def register(app: typer.Typer) -> None:
    """Attach the genre apply."""

    @app.command("apply")
    def apply_cmd(
        root: Path = LIB,
        *,
        write: bool = typer.Option(default=False, help="actually change the files"),
    ) -> None:
        """Write the reviewed genres. A dry run without `--write`."""
        apply_all(root, write=write)
