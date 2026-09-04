#!/usr/bin/env python3
"""Record every file's current genre, so an apply can be undone.

`apply.py` edits tags in place and there is no other record of what was there.
This is the record: one row per file, written before the change and restorable
with ``--restore``.
"""

from __future__ import annotations

import csv
from pathlib import Path

import typer

from gmuse_tools.genre.apply import set_genre
from gmuse_tools.genre.common import out_path, tags_of, walk


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


def snapshot_all(root: Path, out: str, *, restore_it: bool, write: bool) -> None:
    """Write a snapshot of the library's genres, or restore one."""
    root = root.expanduser()
    path = out_path(out)
    if restore_it:
        restore(root, path, write=write)
    else:
        take(root, path)


LIB = typer.Argument(Path("~/MusicLibrary"), help="library root")


def register(app: typer.Typer) -> None:
    """Attach the genre snapshot."""

    @app.command("snapshot")
    def snapshot_cmd(
        root: Path = LIB,
        out: str = typer.Option("genre-snapshot.tsv", help="snapshot file, inside tools/out/"),
        *,
        restore: bool = typer.Option(default=False, help="put the snapshot's genres back"),
        write: bool = typer.Option(default=False, help="with --restore, actually change files"),
    ) -> None:
        """Every album's genres before a change, so an apply can be undone."""
        snapshot_all(root, out, restore_it=restore, write=write)
