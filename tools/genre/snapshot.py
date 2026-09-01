#!/usr/bin/env python3
"""Record every file's current genre, so an apply can be undone.

`apply.py` edits tags in place and there is no other record of what was there.
This is the record: one row per file, written before the change and restorable
with ``--restore``.
"""

from __future__ import annotations

import argparse
import csv
from pathlib import Path

from genre.apply import set_genre
from genre.common import tags_of, walk


def take(root: Path, path: Path) -> None:
    """Write one row per file: its path relative to `root`, and its genre."""
    with path.open("w", newline="", encoding="utf-8") as handle:
        writer = csv.DictWriter(handle, fieldnames=["path", "genre"], delimiter="\t")
        writer.writeheader()
        writer.writerows({"path": str(p.relative_to(root)), "genre": tags_of(p).get("genre") or ""} for p in walk(root))
    print(f"snapshot written to {path}")


def restore(root: Path, path: Path, *, write: bool) -> None:
    """Put the snapshot's genres back, skipping anything already matching."""
    restored = 0
    with path.open(encoding="utf-8") as handle:
        for row in csv.DictReader(handle, delimiter="\t"):
            target = root / row["path"]
            if not target.exists() or (tags_of(target).get("genre") or "") == row["genre"]:
                continue
            if write:
                set_genre(target, row["genre"])
            restored += 1
    verb = "restored" if write else "would restore"
    print(f"{verb} {restored} files from {path}")
    if not write:
        print("dry run — pass --write to actually restore")


def main() -> None:
    """Write a snapshot of the library's genres, or restore one."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", nargs="?", default="~/MusicLibrary", help="library root")
    parser.add_argument("--out", default="genre-snapshot.tsv", help="snapshot file, beside this script")
    parser.add_argument("--restore", action="store_true", help="put the snapshot's genres back")
    parser.add_argument("--write", action="store_true", help="with --restore, actually change the files")
    args = parser.parse_args()

    root = Path(args.root).expanduser()
    path = Path(__file__).parent / args.out
    if args.restore:
        restore(root, path, write=args.write)
    else:
        take(root, path)


if __name__ == "__main__":
    main()
