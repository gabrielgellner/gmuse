#!/usr/bin/env python3
"""Write the reviewed genres back onto the files. The only script here that writes.

**mutagen, editing in place.** An ffmpeg remux would rewrite the file and take
the ReplayGain frames with it, and rsgain is a slow pass to repeat.

Dry run unless given ``--write``, and running it twice changes nothing the
second time.
"""

from __future__ import annotations

import argparse
import csv
from pathlib import Path

from mutagen import File as MutagenFile
from mutagen.id3 import TCON

from genre.common import album_key, tags_of, walk

TRUTHY = frozenset({"y", "yes", "1"})


def wanted_from(review: Path) -> dict[tuple[str, str], str]:
    """Read the review file, keeping only rows marked to apply."""
    with review.open(encoding="utf-8") as handle:
        return {
            (row["artist"], row["album"]): row["proposed"].strip()
            for row in csv.DictReader(handle, delimiter="\t")
            if row["apply"].strip().lower() in TRUTHY and row["proposed"].strip()
        }


def set_genre(path: Path, value: str) -> bool:
    """Set one file's genre in place. Returns False if mutagen cannot read it."""
    audio = MutagenFile(path)
    if audio is None:
        return False
    tags = getattr(audio, "tags", None)
    if tags is not None and hasattr(tags, "setall"):
        tags.setall("TCON", [TCON(encoding=3, text=[value])])  # ID3
    else:
        audio["genre"] = value  # Vorbis comments, MP4 atoms
    audio.save()
    return True


def main() -> None:
    """Apply the review file, or show what it would do."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", nargs="?", default="~/MusicLibrary", help="library root to edit")
    parser.add_argument("--write", action="store_true", help="actually change the files")
    args = parser.parse_args()

    root = Path(args.root).expanduser()
    wanted = wanted_from(Path(__file__).parent / "review.tsv")
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
        if not args.write:
            print(f"  would set {path.relative_to(root)}\n      {tags.get('genre', '')!r} -> {target!r}")
            changed += 1
        elif set_genre(path, target):
            changed += 1
        else:
            unreadable += 1

    verb = "changed" if args.write else "would change"
    print(f"\n{verb} {changed} files, {skipped} already correct, {unreadable} unreadable")
    if not args.write:
        print("dry run — pass --write to actually edit")
    else:
        print(
            "Now launch gmuse once before any reorganisation: it recognises a moved "
            "file by size, duration and tags, and a retag changes that signature."
        )


if __name__ == "__main__":
    main()
